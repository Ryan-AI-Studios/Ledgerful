//! Bounded SQL inputs for an opt-in capture. Filesystem completeness is separate.
use super::super::identity::{EdgeCoverage, FileEdge, input_digest};
use super::{has_capped_edges, read_coverage, read_edge_sources, read_edges};
use crate::index::staleness::is_supported_code_path;
use rusqlite::{Row, Transaction, types::ValueRef};

const MAX_PATH_BYTES: usize = 4096;
const MAX_SMALL_FIELD_BYTES: usize = 256;
const MAX_EVIDENCE_BYTES: usize = 4096;
const MAX_METADATA_VALUE_BYTES: usize = 4096;

#[derive(Clone, Copy)]
pub(crate) struct CaptureSnapshotLimits {
    pub(crate) active_files: usize,
    pub(crate) scanned_files: usize,
    pub(crate) scanned_edges: usize,
    pub(crate) scanned_metadata: usize,
    pub(crate) text_bytes: usize,
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum SnapshotBound {
    ActiveFiles,
    ScannedFiles,
    ScannedEdges,
    ScannedMetadata,
    TextBytes,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum CaptureSnapshotError {
    #[error("baseline snapshot limit")]
    Limit(SnapshotBound),
    #[error("malformed baseline snapshot input")]
    Corrupt(&'static str),
    #[error("baseline snapshot allocation")]
    Allocation,
    #[error("baseline snapshot SQL read failed")]
    Sql(#[from] rusqlite::Error),
    #[error("baseline snapshot graph read failed")]
    Graph(miette::Report),
}

type Result<T> = std::result::Result<T, CaptureSnapshotError>;
type DigestRow = (String, Option<String>, String);

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct IndexGeneration {
    pub(crate) head_hash: Option<String>,
    pub(crate) last_indexed_at: Option<String>,
    pub(crate) parser_version: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FileStamp {
    file_size: Option<i64>,
    mtime_ns: Option<i64>,
}

pub(crate) struct IndexInputWitness<'a> {
    pub(crate) path: &'a str,
    pub(crate) content_hash: &'a str,
    pub(crate) parser_version: &'a str,
    pub(crate) file_size: Option<i64>,
    pub(crate) mtime_ns: Option<i64>,
}

/// Owned SQL facts from one caller-owned read transaction. Contains no
/// filesystem completeness verdict or Git authorization.
pub(crate) struct CaptureSnapshot {
    pub(crate) generation: IndexGeneration,
    pub(crate) coverage: EdgeCoverage,
    pub(crate) edges: Vec<FileEdge>,
    pub(crate) edge_sources: Vec<String>,
    pub(crate) edges_capped: bool,
    pub(crate) input_digest: String,
    digest_rows: Vec<DigestRow>,
    stamps: Vec<FileStamp>,
}

impl CaptureSnapshot {
    #[cfg(any(windows, target_os = "linux"))]
    pub(crate) fn finish_proven(
        self,
        proof: &crate::change_evidence::pack_store::paired_checkout::PairedDefaultIndexedHead,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<(IndexGeneration, super::FileEdgeSet)> {
        self.confirm_inputs(
            proof.head(),
            proof.verified_witnesses(),
            proof.input_digest(),
        )?;
        Ok(self.build_set(proof.stale_threshold_days(), false, now))
    }

    #[cfg(any(windows, target_os = "linux"))]
    pub(crate) fn finish_prepared(
        self,
        proof: &crate::change_evidence::pack_store::paired_checkout::PairedPrepareProof,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<(IndexGeneration, super::FileEdgeSet)> {
        if let Some((rows, digest)) = proof.verified_input() {
            self.confirm_inputs(proof.head(), rows, digest)?;
        } else if !proof.content_drift() {
            return Err(CaptureSnapshotError::Corrupt("proof-snapshot-mismatch"));
        }
        let result = self.build_set(proof.stale_threshold_days(), proof.content_drift(), now);
        if proof.content_drift() && result.1.identity.input_completeness.is_complete() {
            return Err(CaptureSnapshotError::Corrupt("proof-snapshot-mismatch"));
        }
        Ok(result)
    }

    #[cfg(any(windows, target_os = "linux"))]
    fn confirm_inputs(&self, head: gix::ObjectId, rows: usize, digest: blake3::Hash) -> Result<()> {
        if self.generation.head_hash.as_deref() != Some(&head.to_string())
            || self.witnesses().len() != rows
            || self.input_digest.strip_prefix("blake3:") != Some(digest.to_hex().as_str())
        {
            return Err(CaptureSnapshotError::Corrupt("proof-snapshot-mismatch"));
        }
        Ok(())
    }

    #[cfg(any(windows, target_os = "linux"))]
    fn build_set(
        self,
        stale_threshold_days: u64,
        content_drift: bool,
        now: chrono::DateTime<chrono::Utc>,
    ) -> (IndexGeneration, super::FileEdgeSet) {
        use crate::index::staleness::{
            IndexFreshnessState, age_with_index_timestamp, state_with_content_drift,
        };

        let age = self
            .generation
            .last_indexed_at
            .as_deref()
            .and_then(|time| chrono::DateTime::parse_from_rfc3339(time).ok())
            .and_then(|time| {
                age_with_index_timestamp(
                    self.coverage.files_indexed > 0,
                    time.with_timezone(&chrono::Utc),
                    now,
                    stale_threshold_days,
                )
            })
            .map_or(IndexFreshnessState::Indeterminate, |(state, _)| state);
        let completeness = super::finish_completeness(
            state_with_content_drift(age, content_drift),
            &super::CompletenessInputs {
                files_parse_failed: self.coverage.files_parse_failed,
                edges_capped: self.edges_capped,
                parser_version_present: self.generation.parser_version.is_some(),
            },
            Vec::new(),
        );
        let set = super::snapshot_edge_set(
            super::SnapshotIdentity {
                parser_version: self.generation.parser_version.clone(),
                index_head_hash: self.generation.head_hash.clone(),
                edge_sources: self.edge_sources,
                input_digest: self.input_digest,
            },
            self.coverage,
            self.edges,
            completeness,
        );
        (self.generation, set)
    }

    pub(crate) fn recompute_input_digest(&self) -> String {
        input_digest(&self.digest_rows)
    }

    pub(crate) fn witnesses(&self) -> impl ExactSizeIterator<Item = IndexInputWitness<'_>> {
        self.digest_rows
            .iter()
            .zip(&self.stamps)
            .map(|((path, hash, parser_version), stamp)| IndexInputWitness {
                path,
                content_hash: hash.as_deref().expect("admitted capture hash"),
                parser_version,
                file_size: stamp.file_size,
                mtime_ns: stamp.mtime_ns,
            })
    }
}

/// Read from the caller's schema-qualified transaction on the conventional
/// baseline store. Admission and legacy SQL helpers use that exact snapshot.
pub(crate) fn extract_capture_snapshot(
    transaction: &Transaction<'_>,
    limits: CaptureSnapshotLimits,
) -> Result<CaptureSnapshot> {
    let encoding: String = transaction.query_row("PRAGMA encoding", [], |row| row.get(0))?;
    if encoding != "UTF-8" {
        return Err(CaptureSnapshotError::Corrupt("sqlite-encoding"));
    }
    let mut bytes = TextAllowance(limits.text_bytes);
    let files = admit_files(transaction, limits, &mut bytes)?;
    let edge_rows = admit_edges(transaction, limits.scanned_edges, &mut bytes)?;
    admit_metadata(transaction, limits.scanned_metadata, &mut bytes)?;
    // The landed unique file IDs allow no more than one grouping key pair per
    // physical edge. Cover both the grouping paths and the returned owned paths.
    let edge_paths = edge_rows
        .checked_mul(4 * files.max_path_bytes)
        .ok_or(CaptureSnapshotError::Limit(SnapshotBound::TextBytes))?;
    bytes.charge(edge_paths)?;
    let (digest_rows, stamps) = read_witnesses(transaction, files.active)?;
    let generation = read_generation(transaction)?;
    let edges = read_edges(transaction).map_err(CaptureSnapshotError::Graph)?;
    let coverage = read_coverage(transaction, &edges).map_err(CaptureSnapshotError::Graph)?;
    let edge_sources = read_edge_sources(transaction).map_err(CaptureSnapshotError::Graph)?;
    let edges_capped = has_capped_edges(transaction).map_err(CaptureSnapshotError::Graph)?;
    let input_digest = input_digest(&digest_rows);
    Ok(CaptureSnapshot {
        generation,
        coverage,
        edges,
        edge_sources,
        edges_capped,
        input_digest,
        digest_rows,
        stamps,
    })
}

struct FileAdmission {
    active: usize,
    max_path_bytes: usize,
}

struct TextAllowance(usize);

impl TextAllowance {
    fn charge(&mut self, bytes: usize) -> Result<()> {
        self.0 = self
            .0
            .checked_sub(bytes)
            .ok_or(CaptureSnapshotError::Limit(SnapshotBound::TextBytes))?;
        Ok(())
    }
}

fn probe_limit(cap: usize, bound: SnapshotBound) -> Result<i64> {
    cap.checked_add(1)
        .and_then(|value| i64::try_from(value).ok())
        .ok_or(CaptureSnapshotError::Limit(bound))
}

fn charge_text_column(
    kind: ValueRef<'_>,
    length: Option<i64>,
    nullable: bool,
    ceiling: usize,
    field: &'static str,
    allowance: &mut TextAllowance,
) -> Result<usize> {
    if matches!(kind, ValueRef::Text(b"null")) && nullable {
        return Ok(0);
    }
    if !matches!(kind, ValueRef::Text(b"text")) {
        return Err(CaptureSnapshotError::Corrupt(field));
    }
    let len = length
        .and_then(|len| usize::try_from(len).ok())
        .ok_or(CaptureSnapshotError::Corrupt(field))?;
    if len > ceiling {
        return Err(CaptureSnapshotError::Limit(SnapshotBound::TextBytes));
    }
    allowance.charge(len)?;
    Ok(len)
}

fn bounded_status<'a>(row: &'a Row<'_>, field: &'static str) -> Result<&'a str> {
    row.get_ref(0)?
        .as_str()
        .map_err(|_| CaptureSnapshotError::Corrupt(field))
}

fn admit_files(
    transaction: &Transaction<'_>,
    limits: CaptureSnapshotLimits,
    allowance: &mut TextAllowance,
) -> Result<FileAdmission> {
    let mut statement = transaction.prepare(
        "SELECT CASE WHEN octet_length(parse_status) <= 32 THEN parse_status END,
                typeof(file_path), octet_length(file_path),
                typeof(content_hash), octet_length(content_hash),
                typeof(parser_version), octet_length(parser_version),
                typeof(file_size), typeof(mtime_ns)
         FROM project_files NOT INDEXED LIMIT ?1",
    )?;
    let mut rows = statement.query([probe_limit(
        limits.scanned_files,
        SnapshotBound::ScannedFiles,
    )?])?;
    let mut physical = 0;
    let mut result = FileAdmission {
        active: 0,
        max_path_bytes: 0,
    };
    while let Some(row) = rows.next()? {
        physical += 1;
        if physical > limits.scanned_files {
            return Err(CaptureSnapshotError::Limit(SnapshotBound::ScannedFiles));
        }
        let status = bounded_status(row, "file-status")?;
        allowance.charge(status.len())?;
        match status {
            "DELETED" => continue,
            "OK" | "PARSE_FAILED" => {}
            _ => return Err(CaptureSnapshotError::Corrupt("file-status")),
        }
        result.active += 1;
        if result.active > limits.active_files {
            return Err(CaptureSnapshotError::Limit(SnapshotBound::ActiveFiles));
        }
        let path_bytes = charge_text_column(
            row.get_ref(1)?,
            row.get(2)?,
            false,
            MAX_PATH_BYTES,
            "path",
            allowance,
        )?;
        result.max_path_bytes = result.max_path_bytes.max(path_bytes);
        charge_text_column(
            row.get_ref(3)?,
            row.get(4)?,
            true,
            MAX_SMALL_FIELD_BYTES,
            "content-hash",
            allowance,
        )?;
        charge_text_column(
            row.get_ref(5)?,
            row.get(6)?,
            false,
            MAX_SMALL_FIELD_BYTES,
            "parser-version",
            allowance,
        )?;
        for (index, field) in [(7, "file-size"), (8, "mtime")] {
            if !matches!(row.get_ref(index)?, ValueRef::Text(b"integer" | b"null")) {
                return Err(CaptureSnapshotError::Corrupt(field));
            }
        }
    }
    Ok(result)
}

fn admit_edges(
    transaction: &Transaction<'_>,
    limit: usize,
    allowance: &mut TextAllowance,
) -> Result<usize> {
    let mut statement = transaction.prepare(
        "SELECT CASE WHEN octet_length(resolution_status) <= 32 THEN resolution_status END,
                typeof(evidence), octet_length(evidence),
                typeof(caller_file_id), typeof(callee_file_id),
                CASE WHEN octet_length(evidence) <= ?2 THEN evidence END
         FROM structural_edges NOT INDEXED LIMIT ?1",
    )?;
    let mut rows = statement.query([
        probe_limit(limit, SnapshotBound::ScannedEdges)?,
        MAX_EVIDENCE_BYTES as i64,
    ])?;
    let mut count = 0;
    while let Some(row) = rows.next()? {
        count += 1;
        if count > limit {
            return Err(CaptureSnapshotError::Limit(SnapshotBound::ScannedEdges));
        }
        let status = bounded_status(row, "edge-status")?;
        if !matches!(status, "RESOLVED" | "UNRESOLVED" | "AMBIGUOUS" | "CAPPED") {
            return Err(CaptureSnapshotError::Corrupt("edge-status"));
        }
        allowance.charge(status.len())?;
        charge_text_column(
            row.get_ref(1)?,
            row.get(2)?,
            true,
            MAX_EVIDENCE_BYTES,
            "edge-evidence",
            allowance,
        )?;
        if !matches!(row.get_ref(5)?, ValueRef::Null) {
            row.get_ref(5)?
                .as_str()
                .map_err(|_| CaptureSnapshotError::Corrupt("edge-evidence"))?;
        }
        if !matches!(row.get_ref(3)?, ValueRef::Text(b"integer"))
            || !matches!(row.get_ref(4)?, ValueRef::Text(b"integer" | b"null"))
        {
            return Err(CaptureSnapshotError::Corrupt("edge-file-id"));
        }
    }
    Ok(count)
}

fn admit_metadata(
    transaction: &Transaction<'_>,
    limit: usize,
    allowance: &mut TextAllowance,
) -> Result<()> {
    let mut statement = transaction.prepare(
        "SELECT typeof(key), octet_length(key), typeof(value), octet_length(value),
                CASE WHEN octet_length(key) <= ?2 THEN key END,
                CASE WHEN octet_length(value) <= ?3 THEN value END
         FROM index_metadata NOT INDEXED LIMIT ?1",
    )?;
    let mut rows = statement.query([
        probe_limit(limit, SnapshotBound::ScannedMetadata)?,
        MAX_SMALL_FIELD_BYTES as i64,
        MAX_METADATA_VALUE_BYTES as i64,
    ])?;
    let mut count = 0;
    while let Some(row) = rows.next()? {
        count += 1;
        if count > limit {
            return Err(CaptureSnapshotError::Limit(SnapshotBound::ScannedMetadata));
        }
        charge_text_column(
            row.get_ref(0)?,
            row.get(1)?,
            false,
            MAX_SMALL_FIELD_BYTES,
            "metadata-key",
            allowance,
        )?;
        charge_text_column(
            row.get_ref(2)?,
            row.get(3)?,
            false,
            MAX_METADATA_VALUE_BYTES,
            "metadata-value",
            allowance,
        )?;
        for (index, field) in [(4, "metadata-key"), (5, "metadata-value")] {
            row.get_ref(index)?
                .as_str()
                .map_err(|_| CaptureSnapshotError::Corrupt(field))?;
        }
    }
    Ok(())
}

fn read_witnesses(
    transaction: &Transaction<'_>,
    active: usize,
) -> Result<(Vec<DigestRow>, Vec<FileStamp>)> {
    let mut statement = transaction.prepare(
        "SELECT file_path, content_hash, parser_version, file_size, mtime_ns
         FROM project_files WHERE parse_status != 'DELETED' LIMIT ?1",
    )?;
    let mut rows = statement.query([probe_limit(active, SnapshotBound::ActiveFiles)?])?;
    let mut inputs = Vec::new();
    let mut stamps = Vec::new();
    inputs
        .try_reserve_exact(active)
        .map_err(|_| CaptureSnapshotError::Allocation)?;
    stamps
        .try_reserve_exact(active)
        .map_err(|_| CaptureSnapshotError::Allocation)?;
    let mut count = 0;
    while let Some(row) = rows.next()? {
        count += 1;
        if count > active {
            return Err(CaptureSnapshotError::Limit(SnapshotBound::ActiveFiles));
        }
        let path: String = row.get(0)?;
        let hash: Option<String> = row.get(1)?;
        let version: String = row.get(2)?;
        if !digest_field(&path) {
            return Err(CaptureSnapshotError::Corrupt("path"));
        }
        if !is_supported_code_path(&path) {
            continue;
        }
        if !hash.as_deref().is_some_and(|hash| lowercase_hex(hash, 64)) {
            return Err(CaptureSnapshotError::Corrupt("content-hash"));
        }
        if !digest_field(&version) {
            return Err(CaptureSnapshotError::Corrupt("parser-version"));
        }
        let stamp = FileStamp {
            file_size: row.get(3)?,
            mtime_ns: row.get(4)?,
        };
        if stamp.file_size.is_some_and(|size| size < 0)
            || stamp.mtime_ns.is_some_and(|mtime| mtime < 0)
        {
            return Err(CaptureSnapshotError::Corrupt("file-stamp"));
        }
        inputs.push((path, hash, version));
        stamps.push(stamp);
    }
    if count != active {
        return Err(CaptureSnapshotError::Corrupt("active-file-count"));
    }
    Ok((inputs, stamps))
}

pub(crate) fn read_generation(conn: &rusqlite::Connection) -> Result<IndexGeneration> {
    use rusqlite::OptionalExtension;
    let mut statement = conn.prepare(
        "SELECT CASE WHEN typeof(value)='text' AND octet_length(value) BETWEEN 1 AND ?2
                     THEN value END
         FROM index_metadata WHERE key COLLATE BINARY=?1",
    )?;
    let mut read = |key: &str, cap| -> Result<Option<String>> {
        statement
            .query_row((key, cap), |row| row.get(0))
            .optional()
            .map_err(Into::into)
    };
    let head_hash = read("head_hash", 40)?;
    let last_indexed_at = read("last_indexed_at", 64)?;
    let parser_version = read("parser_version", MAX_SMALL_FIELD_BYTES as i64)?;
    if head_hash
        .as_deref()
        .is_some_and(|head| !lowercase_hex(head, 40))
    {
        return Err(CaptureSnapshotError::Corrupt("generation-head"));
    }
    if last_indexed_at
        .as_deref()
        .is_some_and(|stamp| chrono::DateTime::parse_from_rfc3339(stamp).is_err())
    {
        return Err(CaptureSnapshotError::Corrupt("generation-time"));
    }
    if parser_version
        .as_deref()
        .is_some_and(|version| !digest_field(version))
    {
        return Err(CaptureSnapshotError::Corrupt("generation-parser"));
    }
    Ok(IndexGeneration {
        head_hash,
        last_indexed_at,
        parser_version,
    })
}

fn lowercase_hex(value: &str, bytes: usize) -> bool {
    value.len() == bytes
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn digest_field(value: &str) -> bool {
    !value.is_empty() && !value.as_bytes().iter().any(|byte| byte.is_ascii_control())
}
