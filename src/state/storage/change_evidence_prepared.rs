//! Canonical, detached prepared evidence and exact-key storage reads.

use super::{
    E, PreparedBody, PreparedProjection, PreparedStatus, Result, WriteScope, content_digest,
    key_params, read_index_generation, seal,
};
use crate::change_evidence::{
    git::exact_oid,
    pack_store::changed_files::{ChangedFileCompleteness, ChangedFileKind, ChangedFiles},
    tree_index::is_evidence_path,
};
use crate::commands::baseline::{
    BaselineIdentity, Comparability, DeltaClass, DeltaCounts, DeltaItem, EdgeCoverage,
    EdgeSetDelta, FileEdgeSet, IncomparableReason, Regression,
    extract::capture_snapshot::IndexGeneration,
    identity::{BASELINE_SCHEMA, PAYLOAD_KIND},
    regression_for,
};
use chrono::{DateTime, Utc};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use super::super::{ExactPreparedKey, integer, sql_error, text};
const GIT_ROW_LIMIT: usize = 100;
const DELTA_ROW_LIMIT: usize = 100;
pub(crate) const MAX_COMPARE_EDGES: usize = 16_384;

#[derive(Serialize, Deserialize)]
#[cfg_attr(feature = "web", derive(utoipa::ToSchema))]
#[serde(rename_all = "lowercase")]
pub(crate) enum PreparedFileKind {
    Added,
    Deleted,
    Modified,
    Rewrite,
}

#[derive(Serialize, Deserialize)]
#[cfg_attr(feature = "web", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub(crate) struct PreparedFile {
    pub(crate) kind: PreparedFileKind,
    pub(crate) path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) old_path: Option<String>,
    pub(crate) additions: Option<u64>,
    pub(crate) deletions: Option<u64>,
    pub(crate) is_binary: Option<bool>,
}

#[derive(Clone, Copy, Serialize, Deserialize)]
#[cfg_attr(feature = "web", derive(utoipa::ToSchema))]
#[serde(rename_all = "kebab-case")]
pub(crate) enum GitPartialReason {
    UnsupportedTreePath,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "kebab-case")]
pub(crate) enum PreparedGit {
    Ready {
        files: Vec<PreparedFile>,
        total: usize,
        omitted: usize,
    },
    Partial {
        files: Vec<PreparedFile>,
        #[serde(skip_serializing_if = "Option::is_none")]
        total: Option<usize>,
        #[serde(skip_serializing_if = "Option::is_none")]
        omitted: Option<usize>,
        #[serde(
            default,
            rename = "reasonCode",
            skip_serializing_if = "Option::is_none"
        )]
        reason_code: Option<GitPartialReason>,
    },
    Unavailable,
}

impl PreparedGit {
    pub(crate) fn from_files(files: &ChangedFiles<'_>) -> Result<Self> {
        let rows: Vec<_> = files
            .files()
            .iter()
            .map(|file| PreparedFile {
                kind: match file.kind {
                    ChangedFileKind::Added => PreparedFileKind::Added,
                    ChangedFileKind::Deleted => PreparedFileKind::Deleted,
                    ChangedFileKind::Modified => PreparedFileKind::Modified,
                    ChangedFileKind::Rewrite => PreparedFileKind::Rewrite,
                },
                path: file.path().to_owned(),
                old_path: file.old_path().map(str::to_owned),
                additions: file.additions,
                deletions: file.deletions,
                is_binary: file.is_binary,
            })
            .collect();
        let sample = match files.completeness {
            ChangedFileCompleteness::Complete => Self::Ready {
                total: rows.len(),
                omitted: 0,
                files: rows,
            },
            ChangedFileCompleteness::Partial { paths_omitted, .. } => Self::Partial {
                files: rows,
                total: files.total,
                omitted: files.omitted,
                reason_code: paths_omitted.then_some(GitPartialReason::UnsupportedTreePath),
            },
        };
        sample.check()?;
        Ok(sample)
    }

