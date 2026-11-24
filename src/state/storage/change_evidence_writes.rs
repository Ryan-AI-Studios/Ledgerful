//! Guarded publication for opt-in baseline producers. Callers own Git proof and
//! the native database lease. This module performs no filesystem or graph work.

use super::{
    EvidenceStorageError, ExactPreparedKey, integer, require_foreign_keys,
    resolve_optional_qualified_tx, resolve_or_provision_write_tx, sql_error, text,
};
use crate::commands::baseline::{
    BaselineError, canonical_set_json, decode_verified_baseline_row,
    extract::capture_snapshot::{CaptureSnapshotError, IndexGeneration, read_generation},
    identity::FileEdgeSet,
};
use crate::state::storage::baselines::{BaselineRow, validate_baseline_name};
use chrono::{DateTime, Utc};
use rusqlite::{Connection, TransactionBehavior, params};
use serde::Serialize;
use uuid::Uuid;

#[path = "change_evidence_prepared.rs"]
pub(crate) mod prepared;

const BASELINE_BODY_CAP: i64 = 8 * 1024 * 1024;
const IDENTITY_CAP: i64 = 16 * 1024;
const PREPARED_CAP: usize = 256 * 1024;
const VERSION: i64 = 1;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum WriteError {
    Storage(EvidenceStorageError),
    ScopeUnavailable,
    Corrupt,
    InvalidInput,
    PayloadOverBudget,
    WriteConflict,
    Capacity,
    BaselineMissing,
    BaselineChanged,
    BaselineInvalid,
    IndexChanged,
    RevisionChanged,
    AttestationMissing,
}
type E = WriteError;
type Result<T> = std::result::Result<T, E>;

impl From<EvidenceStorageError> for WriteError {
    fn from(error: EvidenceStorageError) -> Self {
        match error {
            EvidenceStorageError::ScopeUnavailable | EvidenceStorageError::ForeignScope => {
                Self::ScopeUnavailable
            }
            EvidenceStorageError::Corrupt => Self::Corrupt,
            EvidenceStorageError::InvalidInput => Self::InvalidInput,
            EvidenceStorageError::PayloadOverBudget => Self::PayloadOverBudget,
            other => Self::Storage(other),
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) enum ScopeExpectation {
    Existing(Uuid),
    AuthorizeFirst,
}

#[derive(Clone, Copy)]
pub(crate) struct WriteScope<'a> {
    pub guard: &'a [u8; 32],
    pub expected: ScopeExpectation,
}

impl WriteScope<'_> {
    fn verify(self, conn: &Connection) -> Result<Option<Uuid>> {
        let current = resolve_optional_qualified_tx(conn, self.guard)?;
        match (self.expected, current) {
            (ScopeExpectation::Existing(expected), Some(actual)) if expected == actual => {
                Ok(Some(actual))
            }
            (ScopeExpectation::AuthorizeFirst, None) => Ok(None),
            _ => Err(E::ScopeUnavailable),
        }
    }

    fn provision(self, conn: &Connection, current: Option<Uuid>, time: &str) -> Result<Uuid> {
        match current {
            Some(id) => Ok(id),
            None => Ok(resolve_or_provision_write_tx(conn, self.guard, time)?),
        }
    }
}

fn bounded_timestamp(value: &str) -> Result<DateTime<Utc>> {
    if !(1..=64).contains(&value.len()) {
        return Err(E::Corrupt);
    }
    DateTime::parse_from_rfc3339(value)
        .map(|time| time.with_timezone(&Utc))
        .map_err(|_| E::Corrupt)
}

fn content_digest(body: &str) -> String {
    format!("blake3:{}", blake3::hash(body.as_bytes()).to_hex())
}

