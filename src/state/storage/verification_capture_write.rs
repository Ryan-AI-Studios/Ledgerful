use super::verification_capture::{
    CanonicalResultBinding, CaptureUnavailable as Unavailable, TransactionStage,
};
#[cfg(target_os = "linux")]
use crate::change_evidence::pack_store::capture_db::NativeCaptureRecheck;
use crate::commands::verify::AttemptPayload;
use crate::state::storage::StorageManager;
use crate::state::storage::change_evidence::{
    resolve_or_provision_write_tx, resolve_qualified_write_tx,
};
use crate::verify::engine::MAX_CAPTURE_POSITIONS;
use chrono::{DateTime, Datelike, SecondsFormat, Utc};
use rusqlite::{Connection, Savepoint, Transaction, TransactionBehavior, params};
use uuid::Uuid;

/// The future head-only proof must build this from the same attempt intent and
/// normal execution. Storage validates its shape and rechecks its scoped owner.
pub(crate) struct AttemptIdentity {
    pub(crate) repo_scope: Option<Uuid>,
    pub(crate) authorize_first_scope: bool,
    pub(crate) scope_guard: [u8; 32],
    pub(crate) attempt_id: Uuid,
    pub(crate) head: String,
    pub(crate) observed_from: DateTime<Utc>,
    pub(crate) observed_to: DateTime<Utc>,
}

pub(crate) struct PreparedExecutedAttempt {
    identity: AttemptIdentity,
    payload: AttemptPayload,
}

impl PreparedExecutedAttempt {
    pub(crate) fn new(
        identity: AttemptIdentity,
        payload: AttemptPayload,
    ) -> Result<Self, Unavailable> {
        validate_identity(&identity)?;
        if payload.kind() != "executed" {
            return Err(Unavailable::InvalidOrigin);
        }
        Ok(Self { identity, payload })
    }

    pub(crate) fn identity(&self) -> &AttemptIdentity {
        &self.identity
    }
}

fn validate_identity(identity: &AttemptIdentity) -> Result<(), Unavailable> {
    if identity.repo_scope.is_some() == identity.authorize_first_scope
        || identity.repo_scope.is_some_and(|scope| scope.is_nil())
        || identity.attempt_id.get_version_num() != 4
        || identity.attempt_id.get_variant() != uuid::Variant::RFC4122
        || identity.head.len() != 40
        || !identity
            .head
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        || !(0..=9999).contains(&identity.observed_from.year())
        || !(0..=9999).contains(&identity.observed_to.year())
        || identity.observed_to < identity.observed_from
    {
        return Err(Unavailable::InvalidQualification);
    }
    Ok(())
}

/// Only this module can create a receipt. The transaction owner publishes it
/// after successful outer COMMIT.
pub(super) struct InsertedAttempt {
    attempt_id: Uuid,
    confirmed_samples: u8,
}