    fn check(&self) -> Result<()> {
        let (rows, total, omitted) = match self {
            Self::Unavailable => return Ok(()),
            Self::Ready {
                files,
                total,
                omitted,
            } if *omitted == 0 => (files, Some(*total), Some(0)),
            Self::Ready { .. } => return Err(E::Corrupt),
            Self::Partial {
                files,
                total,
                omitted,
                ..
            } => (files, *total, *omitted),
        };
        if rows.len() > GIT_ROW_LIMIT
            || total
                .zip(omitted)
                .is_some_and(|(total, omitted)| rows.len().checked_add(omitted) != Some(total))
            || total.is_some() != omitted.is_some()
            || rows.iter().any(|row| {
                !is_evidence_path(&row.path)
                    || match (&row.kind, &row.old_path) {
                        (PreparedFileKind::Rewrite, Some(old)) => {
                            old == &row.path || !is_evidence_path(old)
                        }
                        (PreparedFileKind::Rewrite, None) | (_, Some(_)) => true,
                        (_, None) => false,
                    }
            })
        {
            return Err(E::Corrupt);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Serialize, Deserialize)]
#[cfg_attr(feature = "web", derive(utoipa::ToSchema))]
#[serde(rename_all = "kebab-case")]
pub(crate) enum PreparationRefusal {
    UnsupportedInputEncoding,
    UnsupportedTreePath,
    IndexGenerationMismatch,
    IndexParserVersionMismatch,
    IndexInputStampMissing,
    IndexInputStampMismatch,
    IndexInputHashMismatch,
    IndexInputUnavailable,
}

impl PreparationRefusal {
    pub(crate) fn code(self) -> &'static str {
        match self {
            Self::UnsupportedInputEncoding => "unsupported-input-encoding",
            Self::UnsupportedTreePath => "unsupported-tree-path",
            Self::IndexGenerationMismatch => "index-generation-mismatch",
            Self::IndexParserVersionMismatch => "index-parser-version-mismatch",
            Self::IndexInputStampMissing => "index-input-stamp-missing",
            Self::IndexInputStampMismatch => "index-input-stamp-mismatch",
            Self::IndexInputHashMismatch => "index-input-hash-mismatch",
            Self::IndexInputUnavailable => "index-input-unavailable",
        }
    }

    pub(crate) fn from_index_reason(reason: &'static str) -> Self {
        match reason {
            "unsupported-input-encoding" => Self::UnsupportedInputEncoding,
            "index-generation-mismatch" => Self::IndexGenerationMismatch,
            "index-parser-version-mismatch" => Self::IndexParserVersionMismatch,
            "index-input-stamp-missing" => Self::IndexInputStampMissing,
            "index-input-stamp-mismatch" => Self::IndexInputStampMismatch,
            "index-input-hash-mismatch" => Self::IndexInputHashMismatch,
            _ => Self::IndexInputUnavailable,
        }
    }
}

#[derive(Serialize, Deserialize)]
#[cfg_attr(feature = "web", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub(crate) struct ComparisonSample {
    pub(crate) complete: bool,
    pub(crate) omitted: usize,
}

#[derive(Serialize, Deserialize)]
#[cfg_attr(feature = "web", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub(crate) struct ComparableComparison {
    pub(crate) baseline_identity: BaselineIdentity,
    pub(crate) current_identity: BaselineIdentity,
    pub(crate) baseline_coverage: EdgeCoverage,
    pub(crate) current_coverage: EdgeCoverage,
    pub(crate) regression: Regression,
    pub(crate) regression_reasons: Vec<String>,
    pub(crate) notes: Vec<String>,
    pub(crate) counts: DeltaCounts,
    pub(crate) items: Vec<DeltaItem>,
    pub(crate) sample: ComparisonSample,
}

impl ComparableComparison {
    fn check_counts_and_regression(&self) -> Result<()> {
        let known = [
            self.counts.added,
            self.counts.removed,
            self.counts.worsened,
            self.counts.improved,
        ];
        let changed = known
            .iter()
            .try_fold(0_u64, |count, value| count.checked_add(*value));
        let total = changed.and_then(|changed| changed.checked_add(self.counts.unchanged));
        let sampled = self
            .items
            .len()
            .checked_add(self.sample.omitted)
            .and_then(|count| u64::try_from(count).ok());
        if total.is_none_or(|count| count > MAX_COMPARE_EDGES as u64) || changed != sampled {
            return Err(E::Corrupt);
        }
        let mut seen = [0; 4];
        for item in &self.items {
            let slot = match item.class {
                DeltaClass::Added => 0,
                DeltaClass::Removed => 1,
                DeltaClass::Worsened => 2,
                DeltaClass::Improved => 3,
                DeltaClass::Unchanged => return Err(E::Corrupt),
            };
            seen[slot] += 1;
        }
        if seen
            .into_iter()
            .zip(known)
            .any(|(seen, known)| seen > known)
            || regression_for(
                &self.counts,
                &self.baseline_coverage,
                &self.current_coverage,
            ) != (self.regression, self.regression_reasons.clone())
        {
            return Err(E::Corrupt);
        }
        Ok(())
    }
}

