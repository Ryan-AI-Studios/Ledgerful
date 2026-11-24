use super::{
    EvidenceStorageError as E, ExactPreparedKey, Result, integer, sql_error, text,
    writes::{VerifiedBaseline, WriteError, read_baseline},
};
use chrono::{DateTime, Utc};
use rusqlite::{Connection, params};
use serde::Serialize;
use uuid::Uuid;

const CANDIDATE_CAP: usize = 32;
const BASELINE_READ_CAP: u64 = 16 * 1024 * 1024;

#[derive(Serialize)]
#[cfg_attr(feature = "web", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub(crate) struct BaselineCandidate {
    name: String,
    payload_digest: String,
    base: String,
    #[cfg_attr(feature = "web", schema(value_type = String, format = DateTime))]
    observed_at: DateTime<Utc>,
}

pub(crate) struct CandidatePage {
    pub(crate) items: Vec<BaselineCandidate>,
    pub(crate) complete: bool,
}

#[derive(Clone, Copy, Serialize)]
#[cfg_attr(feature = "web", derive(utoipa::ToSchema))]
#[serde(rename_all = "kebab-case")]
pub(crate) enum BaselineVerdict {
    BaselineMissing,
    BaselineUnattested,
    BaselineWrongParent,
    BaselineDigestMismatch,
    BaselineIncompatible,
    NotPrepared,
    LiveProofUnavailable,
    HistoricalHead,
    HeadMoved,
    DirtyWorktree,
    IndexStale,
    ConfigChanged,
    ProofStale,
    ProofTimeInvalid,
    ProofTimeFuture,
}

pub(crate) enum SelectionMetadata {
    Eligible,
    Unavailable(BaselineVerdict),
}

fn load_baseline(
    conn: &Connection,
    guard: &[u8; 32],
    scope: &str,
    name: &str,
    remaining_bytes: &mut u64,
) -> Result<Option<std::result::Result<VerifiedBaseline, BaselineVerdict>>> {
    use rusqlite::OptionalExtension;
    let bytes = conn
        .query_row(
            "SELECT octet_length(b.payload_json) FROM main.analysis_baselines b
             JOIN main.change_evidence_scopes s ON s.repo_scope COLLATE BINARY=?1
             WHERE b.name COLLATE BINARY=?2 LIMIT 1",
            params![scope, name],
            |row| row.get::<_, Option<i64>>(0),
        )
        .optional()
        .map_err(sql_error)?;
    let Some(bytes) = bytes else { return Ok(None) };
    let Some(bytes) = bytes.and_then(|bytes| u64::try_from(bytes).ok()) else {
        return Ok(Some(Err(BaselineVerdict::BaselineIncompatible)));
    };
    *remaining_bytes = remaining_bytes
        .checked_sub(bytes)
        .ok_or(E::PayloadOverBudget)?;
    let verified = match read_baseline(conn, guard, name) {
        Ok(baseline) => Ok(baseline),
        Err(WriteError::BaselineMissing) => return Ok(None),
        Err(WriteError::BaselineChanged) => Err(BaselineVerdict::BaselineDigestMismatch),
        Err(WriteError::BaselineInvalid) => Err(BaselineVerdict::BaselineIncompatible),
        Err(WriteError::Storage(error)) => return Err(error),
        Err(_) => return Err(E::Corrupt),
    };
    Ok(Some(verified))
}

fn read_attested_time(
    conn: &Connection,
    scope: &str,
    base: &str,
    name: &str,
    digest: &str,
    baseline: &VerifiedBaseline,
) -> Result<Option<DateTime<Utc>>> {
    let mut statement = conn.prepare(
        "SELECT attestation_version,
            CASE WHEN typeof(input_identity_json)='text' AND octet_length(input_identity_json) BETWEEN 1 AND 16384
                 THEN input_identity_json END,
            CASE WHEN typeof(attestation_digest)='text' AND octet_length(attestation_digest)=71
                 THEN attestation_digest END,
            CASE WHEN typeof(observed_at)='text' AND octet_length(observed_at) BETWEEN 1 AND 64
                 THEN observed_at END
         FROM main.change_evidence_attestations INDEXED BY sqlite_autoindex_change_evidence_attestations_1
         WHERE repo_scope COLLATE BINARY=?1 AND base COLLATE BINARY=?2
           AND baseline_name COLLATE BINARY=?3 AND baseline_digest COLLATE BINARY=?4
         LIMIT 1",
    ).map_err(sql_error)?;
    let mut rows = statement
        .query(params![scope, base, name, digest])
        .map_err(sql_error)?;
    let Some(row) = rows.next().map_err(sql_error)? else {
        return Ok(None);
    };
    let identity = serde_json::to_string(&baseline.set().identity).map_err(|_| E::Corrupt)?;
    if integer(row, 0)? != 1
        || text(row, 1)? != identity
        || text(row, 2)? != format!("blake3:{}", blake3::hash(identity.as_bytes()).to_hex())
    {
        return Err(E::Corrupt);
    }
    DateTime::parse_from_rfc3339(text(row, 3)?)
        .map(|time| Some(time.with_timezone(&Utc)))
        .map_err(|_| E::Corrupt)
}