impl InsertedAttempt {
    pub(super) fn attempt_id(&self) -> Uuid {
        self.attempt_id
    }
    pub(super) fn confirmed_samples(&self) -> u8 {
        self.confirmed_samples
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlanningAttemptKind {
    DryRun,
    Refused,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlanningCaptureCommit {
    Recorded {
        attempt_id: Uuid,
        kind: PlanningAttemptKind,
    },
    Unavailable(Unavailable),
    Unconfirmed(TransactionStage),
}

impl StorageManager {
    pub(crate) fn persist_planning_attempt(
        &self,
        identity: AttemptIdentity,
        payload: AttemptPayload,
        #[cfg(target_os = "linux")] native: &mut NativeCaptureRecheck<'_>,
    ) -> PlanningCaptureCommit {
        if self.is_read_only {
            return PlanningCaptureCommit::Unavailable(Unavailable::StorageUnavailable);
        }
        match persist_planning_attempt(
            &self.conn,
            &identity,
            &payload,
            #[cfg(target_os = "linux")]
            native,
        ) {
            Ok(committed) => committed,
            Err(reason) => PlanningCaptureCommit::Unavailable(reason),
        }
    }
}

fn persist_planning_attempt(
    conn: &Connection,
    identity: &AttemptIdentity,
    payload: &AttemptPayload,
    #[cfg(target_os = "linux")] native: &mut NativeCaptureRecheck<'_>,
) -> Result<PlanningCaptureCommit, Unavailable> {
    validate_identity(identity)?;
    let kind = match (payload.kind(), payload.positions().is_empty()) {
        ("dry-run", true) => PlanningAttemptKind::DryRun,
        ("refused", true) => PlanningAttemptKind::Refused,
        _ => return Err(Unavailable::InvalidOrigin),
    };
    let Ok(tx) = Transaction::new_unchecked(conn, TransactionBehavior::Immediate) else {
        return Ok(PlanningCaptureCommit::Unconfirmed(TransactionStage::Begin));
    };
    #[cfg(target_os = "linux")]
    if !native.admit() {
        return if tx.rollback().is_ok() {
            Err(Unavailable::ScopeUnavailable)
        } else {
            Ok(PlanningCaptureCommit::Unconfirmed(
                TransactionStage::Rollback,
            ))
        };
    }
    let written = (|| {
        let header = AttemptHeader::new(&tx, identity, payload, None, 0)?;
        header.insert(&tx)?;
        header.confirm(&tx)?;
        let has_samples = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM change_evidence_samples
              WHERE repo_scope=?1 AND attempt_id=?2 LIMIT 1)",
                params![&header.scope_key, &header.attempt_key],
                |row| row.get::<_, bool>(0),
            )
            .map_err(|_| Unavailable::AttemptWriteFailed)?;
        if has_samples {
            return Err(Unavailable::AttemptWriteFailed);
        }
        Ok(())
    })();
    if let Err(reason) = written {
        return if tx.rollback().is_ok() {
            Err(reason)
        } else {
            Ok(PlanningCaptureCommit::Unconfirmed(
                TransactionStage::Rollback,
            ))
        };
    }
    #[cfg(target_os = "linux")]
    if !native.check() {
        return if tx.rollback().is_ok() {
            Err(Unavailable::ScopeUnavailable)
        } else {
            Ok(PlanningCaptureCommit::Unconfirmed(
                TransactionStage::Rollback,
            ))
        };
    }
    if tx.commit().is_err() {
        return Ok(PlanningCaptureCommit::Unconfirmed(TransactionStage::Commit));
    }
    Ok(PlanningCaptureCommit::Recorded {
        attempt_id: identity.attempt_id,
        kind,
    })
}

struct AttemptHeader<'a> {
    identity: &'a AttemptIdentity,
    payload: &'a AttemptPayload,
    scope_key: String,
    attempt_key: String,
    observed_from: String,
    observed_to: String,
    run_id: Option<i64>,
    sample_count: u8,
}

impl<'a> AttemptHeader<'a> {
    fn new(
        conn: &Connection,
        identity: &'a AttemptIdentity,
        payload: &'a AttemptPayload,
        run_id: Option<i64>,
        sample_count: u8,
    ) -> Result<Self, Unavailable> {
        let observed_from = identity
            .observed_from
            .to_rfc3339_opts(SecondsFormat::Nanos, true);
        let observed_to = identity
            .observed_to
            .to_rfc3339_opts(SecondsFormat::Nanos, true);
        let scope = match identity.repo_scope {
            Some(original) => resolve_qualified_write_tx(conn, &identity.scope_guard)
                .ok()
                .filter(|scope| *scope == original),
            None => resolve_or_provision_write_tx(conn, &identity.scope_guard, &observed_from).ok(),
        }
        .ok_or(Unavailable::ScopeUnavailable)?;
        let scope_key = scope.hyphenated().to_string();
        let count: i64 = conn
            .query_row(
                "SELECT count(*) FROM (SELECT 1 FROM change_evidence_attempts
             INDEXED BY idx_change_evidence_attempt_scope_head WHERE repo_scope=?1 LIMIT 256)",
                [&scope_key],
                |row| row.get(0),
            )
            .map_err(|_| Unavailable::AttemptWriteFailed)?;
        if count >= 256 {
            return Err(Unavailable::CapacityReached);
        }
        Ok(Self {
            identity,
            payload,
            scope_key,
            attempt_key: identity.attempt_id.hyphenated().to_string(),
            observed_from,
            observed_to,
            run_id,
            sample_count,
        })
    }

    fn insert(&self, conn: &Connection) -> Result<(), Unavailable> {
        let affected = conn
            .execute(
                "INSERT INTO change_evidence_attempts
             (repo_scope,attempt_id,head,attempt_kind,originally_bound,run_id,sample_count,
              payload_version,payload_json,payload_digest,observed_from,observed_to)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
                self.parameters(),
            )
            .map_err(|_| Unavailable::AttemptWriteFailed)?;
        if affected != 1 {
            return Err(Unavailable::AttemptWriteFailed);
        }
        Ok(())
    }

    fn confirm(&self, conn: &Connection) -> Result<(), Unavailable> {
        let matches = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM change_evidence_attempts
              WHERE repo_scope=?1 AND attempt_id=?2 AND head=?3 AND attempt_kind=?4
                AND originally_bound=?5 AND run_id IS ?6 AND sample_count=?7
                AND payload_version=?8 AND payload_json=?9 AND payload_digest=?10
                AND observed_from=?11 AND observed_to=?12)",
                self.parameters(),
                |row| row.get::<_, bool>(0),
            )
            .map_err(|_| Unavailable::AttemptWriteFailed)?;
        if !matches {
            return Err(Unavailable::AttemptWriteFailed);
        }
        Ok(())
    }

    fn parameters(&self) -> impl rusqlite::Params {
        (
            &self.scope_key,
            &self.attempt_key,
            &self.identity.head,
            self.payload.kind(),
            self.run_id.is_some(),
            self.run_id,
            self.sample_count,
            self.payload.version(),
            self.payload.json(),
            self.payload.digest(),
            &self.observed_from,
            &self.observed_to,
        )
    }
}