#[derive(Serialize, Deserialize)]
#[cfg_attr(feature = "web", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub(crate) struct IncomparableComparison {
    pub(crate) baseline_identity: BaselineIdentity,
    pub(crate) incomparable_reasons: Vec<IncomparableReason>,
    pub(crate) current_input_reasons: Vec<String>,
    pub(crate) notes: Vec<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(
    tag = "status",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase"
)]
pub(crate) enum PreparedComparison {
    Comparable(Box<ComparableComparison>),
    Incomparable(Box<IncomparableComparison>),
    Unavailable { reason_code: PreparationRefusal },
}

impl PreparedComparison {
    pub(crate) fn from_delta(
        baseline: &FileEdgeSet,
        current: &FileEdgeSet,
        delta: EdgeSetDelta,
    ) -> Result<Self> {
        Ok(match delta.status {
            Comparability::Comparable => {
                let counts = delta.counts.ok_or(E::Corrupt)?;
                let mut items = delta.items.ok_or(E::Corrupt)?;
                let mut bytes = 0;
                let retained = items
                    .iter()
                    .take(DELTA_ROW_LIMIT)
                    .take_while(|item| {
                        bytes += item.edge_key.src_path.len() + item.edge_key.dst_path.len();
                        bytes <= 90 * 1024
                    })
                    .count();
                let omitted = items.len() - retained;
                items.truncate(retained);
                Self::Comparable(Box::new(ComparableComparison {
                    baseline_identity: baseline.identity.clone(),
                    current_identity: current.identity.clone(),
                    baseline_coverage: baseline.coverage.clone(),
                    current_coverage: current.coverage.clone(),
                    regression: delta.regression,
                    regression_reasons: delta.regression_reasons,
                    notes: delta.notes,
                    counts,
                    items,
                    sample: ComparisonSample {
                        complete: omitted == 0,
                        omitted,
                    },
                }))
            }
            Comparability::Incomparable(mut reasons) => {
                if delta.counts.is_some() || delta.items.is_some() {
                    return Err(E::Corrupt);
                }
                reasons.sort_unstable_by_key(|reason| reason.token());
                reasons.dedup();
                Self::Incomparable(Box::new(IncomparableComparison {
                    baseline_identity: baseline.identity.clone(),
                    incomparable_reasons: reasons,
                    current_input_reasons: current.identity.input_completeness.reasons.clone(),
                    notes: delta.notes,
                }))
            }
        })
    }
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PreparedIndexMarker {
    pub(crate) index_head_hash: Option<String>,
    pub(crate) last_indexed_at: Option<String>,
    pub(crate) parser_version: Option<String>,
    pub(crate) config_fingerprint: String,
    pub(crate) discovery_fingerprint: String,
}

impl PreparedIndexMarker {
    pub(crate) fn from_generation(
        generation: &IndexGeneration,
        days: u64,
        retained_discovery: [u8; 32],
    ) -> Result<Self> {
        let marker = Self {
            index_head_hash: generation.head_hash.clone(),
            last_indexed_at: generation.last_indexed_at.clone(),
            parser_version: generation.parser_version.clone(),
            config_fingerprint: effective_config_fingerprint(days),
            discovery_fingerprint: format_discovery_fingerprint(retained_discovery),
        };
        marker.check()?;
        Ok(marker)
    }

    fn check(&self) -> Result<()> {
        if self
            .parser_version
            .as_deref()
            .is_some_and(|version| !short_token(version))
            || self
                .index_head_hash
                .as_deref()
                .is_some_and(|head| exact_oid(head.as_bytes()).is_err())
            || self
                .last_indexed_at
                .as_deref()
                .is_some_and(|time| time.len() > 64 || DateTime::parse_from_rfc3339(time).is_err())
            || !digest(&self.config_fingerprint)
            || !digest(&self.discovery_fingerprint)
        {
            return Err(E::Corrupt);
        }
        Ok(())
    }
}

pub(crate) fn format_discovery_fingerprint(witness: [u8; 32]) -> String {
    format!("blake3:{}", blake3::Hash::from_bytes(witness).to_hex())
}

pub(crate) fn effective_config_fingerprint(days: u64) -> String {
    let hash = blake3::derive_key(
        "ledgerful change evidence effective baseline config v1",
        &days.to_be_bytes(),
    );
    format!("blake3:{}", blake3::Hash::from_bytes(hash).to_hex())
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PreparedEvidence {
    pub(crate) git: PreparedGit,
    pub(crate) comparison: PreparedComparison,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) index_marker: Option<PreparedIndexMarker>,
}

impl seal::Sealed for PreparedEvidence {}

impl PreparedProjection for PreparedEvidence {
    fn status(&self) -> PreparedStatus {
        match self.comparison {
            PreparedComparison::Comparable(_) => PreparedStatus::Comparable,
            PreparedComparison::Incomparable(_) => PreparedStatus::Incomparable,
            PreparedComparison::Unavailable { .. } => PreparedStatus::Unavailable,
        }
    }