/// Read and strictly verify one named row in the already-open exact scope
/// snapshot before starting any repository observation.
pub(crate) fn read_baseline(
    conn: &Connection,
    guard: &[u8; 32],
    name: &str,
) -> Result<VerifiedBaseline> {
    resolve_optional_qualified_tx(conn, guard)?;
    validate_baseline_name(name).map_err(|_| E::InvalidInput)?;
    let mut statement = conn.prepare(
        "SELECT baseline_schema,payload_kind,identity_json,payload_json,payload_digest,created_at
         FROM analysis_baselines WHERE name COLLATE BINARY=?1",
    ).map_err(sql_error)?;
    // Check byte sizes before accessing body columns from Rust.
    ensure_baseline_shape(conn, name)?;
    let mut rows = statement.query([name]).map_err(sql_error)?;
    let raw = rows.next().map_err(sql_error)?.ok_or(E::BaselineMissing)?;
    let row = BaselineRow {
        name: name.to_owned(),
        baseline_schema: u32::try_from(integer(raw, 0)?).map_err(|_| E::BaselineInvalid)?,
        payload_kind: text(raw, 1)?.to_owned(),
        identity_json: text(raw, 2)?.to_owned(),
        payload_json: text(raw, 3)?.to_owned(),
        payload_digest: text(raw, 4)?.to_owned(),
        created_at: text(raw, 5)?.to_owned(),
    };
    if rows.next().map_err(sql_error)?.is_some() {
        return Err(E::Corrupt);
    }
    bounded_timestamp(&row.created_at).map_err(|_| E::BaselineInvalid)?;
    let set = decode_verified_baseline_row(&row).map_err(|error| {
        match error.downcast_ref::<BaselineError>() {
            Some(BaselineError::DigestMismatch { .. }) => E::BaselineChanged,
            _ => E::BaselineInvalid,
        }
    })?;
    let (identity, body) = canonical_set_json(&set).map_err(|_| E::BaselineInvalid)?;
    if identity != row.identity_json || body != row.payload_json {
        return Err(E::BaselineInvalid);
    }
    Ok(VerifiedBaseline { row, set })
}

fn ensure_baseline_shape(conn: &Connection, name: &str) -> Result<()> {
    let mut statement = conn
        .prepare(
            "SELECT typeof(baseline_schema)='integer'
             AND typeof(payload_kind)='text' AND octet_length(payload_kind) BETWEEN 1 AND 64
             AND typeof(identity_json)='text' AND octet_length(identity_json) BETWEEN 1 AND ?2
             AND typeof(payload_json)='text' AND octet_length(payload_json) BETWEEN 1 AND ?3
             AND typeof(payload_digest)='text' AND octet_length(payload_digest)=71
             AND typeof(created_at)='text' AND octet_length(created_at) BETWEEN 1 AND 64
         FROM analysis_baselines WHERE name COLLATE BINARY=?1",
        )
        .map_err(sql_error)?;
    let mut rows = statement
        .query(params![name, IDENTITY_CAP, BASELINE_BODY_CAP])
        .map_err(sql_error)?;
    let row = rows.next().map_err(sql_error)?.ok_or(E::BaselineMissing)?;
    if integer(row, 0)? != 1 || rows.next().map_err(sql_error)?.is_some() {
        return Err(E::BaselineInvalid);
    }
    Ok(())
}

pub(crate) struct VerifiedBaseline {
    row: BaselineRow,
    set: FileEdgeSet,
}

impl VerifiedBaseline {
    pub(crate) fn set(&self) -> &FileEdgeSet {
        &self.set
    }

    pub(crate) fn digest(&self) -> &str {
        &self.row.payload_digest
    }

    fn confirm(&self, conn: &Connection) -> Result<()> {
        let row = &self.row;
        ensure_baseline_shape(conn, &row.name)?;
        let matches: i64 = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM analysis_baselines WHERE name COLLATE BINARY=?1
                 AND baseline_schema=?2 AND payload_kind COLLATE BINARY=?3
                 AND identity_json COLLATE BINARY=?4 AND payload_json COLLATE BINARY=?5
                 AND payload_digest COLLATE BINARY=?6 AND created_at COLLATE BINARY=?7)",
                params![
                    row.name,
                    row.baseline_schema,
                    row.payload_kind,
                    row.identity_json,
                    row.payload_json,
                    row.payload_digest,
                    row.created_at
                ],
                |r| r.get(0),
            )
            .map_err(sql_error)?;
        if matches != 1 {
            return Err(E::BaselineChanged);
        }
        Ok(())
    }

    fn require_attestable(&self, key: &ExactPreparedKey<'_>) -> Result<()> {
        let identity = &self.set.identity;
        if identity.index_head_hash.as_deref() != Some(key.base)
            || !identity.input_completeness.is_complete()
        {
            return Err(E::BaselineInvalid);
        }
        Ok(())
    }

    fn require_key(&self, key: &ExactPreparedKey<'_>) -> Result<()> {
        if self.row.name != key.baseline_name || self.row.payload_digest != key.baseline_digest {
            return Err(E::BaselineChanged);
        }
        Ok(())
    }
}

