//! Bounded selected-attempt observation after a real Git pair proof.
use super::{
    EvidenceReader, EvidenceStorageError as E, Result, integer, resolve_qualified_tx, sql_error,
    text,
};
pub(crate) use crate::change_evidence::attempt_projection::AttemptKind;
use crate::change_evidence::attempt_projection::{PAYLOAD_CAP, Projection, Scope, payload_digest};
use crate::change_evidence::pack_store::CommitPair;
use crate::verify::selection_evidence::{SelectionBasis, StepState};
use chrono::{DateTime, Utc};
use rusqlite::{Connection, types::ValueRef};
use std::collections::HashSet;
use uuid::Uuid;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SampleRef {
    pub position: i64,
    pub result_id: i64,
}
/// Opaque internal value. The caller-supplied bounded validator has accepted
/// this exact run-associated tx, and a same-snapshot ledger EXISTS matched it.
#[derive(Clone, Eq, PartialEq)]
pub(crate) struct ProvenTxId(String);
impl std::fmt::Debug for ProvenTxId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ProvenTxId(<validated>)")
    }
}
impl ProvenTxId {
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum AttemptState {
    NotRecorded,
    Planned {
        kind: AttemptKind,
    },
    RunRecordLost {
        kind: AttemptKind,
    },
    Recorded {
        run_id: i64,
        samples: Vec<SampleRef>,
        tx_id: Option<ProvenTxId>,
    },
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SelectedAttempt {
    pub attempt_id: Uuid,
    pub state: AttemptState,
}

fn oid(value: &str) -> bool {
    value.len() == 40
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
// Metadata reads are capped: oversized schemas are unavailable, not scanned.
fn require_index(
    conn: &Connection,
    table: &str,
    index: &str,
    unique: i64,
    origin: &str,
    keys: &[&str],
    descending_last: bool,
) -> Result<()> {
    let mut stmt = conn.prepare("SELECT name COLLATE BINARY=?2, \"unique\", origin, partial FROM pragma_index_list(?1,'main') LIMIT 65").map_err(sql_error)?;
    let mut rows = stmt
        .query(rusqlite::params![table, index])
        .map_err(sql_error)?;
    let mut found = false;
    let mut n = 0;
    while let Some(row) = rows.next().map_err(sql_error)? {
        n += 1;
        if n > 64 {
            return Err(E::StorageUnavailable);
        }
        if integer(row, 0)? == 1 {
            if found
                || integer(row, 1)? != unique
                || text(row, 2)? != origin
                || integer(row, 3)? != 0
            {
                return Err(E::Corrupt);
            }
            found = true;
        }
    }
    if !found {
        return Err(E::Corrupt);
    }
    let mut stmt = conn.prepare("SELECT seqno,cid,typeof(name),octet_length(name),name,\"desc\",typeof(coll),octet_length(coll),coll,\"key\" FROM pragma_index_xinfo(?1,'main') LIMIT 6").map_err(sql_error)?;
    let mut rows = stmt.query([index]).map_err(sql_error)?;
    let mut n = 0;
    let mut k = 0;
    let mut tail = false;
    while let Some(row) = rows.next().map_err(sql_error)? {
        n += 1;
        if n > 5 {
            return Err(E::StorageUnavailable);
        }
        match integer(row, 9)? {
            0 => tail = true,
            1 => {
                if tail
                    || k >= keys.len()
                    || integer(row, 0)? != k as i64
                    || integer(row, 1)? < 0
                    || text(row, 2)? != "text"
                    || integer(row, 3)? != keys[k].len() as i64
                    || text(row, 4)? != keys[k]
                    || integer(row, 5)? != i64::from(descending_last && k + 1 == keys.len())
                    || text(row, 6)? != "text"
                    || integer(row, 7)? != 6
                    || text(row, 8)? != "BINARY"
                {
                    return Err(E::Corrupt);
                }
                k += 1;
            }
            _ => return Err(E::Corrupt),
        }
    }
    if k != keys.len() {
        return Err(E::Corrupt);
    }
    Ok(())
}

fn require_rowid_pk(conn: &Connection, table: &str) -> Result<()> {
    let mut stmt = conn
        .prepare(
            "SELECT type,wr FROM pragma_table_list(?1) WHERE schema COLLATE BINARY='main' LIMIT 2",
        )
        .map_err(sql_error)?;
    let mut rows = stmt.query([table]).map_err(sql_error)?;
    let row = rows.next().map_err(sql_error)?.ok_or(E::Corrupt)?;
    if text(row, 0)? != "table"
        || integer(row, 1)? != 0
        || rows.next().map_err(sql_error)?.is_some()
    {
        return Err(E::Corrupt);
    }
    let mut stmt = conn.prepare("SELECT name='id' COLLATE NOCASE, name='_rowid_' COLLATE NOCASE OR name='rowid' COLLATE NOCASE OR name='oid' COLLATE NOCASE, typeof(type),octet_length(type),type,pk,hidden FROM pragma_table_xinfo(?1,'main') LIMIT 65").map_err(sql_error)?;
    let mut rows = stmt.query([table]).map_err(sql_error)?;
    let mut found = false;
    let mut n = 0;
    while let Some(row) = rows.next().map_err(sql_error)? {
        n += 1;
        if n > 64 {
            return Err(E::StorageUnavailable);
        }
        if integer(row, 1)? != 0 {
            return Err(E::Corrupt);
        }
        let pk = integer(row, 5)?;
        if integer(row, 0)? == 1 {
            if found
                || pk != 1
                || integer(row, 6)? != 0
                || text(row, 2)? != "text"
                || integer(row, 3)? != 7
                || !text(row, 4)?.eq_ignore_ascii_case("INTEGER")
            {
                return Err(E::Corrupt);
            }
            found = true;
        } else if pk != 0 {
            return Err(E::Corrupt);
        }
    }
    if !found {
        return Err(E::Corrupt);
    }
    let mut stmt = conn
        .prepare("SELECT origin COLLATE BINARY='pk' FROM pragma_index_list(?1,'main') LIMIT 65")
        .map_err(sql_error)?;
    let mut rows = stmt.query([table]).map_err(sql_error)?;
    let mut n = 0;
    while let Some(row) = rows.next().map_err(sql_error)? {
        n += 1;
        if n > 64 {
            return Err(E::StorageUnavailable);
        }
        if integer(row, 0)? != 0 {
            return Err(E::Corrupt);
        }
    }
    Ok(())
}

fn lost(id: Uuid) -> SelectedAttempt {
    SelectedAttempt {
        attempt_id: id,
        state: AttemptState::RunRecordLost {
            kind: AttemptKind::Executed,
        },
    }
}

/// The `pair` must come from `Observation::observe_commit_pair` on the served,
/// independently qualified repository, in the same request budget. The store
/// has no base column on attempts: it binds the verified pair's exact HEAD, not
/// a caller-provided or independently SQL-verified parent. The tx validator and
/// byte cap are supplied by the safe transport boundary; the validator must be
/// pure and bounded, and this seam does not invent a transaction-ID grammar or
/// expose unvalidated values.
pub(crate) fn selected_attempt_in_tx<F>(
    conn: &Connection,
    guard: &[u8; 32],
    requested_scope: Uuid,
    pair: &CommitPair,
    attempt_id: Uuid,
    tx_byte_cap: u64,
    tx_valid: &F,
) -> Result<SelectedAttempt>
where
    F: Fn(&str) -> bool,
{
    let resolved = resolve_qualified_tx(conn, guard)?;
    if resolved != requested_scope {
        return Err(E::ScopeUnavailable);
    }
    let head = pair.head.to_string();
    let base = pair.parent.to_string();
    if !oid(&head) || !oid(&base) {
        return Err(E::InvalidInput);
    }
    let attempt_key = attempt_id.hyphenated().to_string();
    let scope_key = resolved.hyphenated().to_string();
    require_index(
        conn,
        "change_evidence_attempts",
        "sqlite_autoindex_change_evidence_attempts_2",
        1,
        "u",
        &["repo_scope", "attempt_id"],
        false,
    )?;
    let (kind, bound, run_id, expected) = {
        let mut stmt=conn.prepare("SELECT typeof(attempt_kind),octet_length(attempt_kind),attempt_kind,originally_bound,run_id,sample_count FROM main.change_evidence_attempts INDEXED BY sqlite_autoindex_change_evidence_attempts_2 WHERE repo_scope COLLATE BINARY=?1 AND head COLLATE BINARY=?2 AND attempt_id COLLATE BINARY=?3 LIMIT 2").map_err(sql_error)?;
        let mut rows = stmt
            .query(rusqlite::params![&scope_key, &head, &attempt_key])
            .map_err(sql_error)?;
        let values = match rows.next().map_err(sql_error)? {
            None => {
                return Ok(SelectedAttempt {
                    attempt_id,
                    state: AttemptState::NotRecorded,
                });
            }
            Some(row) => {
                if text(row, 0)? != "text" {
                    return Err(E::Corrupt);
                }
                let len = integer(row, 1)?;
                if !(1..=8).contains(&len) {
                    return Err(E::Corrupt);
                }
                let kind = match text(row, 2)? {
                    "executed" => AttemptKind::Executed,
                    "dry-run" => AttemptKind::DryRun,
                    "refused" => AttemptKind::Refused,
                    _ => return Err(E::Corrupt),
                };
                let bound = integer(row, 3)?;
                let run = match row.get_ref(4).map_err(sql_error)? {
                    ValueRef::Null => None,
                    ValueRef::Integer(i) if i > 0 => Some(i),
                    _ => return Err(E::Corrupt),
                };
                let count = integer(row, 5)?;
                if !(0..=32).contains(&count) {
                    return Err(E::Corrupt);
                }
                (kind, bound, run, count)
            }
        };
        if rows.next().map_err(sql_error)?.is_some() {
            return Err(E::Corrupt);
        }
        values
    };
    match kind {
        AttemptKind::Executed if bound != 1 => return Err(E::Corrupt),
        AttemptKind::DryRun | AttemptKind::Refused
            if bound != 0 || run_id.is_some() || expected != 0 =>
        {
            return Err(E::Corrupt);
        }
        _ => {}
    }
    if kind == AttemptKind::Executed && run_id.is_none() {
        return Ok(lost(attempt_id));
    }
    require_index(
        conn,
        "change_evidence_samples",
        "sqlite_autoindex_change_evidence_samples_1",
        1,
        "pk",
        &["repo_scope", "attempt_id", "position"],
        false,
    )?;
    let mut samples = Vec::with_capacity(usize::try_from(expected).map_err(|_| E::Corrupt)?);
    {
        let mut stmt=conn.prepare("SELECT position,result_id FROM main.change_evidence_samples INDEXED BY sqlite_autoindex_change_evidence_samples_1 WHERE repo_scope COLLATE BINARY=?1 AND attempt_id COLLATE BINARY=?2 ORDER BY position COLLATE BINARY LIMIT 33").map_err(sql_error)?;
        let mut rows = stmt
            .query(rusqlite::params![&scope_key, &attempt_key])
            .map_err(sql_error)?;
        let mut ids = HashSet::new();
        let mut previous = None;
        while let Some(row) = rows.next().map_err(sql_error)? {
            if samples.len() == 32 {
                return Err(E::Corrupt);
            }
            let position = integer(row, 0)?;
            let result_id = integer(row, 1)?;
            if position < 0
                || result_id <= 0
                || previous.is_some_and(|p| position <= p)
                || !ids.insert(result_id)
            {
                return Err(E::Corrupt);
            }
            previous = Some(position);
            samples.push(SampleRef {
                position,
                result_id,
            });
        }
    }
    if kind != AttemptKind::Executed {
        if !samples.is_empty() {
            return Err(E::Corrupt);
        }
        return Ok(SelectedAttempt {
            attempt_id,
            state: AttemptState::Planned { kind },
        });
    }
    let expected = usize::try_from(expected).map_err(|_| E::Corrupt)?;
    if samples.len() < expected {
        return Ok(lost(attempt_id));
    }
    if samples.len() > expected {
        return Err(E::Corrupt);
    }
    let run_id = run_id.ok_or(E::Corrupt)?;
    require_rowid_pk(conn, "verification_runs")?;
    require_rowid_pk(conn, "verification_results")?;
    let tx_candidate = {
        let mut stmt=conn.prepare("SELECT id,typeof(tx_id),octet_length(tx_id) FROM main.verification_runs WHERE _rowid_=?1 AND id=?1 LIMIT 2").map_err(sql_error)?;
        let mut rows = stmt.query([run_id]).map_err(sql_error)?;
        let candidate = match rows.next().map_err(sql_error)? {
            None => return Ok(lost(attempt_id)),
            Some(row) => {
                if integer(row, 0)? != run_id {
                    return Err(E::Corrupt);
                }
                if text(row, 1)? == "text" {
                    let n = integer(row, 2)?;
                    n > 0 && u64::try_from(n).is_ok_and(|n| n <= tx_byte_cap)
                } else {
                    false
                }
            }
        };
        if rows.next().map_err(sql_error)?.is_some() {
            return Err(E::Corrupt);
        }
        candidate
    };
    for sample in &samples {
        let mut stmt = conn
            .prepare("SELECT id,run_id FROM main.verification_results WHERE _rowid_=?1 AND id=?1 LIMIT 2")
            .map_err(sql_error)?;
        let mut rows = stmt.query([sample.result_id]).map_err(sql_error)?;
        let matches = match rows.next().map_err(sql_error)? {
            None => return Ok(lost(attempt_id)),
            Some(row) => {
                if integer(row, 0)? != sample.result_id {
                    return Err(E::Corrupt);
                }
                match row.get_ref(1).map_err(sql_error)? {
                    ValueRef::Integer(owner) => owner == run_id,
                    ValueRef::Null => false,
                    _ => return Err(E::Corrupt),
                }
            }
        };
        if rows.next().map_err(sql_error)?.is_some() {
            return Err(E::Corrupt);
        }
        if !matches {
            return Ok(lost(attempt_id));
        }
    }
    let tx_id = if tx_candidate {
        let value = {
            let mut stmt = conn
                .prepare(
                    "SELECT tx_id FROM main.verification_runs WHERE _rowid_=?1 AND id=?1 LIMIT 2",
                )
                .map_err(sql_error)?;
            let mut rows = stmt.query([run_id]).map_err(sql_error)?;
            let value = match rows.next().map_err(sql_error)? {
                Some(row) => match row.get_ref(0).map_err(sql_error)? {
                    ValueRef::Text(bytes)
                        if u64::try_from(bytes.len()).is_ok_and(|n| n <= tx_byte_cap) =>
                    {
                        std::str::from_utf8(bytes)
                            .ok()
                            .filter(|v| tx_valid(v))
                            .map(str::to_owned)
                    }
                    _ => None,
                },
                None => return Ok(lost(attempt_id)),
            };
            if rows.next().map_err(sql_error)?.is_some() {
                return Err(E::Corrupt);
            }
            value
        };
        match value {
            Some(value) => {
                require_index(
                    conn,
                    "ledger_entries",
                    "idx_change_evidence_ledger_tx",
                    0,
                    "c",
                    &["tx_id"],
                    false,
                )?;
                let mut stmt = conn
                    .prepare("SELECT EXISTS(SELECT 1 FROM main.ledger_entries INDEXED BY idx_change_evidence_ledger_tx WHERE tx_id COLLATE BINARY=?1 LIMIT 1)")
                    .map_err(sql_error)?;
                let mut rows = stmt.query([&value]).map_err(sql_error)?;
                let row = rows.next().map_err(sql_error)?.ok_or(E::Corrupt)?;
                if integer(row, 0)? == 1 {
                    Some(ProvenTxId(value))
                } else {
                    None
                }
            }
            None => None,
        }
    } else {
        None
    };
    Ok(SelectedAttempt {
        attempt_id,
        state: AttemptState::Recorded {
            run_id,
            samples,
            tx_id,
        },
    })
}

/// Eight newest exact-head attempts and one sentinel for incomplete history.
#[derive(Debug)]
pub(crate) struct AttemptHistory {
    pub(crate) attempts: Vec<ObservedAttempt>,
    pub(crate) complete: bool,
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct CanonicalOutcome {
    pub(crate) position: i64,
    pub(crate) exit_code: i32,
    pub(crate) duration_ms: u64,
}

#[derive(Debug)]
pub(crate) struct ObservedAttempt {
    pub(crate) selected: SelectedAttempt,
    pub(crate) outcomes: Vec<CanonicalOutcome>,
    pub(crate) observed_from: Option<DateTime<Utc>>,
    pub(crate) observed_to: Option<DateTime<Utc>>,
    pub(crate) projection: Option<Projection>,
}

fn canonical_outcomes(
    conn: &Connection,
    selected: &SelectedAttempt,
) -> Result<Vec<CanonicalOutcome>> {
    let AttemptState::Recorded {
        run_id, samples, ..
    } = &selected.state
    else {
        return Ok(Vec::new());
    };
    let mut outcomes = Vec::with_capacity(samples.len());
    let mut stmt = conn
        .prepare("SELECT CASE WHEN typeof(exit_code)='integer' THEN exit_code END, CASE WHEN typeof(duration_ms)='integer' THEN duration_ms END FROM main.verification_results WHERE _rowid_=?1 AND id=?1 AND run_id=?2")
        .map_err(sql_error)?;
    for sample in samples {
        let mut rows = stmt
            .query(rusqlite::params![sample.result_id, run_id])
            .map_err(sql_error)?;
        let row = rows.next().map_err(sql_error)?.ok_or(E::Corrupt)?;
        outcomes.push(CanonicalOutcome {
            position: sample.position,
            exit_code: i32::try_from(integer(row, 0)?).map_err(|_| E::Corrupt)?,
            duration_ms: u64::try_from(integer(row, 1)?).map_err(|_| E::Corrupt)?,
        });
    }
    Ok(outcomes)
}

type CapturedProjection = (
    Option<DateTime<Utc>>,
    Option<DateTime<Utc>>,
    Option<Projection>,
);

fn captured_projection(
    conn: &Connection,
    scope: Uuid,
    attempt: &SelectedAttempt,
    outcomes: &[CanonicalOutcome],
) -> Result<CapturedProjection> {
    let lost = matches!(attempt.state, AttemptState::RunRecordLost { .. });
    // The checked unique scope/attempt index was already verified by the
    // ownership read in this snapshot. CASE keeps oversized values out of Rust.
    let mut stmt = conn.prepare(
        "SELECT CASE WHEN typeof(observed_from)='text' AND octet_length(observed_from)<=30 THEN observed_from END,
                CASE WHEN typeof(observed_to)='text' AND octet_length(observed_to)<=30 THEN observed_to END,
                CASE WHEN typeof(payload_version)='integer' THEN payload_version END,
                CASE WHEN NOT ?3 AND typeof(payload_json)='text' AND octet_length(payload_json)<=?4 THEN payload_json END,
                CASE WHEN NOT ?3 AND typeof(payload_digest)='text' AND octet_length(payload_digest)=71 THEN payload_digest END
         FROM main.change_evidence_attempts INDEXED BY sqlite_autoindex_change_evidence_attempts_2
         WHERE repo_scope COLLATE BINARY=?1 AND attempt_id COLLATE BINARY=?2"
    ).map_err(sql_error)?;
    let mut rows = stmt
        .query(rusqlite::params![
            scope.hyphenated().to_string(),
            attempt.attempt_id.hyphenated().to_string(),
            lost,
            PAYLOAD_CAP as i64,
        ])
        .map_err(sql_error)?;
    let row = rows.next().map_err(sql_error)?.ok_or(E::Corrupt)?;
    let parse_time = |idx| {
        text(row, idx)
            .ok()
            .and_then(|raw| DateTime::parse_from_rfc3339(raw).ok())
            .map(|time| time.with_timezone(&Utc))
    };
    let from = parse_time(0);
    let to = parse_time(1);
    let kind = match attempt.state {
        AttemptState::RunRecordLost { .. } => return Ok((from, to, None)),
        AttemptState::Recorded { .. } => AttemptKind::Executed,
        AttemptState::Planned { kind } => kind,
        AttemptState::NotRecorded => return Err(E::Corrupt),
    };
    if integer(row, 2)? != 1 {
        return Err(E::Corrupt);
    }
    let json = text(row, 3)?;
    if payload_digest(json) != text(row, 4)? {
        return Err(E::Corrupt);
    }
    let projection: Projection = serde_json::from_str(json).map_err(|_| E::Corrupt)?;
    validate_projection(&projection, kind, outcomes)?;
    Ok((from, to, Some(projection)))
}

fn validate_projection(
    projection: &Projection,
    kind: AttemptKind,
    outcomes: &[CanonicalOutcome],
) -> Result<()> {
    const LIMIT: usize = 32;
    let refused = kind == AttemptKind::Refused;
    if projection.schema_version != 1
        || projection.selection_schema_version != 1
        || projection.attempt_kind != kind
        || projection.steps.len() > LIMIT
        || projection.sample.complete != (projection.sample.omitted == 0)
        || (projection.sample.omitted != 0 && projection.steps.len() != LIMIT)
        || projection.scope_requested == Scope::Refused
        || (projection.scope_executed == Scope::Refused) != refused
        || (projection.basis == SelectionBasis::Refused) != refused
        || (refused && !projection.steps.is_empty())
        || (kind != AttemptKind::Executed && projection.partial)
        || !projection
            .limitation_codes
            .contains(&"selection-soundness-not-established")
        || projection.omissions.contains(&"refused-no-commands-run") != refused
        || projection.omissions.contains(&"steps-not-run") != projection.partial
    {
        return Err(E::Corrupt);
    }
    let mut actual = outcomes.iter();
    let mut saw_not_run = false;
    for (position, step) in projection.steps.iter().enumerate() {
        if step.index != position as u64 {
            return Err(E::Corrupt);
        }
        match (step.exit_code, step.duration_ms, step.timed_out, step.state) {
            (Some(code), Some(duration), Some(_), StepState::Passed | StepState::Failed)
                if kind == AttemptKind::Executed
                    && !saw_not_run
                    && (code == 0) == (step.state == StepState::Passed) =>
            {
                let outcome = actual.next().ok_or(E::Corrupt)?;
                if outcome.position != position as i64
                    || outcome.exit_code != code
                    || outcome.duration_ms != duration
                {
                    return Err(E::Corrupt);
                }
            }
            (None, None, None, StepState::NotRun)
                if kind == AttemptKind::Executed && projection.partial =>
            {
                saw_not_run = true;
            }
            (None, None, None, StepState::Planned) if kind == AttemptKind::DryRun => {}
            _ => return Err(E::Corrupt),
        }
    }
    if actual.next().is_some() || (projection.partial && projection.sample.complete && !saw_not_run)
    {
        return Err(E::Corrupt);
    }
    Ok(())
}

pub(crate) fn attempt_history_in_tx(
    conn: &Connection,
    guard: &[u8; 32],
    requested_scope: Uuid,
    pair: &CommitPair,
) -> Result<AttemptHistory> {
    let resolved = resolve_qualified_tx(conn, guard)?;
    if resolved != requested_scope {
        return Err(E::ForeignScope);
    }
    let head = pair.head.to_string();
    if !oid(&head) || !oid(&pair.parent.to_string()) {
        return Err(E::InvalidInput);
    }
    require_rowid_pk(conn, "change_evidence_attempts")?;
    require_index(
        conn,
        "change_evidence_attempts",
        "idx_change_evidence_attempt_scope_head",
        0,
        "c",
        &["repo_scope", "head", "id"],
        true,
    )?;
    let mut ids = Vec::with_capacity(9);
    {
        let mut stmt = conn
            .prepare("SELECT id, CASE WHEN typeof(attempt_id)='text' AND octet_length(attempt_id)=36 THEN attempt_id END FROM main.change_evidence_attempts INDEXED BY idx_change_evidence_attempt_scope_head WHERE repo_scope COLLATE BINARY=?1 AND head COLLATE BINARY=?2 ORDER BY id COLLATE BINARY DESC LIMIT 9")
            .map_err(sql_error)?;
        let mut rows = stmt
            .query(rusqlite::params![resolved.hyphenated().to_string(), head])
            .map_err(sql_error)?;
        let mut previous = None;
        while let Some(row) = rows.next().map_err(sql_error)? {
            let insertion = integer(row, 0)?;
            let raw = text(row, 1)?;
            let attempt_id = Uuid::parse_str(raw).map_err(|_| E::Corrupt)?;
            if insertion <= 0
                || previous.is_some_and(|last| last <= insertion)
                || attempt_id.get_version_num() != 4
                || attempt_id.get_variant() != uuid::Variant::RFC4122
                || attempt_id.hyphenated().to_string() != raw
            {
                return Err(E::Corrupt);
            }
            previous = Some(insertion);
            ids.push(attempt_id);
        }
    }
    let complete = ids.len() <= 8;
    let mut attempts = Vec::with_capacity(ids.len().min(8));
    for attempt_id in ids.into_iter().take(8) {
        let attempt =
            selected_attempt_in_tx(conn, guard, resolved, pair, attempt_id, 36, &|raw| {
                Uuid::parse_str(raw)
                    .is_ok_and(|id| !id.is_nil() && id.hyphenated().to_string() == raw)
            })?;
        if attempt.state == AttemptState::NotRecorded {
            return Err(E::Corrupt);
        }
        let outcomes = canonical_outcomes(conn, &attempt)?;
        let (observed_from, observed_to, projection) =
            captured_projection(conn, resolved, &attempt, &outcomes)?;
        attempts.push(ObservedAttempt {
            selected: attempt,
            outcomes,
            observed_from,
            observed_to,
            projection,
        });
    }
    Ok(AttemptHistory { attempts, complete })
}

impl EvidenceReader {
    /// `pair` is an actual already-proven Git pair. The returned data is owned;
    /// it contains only verified internal IDs/positions and optional proven tx,
    /// never raw command, outcome, selection or unvalidated payload bytes.
    pub(crate) fn read_selected_attempt<F>(
        &self,
        guard: &[u8; 32],
        requested_scope: Uuid,
        pair: &CommitPair,
        attempt_id: Uuid,
        tx_byte_cap: u64,
        tx_valid: &F,
    ) -> Result<SelectedAttempt>
    where
        F: Fn(&str) -> bool,
    {
        let tx = self.conn.unchecked_transaction().map_err(sql_error)?;
        let result = selected_attempt_in_tx(
            &tx,
            guard,
            requested_scope,
            pair,
            attempt_id,
            tx_byte_cap,
            tx_valid,
        )?;
        tx.commit().map_err(sql_error)?;
        Ok(result)
    }
}