    fn has_git_evidence(&self) -> bool {
        !matches!(self.git, PreparedGit::Unavailable)
    }
}

impl PreparedEvidence {
    pub(crate) fn encode(&self, key: &ExactPreparedKey<'_>) -> Result<PreparedBody> {
        self.check(key)?;
        PreparedBody::encode(self)
    }

    fn check(&self, key: &ExactPreparedKey<'_>) -> Result<()> {
        self.git.check()?;
        match (&self.comparison, &self.index_marker) {
            (PreparedComparison::Comparable(value), Some(marker)) => {
                marker.check()?;
                check_identity(&value.baseline_identity, key.base)?;
                check_identity(&value.current_identity, key.head)?;
                if marker.index_head_hash.as_deref() != Some(key.head)
                    || marker.last_indexed_at.is_none()
                    || marker.parser_version.as_deref()
                        != Some(&value.current_identity.parser_version)
                    || value.items.len() > DELTA_ROW_LIMIT
                    || value.sample.complete != (value.sample.omitted == 0)
                    || value.items.iter().any(|item| {
                        !is_evidence_path(&item.edge_key.src_path)
                            || !is_evidence_path(&item.edge_key.dst_path)
                            || !valid_delta_row(item)
                    })
                    || value
                        .items
                        .windows(2)
                        .any(|rows| rows[0].edge_key >= rows[1].edge_key)
                    || !closed(
                        &value.regression_reasons,
                        &["AMBIGUOUS_GREW", "UNRESOLVED_GREW"],
                    )
                    || !closed(
                        &value.notes,
                        &["RESOLVED_EDGES_REMOVED", "analyzerBuildDiffers"],
                    )
                {
                    return Err(E::Corrupt);
                }
                value.check_counts_and_regression()?;
            }
            (PreparedComparison::Incomparable(value), Some(marker)) => {
                marker.check()?;
                check_identity(&value.baseline_identity, key.base)?;
                if value.incomparable_reasons.is_empty()
                    || value
                        .incomparable_reasons
                        .windows(2)
                        .any(|pair| pair[0].token() >= pair[1].token())
                    || !closed(
                        &value.current_input_reasons,
                        &[
                            "CAPPED_EDGES",
                            "CONTENT_DRIFT",
                            "INDEX_EMPTY",
                            "INDEX_INDETERMINATE",
                            "INDEX_STALE",
                            "PARSE_FAILED_FILES",
                            "SCAN_INCOMPLETE",
                        ],
                    )
                    || !closed(&value.notes, &["analyzerBuildDiffers"])
                {
                    return Err(E::Corrupt);
                }
            }
            (PreparedComparison::Unavailable { .. }, None) => {}
            _ => return Err(E::Corrupt),
        }
        Ok(())
    }
}

fn check_identity(identity: &BaselineIdentity, head: &str) -> Result<()> {
    if identity.index_head_hash.as_deref() != Some(head)
        || !identity.input_completeness.is_complete()
        || !identity.input_completeness.reasons.is_empty()
        || identity.baseline_schema != BASELINE_SCHEMA
        || identity.payload_kind != PAYLOAD_KIND
        || identity.scope.kind != "repo"
        || !short_token(&identity.parser_version)
        || !build_version(&identity.ledgerful_version)
        || !matches!(identity.edge_sources.as_slice(), [native] if native == "native")
            && !matches!(identity.edge_sources.as_slice(), [native, scip] if native == "native" && scip == "scip")
        || !digest(&identity.input_digest)
    {
        return Err(E::Corrupt);
    }
    Ok(())
}

fn closed(values: &[String], allowed: &[&str]) -> bool {
    values.len() <= allowed.len()
        && values.iter().all(|value| allowed.contains(&value.as_str()))
        && values.windows(2).all(|pair| pair[0] < pair[1])
}

fn build_version(value: &str) -> bool {
    match value.split_once(" (") {
        Some((version, hash)) => {
            short_token(version)
                && hash.strip_suffix(')').is_some_and(|hash| {
                    (4..=64).contains(&hash.len())
                        && hash
                            .bytes()
                            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                })
        }
        None => short_token(value),
    }
}

fn short_token(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b".+-_".contains(&b))
}