pub(crate) fn read_index_generation(conn: &Connection) -> Result<IndexGeneration> {
    read_generation(conn).map_err(|error| match error {
        CaptureSnapshotError::Sql(error) => sql_error(error).into(),
        _ => E::Corrupt,
    })
}

fn confirm_index_generation(conn: &Connection, expected: &IndexGeneration) -> Result<()> {
    if &read_index_generation(conn)? != expected {
        return Err(E::IndexChanged);
    }
    Ok(())
}

fn key_params<'a>(scope: &'a str, key: &'a ExactPreparedKey<'a>) -> [&'a str; 5] {
    [
        scope,
        key.head,
        key.base,
        key.baseline_name,
        key.baseline_digest,
    ]
}

fn read_attestation(
    conn: &Connection,
    scope: &str,
    key: &ExactPreparedKey<'_>,
    baseline: &VerifiedBaseline,
    expected_digest: &str,
) -> Result<Option<DateTime<Utc>>> {
    let mut statement = conn
        .prepare(
            "SELECT attestation_version,
                CASE WHEN typeof(input_identity_json)='text'
                          AND octet_length(input_identity_json) BETWEEN 1 AND ?5
                     THEN input_identity_json END,
                CASE WHEN typeof(attestation_digest)='text' AND octet_length(attestation_digest)=71
                     THEN attestation_digest END,
                CASE WHEN typeof(observed_at)='text' AND octet_length(observed_at) BETWEEN 1 AND 64
                     THEN observed_at END
         FROM change_evidence_attestations
         WHERE repo_scope=?1 AND base=?2 AND baseline_name=?3 AND baseline_digest=?4",
        )
        .map_err(sql_error)?;
    let mut rows = statement
        .query(params![
            scope,
            key.base,
            key.baseline_name,
            key.baseline_digest,
            IDENTITY_CAP
        ])
        .map_err(sql_error)?;
    let Some(row) = rows.next().map_err(sql_error)? else {
        return Ok(None);
    };
    baseline.require_attestable(key)?;
    if integer(row, 0)? != VERSION
        || text(row, 1)? != baseline.row.identity_json
        || text(row, 2)? != expected_digest
    {
        return Err(E::Corrupt);
    }
    let observed = bounded_timestamp(text(row, 3)?)?;
    if rows.next().map_err(sql_error)?.is_some() {
        return Err(E::Corrupt);
    }
    Ok(Some(observed))
}

fn require_one(affected: usize) -> Result<()> {
    if affected != 1 {
        return Err(E::WriteConflict);
    }
    Ok(())
}

#[derive(Debug)]
pub(crate) struct Attested {
    pub scope: Uuid,
    pub observed: DateTime<Utc>,
}

/// The producer has already reproved the immutable baseline for the exact
/// parent. Repeated attestation of the same verified row returns the original
/// committed observation rather than rewriting its evidence.
pub(crate) fn commit_attestation(
    conn: &mut Connection,
    ownership: WriteScope<'_>,
    key: &ExactPreparedKey<'_>,
    baseline: &VerifiedBaseline,
    reproduced: &FileEdgeSet,
    generation: &IndexGeneration,
    observed: DateTime<Utc>,
) -> Result<Attested> {
    commit_attestation_guarded(
        conn,
        ownership,
        key,
        baseline,
        AttestationInput {
            reproduced,
            generation,
            observed,
        },
        #[cfg(target_os = "linux")]
        |_| Ok(()),
        |_| Ok(()),
    )
}