pub(crate) fn candidates_in_tx(
    conn: &Connection,
    guard: &[u8; 32],
    scope: Uuid,
    head: &str,
    base: &str,
) -> Result<CandidatePage> {
    let scope = scope.to_string();
    let mut statement = conn.prepare(
        "SELECT CASE WHEN typeof(baseline_name)='text' AND octet_length(baseline_name) BETWEEN 1 AND 64
                     THEN baseline_name END,
                CASE WHEN typeof(baseline_digest)='text' AND octet_length(baseline_digest)=71
                     THEN baseline_digest END
         FROM main.change_evidence_attestations INDEXED BY sqlite_autoindex_change_evidence_attestations_1
         WHERE repo_scope COLLATE BINARY=?1 AND base COLLATE BINARY=?2
         ORDER BY baseline_name COLLATE BINARY, baseline_digest COLLATE BINARY LIMIT 33",
    ).map_err(sql_error)?;
    let mut rows = statement.query(params![scope, base]).map_err(sql_error)?;
    let mut items = Vec::new();
    let mut remaining_bytes = BASELINE_READ_CAP;
    for _ in 0..CANDIDATE_CAP {
        let Some(row) = rows.next().map_err(sql_error)? else {
            return Ok(CandidatePage {
                items,
                complete: true,
            });
        };
        let name = text(row, 0)?;
        let digest = text(row, 1)?;
        ExactPreparedKey::new(head, base, name, digest).map_err(|_| E::Corrupt)?;
        let baseline = match load_baseline(conn, guard, &scope, name, &mut remaining_bytes) {
            Ok(Some(Ok(baseline))) => baseline,
            Ok(_) => continue,
            Err(E::PayloadOverBudget) => {
                return Ok(CandidatePage {
                    items,
                    complete: false,
                });
            }
            Err(error) => return Err(error),
        };
        let identity = &baseline.set().identity;
        if baseline.digest() != digest
            || identity.index_head_hash.as_deref() != Some(base)
            || !identity.input_completeness.is_complete()
        {
            continue;
        }
        if let Some(observed_at) = read_attested_time(conn, &scope, base, name, digest, &baseline)?
        {
            items.push(BaselineCandidate {
                name: name.to_owned(),
                payload_digest: digest.to_owned(),
                base: base.to_owned(),
                observed_at,
            });
        }
    }
    Ok(CandidatePage {
        items,
        complete: rows.next().map_err(sql_error)?.is_none(),
    })
}

pub(crate) fn selection_in_tx(
    conn: &Connection,
    guard: &[u8; 32],
    scope: Uuid,
    key: &ExactPreparedKey<'_>,
) -> Result<SelectionMetadata> {
    let scope = scope.to_string();
    let absent = |verdict| Ok(SelectionMetadata::Unavailable(verdict));
    let mut remaining_bytes = BASELINE_READ_CAP;
    let baseline =
        match load_baseline(conn, guard, &scope, key.baseline_name, &mut remaining_bytes)? {
            Some(Ok(baseline)) => baseline,
            Some(Err(verdict)) => return absent(verdict),
            None => return absent(BaselineVerdict::BaselineMissing),
        };
    if baseline.digest() != key.baseline_digest {
        return absent(BaselineVerdict::BaselineDigestMismatch);
    }
    let identity = &baseline.set().identity;
    if identity.index_head_hash.as_deref() != Some(key.base) {
        return absent(BaselineVerdict::BaselineWrongParent);
    }
    if !identity.input_completeness.is_complete() {
        return absent(BaselineVerdict::BaselineIncompatible);
    }
    if read_attested_time(
        conn,
        &scope,
        key.base,
        key.baseline_name,
        key.baseline_digest,
        &baseline,
    )?
    .is_none()
    {
        return absent(BaselineVerdict::BaselineUnattested);
    }
    Ok(SelectionMetadata::Eligible)
}

/// The first bounded prepared key for this exact pair supplies independent Git
/// evidence when no checked selector record is available.
pub(crate) fn pair_git_key(
    conn: &Connection,
    scope: Uuid,
    head: &str,
    base: &str,
) -> Result<Option<(String, String)>> {
    use rusqlite::OptionalExtension;
    conn.query_row(
        "SELECT CASE WHEN typeof(baseline_name)='text' AND octet_length(baseline_name) BETWEEN 1 AND 64
                     THEN baseline_name END,
                CASE WHEN typeof(baseline_digest)='text' AND octet_length(baseline_digest)=71
                     THEN baseline_digest END
         FROM main.change_evidence_prepared INDEXED BY sqlite_autoindex_change_evidence_prepared_1
         WHERE repo_scope COLLATE BINARY=?1 AND head COLLATE BINARY=?2 AND base COLLATE BINARY=?3
         ORDER BY baseline_name COLLATE BINARY, baseline_digest COLLATE BINARY LIMIT 1",
        params![scope.to_string(), head, base],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
    .optional()
    .map_err(sql_error)
}