fn digest(value: &str) -> bool {
    value.strip_prefix("blake3:").is_some_and(|value| {
        value.len() == 64
            && value
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

fn valid_delta_row(row: &DeltaItem) -> bool {
    match (row.class, row.baseline_weight, row.current_weight) {
        (DeltaClass::Added, None, Some(new)) => new > 0,
        (DeltaClass::Removed, Some(old), None) => old > 0,
        (DeltaClass::Improved, Some(old), Some(new)) => old > new && new > 0,
        (DeltaClass::Worsened, Some(old), Some(new)) => new > old && old > 0,
        _ => false,
    }
}

/// The HTTP reader receives bounded typed data and exact SQL row markers.
pub(crate) struct PreparedRecord {
    pub(crate) generation: i64,
    pub(crate) captured_at: Option<DateTime<Utc>>,
    pub(crate) evidence: PreparedEvidence,
    payload_digest: String,
}

pub(crate) fn read_prepared_in_tx(
    conn: &Connection,
    ownership: WriteScope<'_>,
    key: &ExactPreparedKey<'_>,
) -> Result<Option<PreparedRecord>> {
    let id = ownership.verify(conn)?.ok_or(E::ScopeUnavailable)?;
    let scope = id.hyphenated().to_string();
    let mut statement = conn
        .prepare(
            "SELECT generation,payload_version,
            CASE WHEN typeof(payload_json)='text' AND octet_length(payload_json) BETWEEN 1 AND ?6
                 THEN payload_json END,
            CASE WHEN typeof(payload_digest)='text' AND octet_length(payload_digest)=71
                 THEN payload_digest END,
            CASE WHEN typeof(captured_at)='text' AND octet_length(captured_at) BETWEEN 1 AND 64
                 THEN captured_at END
        FROM change_evidence_prepared
        WHERE repo_scope=?1 AND head=?2 AND base=?3 AND baseline_name=?4 AND baseline_digest=?5",
        )
        .map_err(sql_error)?;
    let [repo_scope, head, base, name, digest] = key_params(&scope, key);
    let mut rows = statement
        .query(rusqlite::params![
            repo_scope,
            head,
            base,
            name,
            digest,
            super::PREPARED_CAP as i64
        ])
        .map_err(sql_error)?;
    let Some(row) = rows.next().map_err(sql_error)? else {
        return Ok(None);
    };
    let generation = integer(row, 0)?;
    let payload = text(row, 2)?;
    let payload_digest = text(row, 3)?.to_owned();
    if generation <= 0
        || integer(row, 1)? != super::VERSION
        || content_digest(payload) != payload_digest
    {
        return Err(E::Corrupt);
    }
    let evidence = serde_json::from_str::<PreparedEvidence>(payload).map_err(|_| E::Corrupt)?;
    let canonical = evidence.encode(key)?;
    if canonical.json != payload {
        return Err(E::Corrupt);
    }
    let captured_at = text(row, 4)
        .ok()
        .and_then(|raw| DateTime::parse_from_rfc3339(raw).ok())
        .map(|time| time.with_timezone(&Utc));
    if rows.next().map_err(sql_error)?.is_some() {
        return Err(E::Corrupt);
    }
    Ok(Some(PreparedRecord {
        generation,
        captured_at,
        evidence,
        payload_digest,
    }))
}

#[derive(Clone, Copy)]
pub(crate) enum PreparedRecheck {
    Current,
    Replaced,
    IndexChanged,
}

pub(crate) fn recheck_prepared_in_tx(
    conn: &Connection,
    ownership: WriteScope<'_>,
    key: &ExactPreparedKey<'_>,
    earlier: &PreparedRecord,
) -> Result<PreparedRecheck> {
    let id = ownership.verify(conn)?.ok_or(E::ScopeUnavailable)?;
    let scope = id.hyphenated().to_string();
    let stamp = super::prepared_stamp(conn, &scope, key)?;
    if !stamp.is_some_and(|stamp| {
        stamp.generation == earlier.generation
            && stamp.digest == earlier.payload_digest
            && Some(stamp.observed) == earlier.captured_at
    }) {
        return Ok(PreparedRecheck::Replaced);
    }
    if let Some(marker) = &earlier.evidence.index_marker {
        let generation = read_index_generation(conn)?;
        if generation.head_hash != marker.index_head_hash
            || generation.last_indexed_at != marker.last_indexed_at
            || generation.parser_version != marker.parser_version
        {
            return Ok(PreparedRecheck::IndexChanged);
        }
    }
    Ok(PreparedRecheck::Current)
}

#[cfg(test)]
#[path = "change_evidence_prepared_tests.rs"]
mod tests;