pub(crate) struct AttestationInput<'a> {
    pub(crate) reproduced: &'a FileEdgeSet,
    pub(crate) generation: &'a IndexGeneration,
    pub(crate) observed: DateTime<Utc>,
}

pub(crate) fn commit_attestation_guarded(
    conn: &mut Connection,
    ownership: WriteScope<'_>,
    key: &ExactPreparedKey<'_>,
    baseline: &VerifiedBaseline,
    input: AttestationInput<'_>,
    #[cfg(target_os = "linux")] admit_native: impl FnOnce(&Connection) -> Result<()>,
    confirm_native: impl FnOnce(&Connection) -> Result<()>,
) -> Result<Attested> {
    let AttestationInput {
        reproduced,
        generation,
        observed,
    } = input;
    baseline.require_key(key)?;
    baseline.require_attestable(key)?;
    if &baseline.set != reproduced || generation.head_hash.as_deref() != Some(key.base) {
        return Err(E::BaselineInvalid);
    }
    let digest = content_digest(&baseline.row.identity_json);
    let observed_at = observed.to_rfc3339();
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(sql_error)?;
    #[cfg(target_os = "linux")]
    admit_native(&tx)?;
    let existing = ownership.verify(&tx)?;
    require_foreign_keys(&tx)?;
    baseline.confirm(&tx)?;
    confirm_index_generation(&tx, generation)?;
    let scope = ownership.provision(&tx, existing, &observed_at)?;
    let scope_key = scope.hyphenated().to_string();
    if let Some(observed) = read_attestation(&tx, &scope_key, key, baseline, &digest)? {
        confirm_native(&tx)?;
        tx.commit().map_err(sql_error)?;
        return Ok(Attested { scope, observed });
    }
    let has_name: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM change_evidence_names WHERE repo_scope=?1 AND baseline_name=?2)",
        params![scope_key, key.baseline_name],
        |r| r.get(0),
    ).map_err(sql_error)?;
    if !has_name {
        let full = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM change_evidence_names WHERE repo_scope=?1 LIMIT 1 OFFSET 127)",
            [&scope_key],
            |r| r.get::<_, bool>(0),
        ).map_err(sql_error)?;
        if full {
            return Err(E::Capacity);
        }
        require_one(
            tx.execute(
                "INSERT INTO change_evidence_names(repo_scope,baseline_name) VALUES(?1,?2)",
                params![scope_key, key.baseline_name],
            )
            .map_err(sql_error)?,
        )?;
    }
    require_one(tx.execute(
        "INSERT INTO change_evidence_attestations(repo_scope,base,baseline_name,baseline_digest,
             attestation_version,input_identity_json,attestation_digest,observed_at)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
        params![scope_key, key.base, key.baseline_name, key.baseline_digest,
                VERSION, baseline.row.identity_json, digest, observed_at],
    ).map_err(sql_error)?)?;
    confirm_native(&tx)?;
    tx.commit().map_err(sql_error)?;
    Ok(Attested { scope, observed })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PreparedStatus {
    Comparable,
    Incomparable,
    Unavailable,
}

// Only explicitly audited DTOs may be admitted here. The authoritative
// projection adds its concrete Sealed/PreparedProjection implementations
// here when the producer is wired.
mod seal {
    pub trait Sealed {}
}

pub(crate) trait PreparedProjection: seal::Sealed + Serialize {
    fn status(&self) -> PreparedStatus;
    fn has_git_evidence(&self) -> bool {
        false
    }
}

pub(crate) struct PreparedBody {
    status: PreparedStatus,
    has_git_evidence: bool,
    json: String,
    digest: String,
}