pub(super) fn write_executed_attempt(
    savepoint: &Savepoint<'_>,
    attempt: PreparedExecutedAttempt,
    run_id: i64,
    samples: &[CanonicalResultBinding],
) -> Result<InsertedAttempt, Unavailable> {
    let PreparedExecutedAttempt { identity, payload } = attempt;
    let expected_positions = payload.positions();
    if samples.len() > MAX_CAPTURE_POSITIONS
        || expected_positions.len() != samples.len()
        || samples
            .iter()
            .zip(expected_positions)
            .enumerate()
            .any(|(index, (sample, expected))| {
                sample.position != index
                    || u64::try_from(sample.position).ok() != Some(*expected)
                    || sample.result_id <= 0
            })
    {
        return Err(Unavailable::InvalidOrigin);
    }
    let confirmed_samples = u8::try_from(samples.len()).map_err(|_| Unavailable::InvalidOrigin)?;
    let header = AttemptHeader::new(
        savepoint,
        &identity,
        &payload,
        Some(run_id),
        confirmed_samples,
    )?;
    header.insert(savepoint)?;

    let mut insert = savepoint
        .prepare(
            "INSERT INTO change_evidence_samples(repo_scope,attempt_id,position,result_id)
         SELECT ?1,?2,?3,id FROM verification_results WHERE id=?4 AND run_id=?5",
        )
        .map_err(|_| Unavailable::AttemptWriteFailed)?;
    for sample in samples {
        let position = i64::try_from(sample.position).map_err(|_| Unavailable::InvalidOrigin)?;
        if insert
            .execute(params![
                &header.scope_key,
                &header.attempt_key,
                position,
                sample.result_id,
                run_id
            ])
            .map_err(|_| Unavailable::AttemptWriteFailed)?
            != 1
        {
            return Err(Unavailable::AttemptWriteFailed);
        }
    }
    header.confirm(savepoint)?;
    let mut stored = savepoint
        .prepare(
            "SELECT s.position,s.result_id,r.run_id FROM change_evidence_samples s
         LEFT JOIN verification_results r ON r.id=s.result_id
         WHERE s.repo_scope=?1 AND s.attempt_id=?2 ORDER BY s.position LIMIT 33",
        )
        .map_err(|_| Unavailable::AttemptWriteFailed)?;
    let mut rows = stored
        .query(params![&header.scope_key, &header.attempt_key])
        .map_err(|_| Unavailable::AttemptWriteFailed)?;
    for sample in samples {
        let row = rows
            .next()
            .map_err(|_| Unavailable::AttemptWriteFailed)?
            .ok_or(Unavailable::AttemptWriteFailed)?;
        let position = i64::try_from(sample.position).map_err(|_| Unavailable::InvalidOrigin)?;
        let read: rusqlite::Result<(i64, i64, i64)> =
            (|| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))();
        if read.map_err(|_| Unavailable::AttemptWriteFailed)?
            != (position, sample.result_id, run_id)
        {
            return Err(Unavailable::AttemptWriteFailed);
        }
    }
    if rows
        .next()
        .map_err(|_| Unavailable::AttemptWriteFailed)?
        .is_some()
    {
        return Err(Unavailable::AttemptWriteFailed);
    }
    Ok(InsertedAttempt {
        attempt_id: identity.attempt_id,
        confirmed_samples,
    })
}