impl PreparedBody {
    pub(crate) fn encode(projection: &impl PreparedProjection) -> Result<Self> {
        use std::io::{self, Write};
        struct Capped(Vec<u8>);
        impl Write for Capped {
            fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
                if buf.len() > PREPARED_CAP - self.0.len() {
                    return Err(io::Error::other("prepared payload limit"));
                }
                self.0.extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let mut sink = Capped(Vec::new());
        serde_json::to_writer(&mut sink, projection).map_err(|error| {
            if error.is_io() {
                E::PayloadOverBudget
            } else {
                E::InvalidInput
            }
        })?;
        let json = String::from_utf8(sink.0).map_err(|_| E::InvalidInput)?;
        let digest = content_digest(&json);
        Ok(Self {
            status: projection.status(),
            has_git_evidence: projection.has_git_evidence(),
            json,
            digest,
        })
    }
}

#[derive(Debug, Eq, PartialEq)]
struct PreparedStamp {
    generation: i64,
    observed: DateTime<Utc>,
    digest: String,
}

fn prepared_stamp(
    conn: &Connection,
    scope: &str,
    key: &ExactPreparedKey<'_>,
) -> Result<Option<PreparedStamp>> {
    let mut statement = conn
        .prepare(
            "SELECT generation,payload_version,typeof(payload_json),octet_length(payload_json),
                CASE WHEN typeof(payload_digest)='text' AND octet_length(payload_digest)=71
                     THEN payload_digest END,
                CASE WHEN typeof(captured_at)='text' AND octet_length(captured_at) BETWEEN 1 AND 64
                     THEN captured_at END
         FROM change_evidence_prepared
         WHERE repo_scope=?1 AND head=?2 AND base=?3 AND baseline_name=?4 AND baseline_digest=?5",
        )
        .map_err(sql_error)?;
    let mut rows = statement.query(key_params(scope, key)).map_err(sql_error)?;
    let Some(row) = rows.next().map_err(sql_error)? else {
        return Ok(None);
    };
    let generation = integer(row, 0)?;
    let bytes = integer(row, 3)?;
    if generation <= 0 || integer(row, 1)? != VERSION || text(row, 2)? != "text" || bytes <= 0 {
        return Err(E::Corrupt);
    }
    if bytes > PREPARED_CAP as i64 {
        return Err(E::PayloadOverBudget);
    }
    let digest = text(row, 4)?;
    ExactPreparedKey::new(key.head, key.base, key.baseline_name, digest).map_err(|_| E::Corrupt)?;
    let observed = bounded_timestamp(text(row, 5)?)?;
    let stamp = PreparedStamp {
        generation,
        observed,
        digest: digest.to_owned(),
    };
    if rows.next().map_err(sql_error)?.is_some() {
        return Err(E::Corrupt);
    }
    Ok(Some(stamp))
}

/// Captured before doing repository work, in the same read transaction as the
/// baseline and relevant index generations. Never refresh after a conflict.
pub(crate) struct PreparedExpectation {
    guard: [u8; 32],
    scope: ScopeExpectation,
    head: String,
    base: String,
    name: String,
    digest: String,
    stamp: Option<PreparedStamp>,
}

pub(crate) fn read_prepared_expectation(
    conn: &Connection,
    ownership: WriteScope<'_>,
    key: ExactPreparedKey<'_>,
) -> Result<PreparedExpectation> {
    let stamp = match ownership.verify(conn)? {
        Some(id) => prepared_stamp(conn, &id.hyphenated().to_string(), &key)?,
        None => None,
    };
    Ok(PreparedExpectation {
        guard: *ownership.guard,
        scope: ownership.expected,
        head: key.head.to_owned(),
        base: key.base.to_owned(),
        name: key.baseline_name.to_owned(),
        digest: key.baseline_digest.to_owned(),
        stamp,
    })
}

pub(crate) fn require_prepared_attestation(
    conn: &Connection,
    scope: Uuid,
    key: &ExactPreparedKey<'_>,
    baseline: &VerifiedBaseline,
) -> Result<()> {
    baseline.require_key(key)?;
    baseline.require_attestable(key)?;
    let digest = content_digest(&baseline.row.identity_json);
    if read_attestation(
        conn,
        &scope.hyphenated().to_string(),
        key,
        baseline,
        &digest,
    )?
    .is_none()
    {
        return Err(E::AttestationMissing);
    }
    Ok(())
}

pub(crate) struct PreparedCommit {
    pub scope: Uuid,
    pub generation: i64,
}

pub(crate) struct PreparedWrite<'a> {
    pub baseline: &'a VerifiedBaseline,
    pub index: &'a IndexGeneration,
    pub body: &'a PreparedBody,
    pub observed: DateTime<Utc>,
}

/// The producer has already checked the exact Git pair and finished its native
/// observations. Baseline/generation reads and the exact-key update share one
/// IMMEDIATE transaction.
pub(crate) fn commit_prepared(
    conn: &mut Connection,
    expected: PreparedExpectation,
    baseline: &VerifiedBaseline,
    index: &IndexGeneration,
    body: &PreparedBody,
    observed: DateTime<Utc>,
) -> Result<PreparedCommit> {
    commit_prepared_guarded(
        conn,
        expected,
        PreparedWrite {
            baseline,
            index,
            body,
            observed,
        },
        #[cfg(target_os = "linux")]
        |_| Ok(()),
        |_| Ok(()),
    )
}

pub(crate) fn commit_prepared_guarded(
    conn: &mut Connection,
    expected: PreparedExpectation,
    write: PreparedWrite<'_>,
    #[cfg(target_os = "linux")] admit_native: impl FnOnce(&Connection) -> Result<()>,
    confirm_native: impl FnOnce(&Connection) -> Result<()>,
) -> Result<PreparedCommit> {
    let PreparedWrite {
        baseline,
        index,
        body,
        observed,
    } = write;
    let key = &ExactPreparedKey::new(
        &expected.head,
        &expected.base,
        &expected.name,
        &expected.digest,
    )?;
    let ownership = WriteScope {
        guard: &expected.guard,
        expected: expected.scope,
    };
    baseline.require_key(key)?;
    let attestation_digest = content_digest(&baseline.row.identity_json);
    let observed_at = observed.to_rfc3339();
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(sql_error)?;
    #[cfg(target_os = "linux")]
    admit_native(&tx)?;
    let existing = ownership.verify(&tx)?;
    require_foreign_keys(&tx)?;
    baseline.confirm(&tx)?;
    confirm_index_generation(&tx, index)?;
    let scope = ownership.provision(&tx, existing, &observed_at)?;
    let scope_key = scope.hyphenated().to_string();
    if (body.status != PreparedStatus::Unavailable || body.has_git_evidence)
        && read_attestation(&tx, &scope_key, key, baseline, &attestation_digest)?.is_none()
    {
        return Err(E::AttestationMissing);
    }
    let current = prepared_stamp(&tx, &scope_key, key)?;
    if current != expected.stamp
        || current
            .as_ref()
            .is_some_and(|stamp| observed < stamp.observed)
    {
        return Err(E::WriteConflict);
    }
    let generation = if let Some(prior) = current {
        let next = prior.generation.checked_add(1).ok_or(E::WriteConflict)?;
        require_one(tx.execute(
            "UPDATE change_evidence_prepared SET generation=?6,payload_version=?7,payload_json=?8,
                 payload_digest=?9,captured_at=?10
             WHERE repo_scope=?1 AND head=?2 AND base=?3 AND baseline_name=?4 AND baseline_digest=?5
                 AND generation=?11",
            params![scope_key, key.head, key.base, key.baseline_name, key.baseline_digest,
                    next, VERSION, body.json, body.digest, observed_at, prior.generation],
        ).map_err(sql_error)?)?;
        next
    } else {
        let full = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM change_evidence_prepared WHERE repo_scope=?1 LIMIT 1 OFFSET 31)",
            [&scope_key],
            |r| r.get::<_, bool>(0),
        ).map_err(sql_error)?;
        if full {
            return Err(E::Capacity);
        }
        require_one(tx.execute(
            "INSERT INTO change_evidence_prepared(repo_scope,head,base,baseline_name,baseline_digest,
                 generation,payload_version,payload_json,payload_digest,captured_at)
             VALUES(?1,?2,?3,?4,?5,1,?6,?7,?8,?9)",
            params![scope_key, key.head, key.base, key.baseline_name, key.baseline_digest,
                    VERSION, body.json, body.digest, observed_at],
        ).map_err(sql_error)?)?;
        1
    };
    confirm_native(&tx)?;
    tx.commit().map_err(sql_error)?;
    Ok(PreparedCommit { scope, generation })
}
