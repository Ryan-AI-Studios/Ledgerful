//! PR scan machine-readable report for the GitHub Action surface (tracks 0047, 0086).
//!
//! This module produces a narrow, versioned, deterministic JSON schema that the
//! `ledgerful-action` wrapper pins. It intentionally does **not** perform the
//! full impact analysis: that path includes indexing, enrichment, and optional
//! LLM calls and is too heavy for a fast CI-runner report. Instead it reports
//! the git diff plus a lightweight, deterministic risk level, plus **index-free**
//! git history signals (schema v2): per-file churn, recency, and walk-window
//! honesty fields.
//!
//! ## Schema v2 (0086)
//!
//! - `schemaVersion` is `2`.
//! - Per change: `churn`, optional `lastCommitAt`, `isSensitive` (plus existing
//!   `oldPath` for renames).
//! - Report: `historyWindowCommits`, `historyTruncated`.
//! - **No author names** in the report — recency/churn only; naming a person in
//!   an automated public PR comment is a social cost with no analytic gain.
//! - Optional `headHash` / `branchName` omit when `None` (never serialize as
//!   `null`) so fail-closed Action validators accept detached-HEAD CI checkouts.
//! - `analysisWarnings` is **reserved**: the engine currently always emits `[]`.
//!   Callers must not treat a non-empty array as a stable contract until a real
//!   warning source is wired deliberately (Action marks the field reserved too).

use crate::git::metadata::{PathHistoryEntry, PathHistoryResult, lookup_path_history};
use crate::git::{ChangeType, FileChange};
use crate::impact::enrichment::test_gaps::TestGapsReport;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap};

/// Stable schema version for `PrScanReport`. Breaking changes bump this.
///
/// v1: base PR diff + risk. v2: + index-free history signals + per-change
/// `isSensitive` + null-omit for optional head identity fields.
pub const PR_SCAN_SCHEMA_VERSION: u32 = 2;

/// Risk level derived from lightweight, deterministic rules.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PrRiskLevel {
    Low,
    Medium,
    High,
}

impl std::fmt::Display for PrRiskLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PrRiskLevel::Low => write!(f, "low"),
            PrRiskLevel::Medium => write!(f, "medium"),
            PrRiskLevel::High => write!(f, "high"),
        }
    }
}

/// A single changed file in a PR diff (schema v2).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PrChange {
    /// Forward-slash-normalized path of the changed file.
    pub path: String,
    /// One of: added, modified, deleted, renamed.
    pub change_type: String,
    /// Present only when `change_type` is `renamed`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub old_path: Option<String>,
    /// Commits in the history walk window that touched this path (always emitted in v2).
    pub churn: u32,
    /// ISO-8601 committer time of the most recent touch in the window.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_commit_at: Option<String>,
    /// Whether this path matches a known sensitive pattern (always emitted in v2).
    pub is_sensitive: bool,
}

impl PrChange {
    fn from_file_change(change: &FileChange, history: &HistoryEnrichment) -> Self {
        let path = forward_slash_normalize(&change.path.to_string_lossy());
        let (change_type, old_path) = match &change.change_type {
            ChangeType::Added => ("added".to_string(), None),
            ChangeType::Modified => ("modified".to_string(), None),
            ChangeType::Deleted => ("deleted".to_string(), None),
            ChangeType::Renamed { old_path } => (
                "renamed".to_string(),
                Some(forward_slash_normalize(&old_path.to_string_lossy())),
            ),
        };
        let is_sensitive = is_sensitive_path(&path);
        let (churn, last_commit_at) = match history.lookup(&path) {
            Some(entry) => (entry.churn, Some(entry.last_commit_at.clone())),
            None => (0, None),
        };
        Self {
            path,
            change_type,
            old_path,
            churn,
            last_commit_at,
            is_sensitive,
        }
    }
}

/// Index-free history enrichment for [`PrScanReport`] (schema v2).
///
/// Built from [`crate::git::metadata::collect_path_history`]. Use
/// [`HistoryEnrichment::default`] / [`HistoryEnrichment::empty`] when a caller
/// only needs risk derivation (e.g. `policy check`) and should not pay for a
/// history walk.
///
/// **No author names** are carried here — see module docs.
#[derive(Debug, Clone, Default)]
pub struct HistoryEnrichment {
    by_path: HashMap<String, PathHistoryEntry>,
    /// How many commits were walked for this enrichment.
    pub history_window_commits: u32,
    /// Whether the walk hit the max-commit bound.
    pub history_truncated: bool,
}

impl HistoryEnrichment {
    /// Empty enrichment: zero window, no per-path signals, not truncated.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Build from a path-history walk result.
    pub fn from_path_history(result: PathHistoryResult) -> Self {
        Self {
            by_path: result.by_path,
            history_window_commits: result.history_window_commits,
            history_truncated: result.history_truncated,
        }
    }

    fn lookup(&self, path: &str) -> Option<&PathHistoryEntry> {
        lookup_path_history(&self.by_path, path)
    }
}

/// Narrow, versioned, deterministic report for `scan --pr --format json`.
///
/// The output is byte-identical for the same `(base_ref, head_hash, repo_state,
/// history window)` except for the volatile `generated_at` field. All
/// collections are sorted.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PrScanReport {
    pub schema_version: u32,
    pub generated_at: String,
    pub base_ref: String,
    pub head_ref: String,
    /// Full HEAD SHA when available. Omitted when unknown (never `null`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub head_hash: Option<String>,
    /// Branch name when available. Omitted on detached HEAD (never `null`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch_name: Option<String>,
    pub tree_clean: bool,
    pub change_count: u32,
    pub changes: Vec<PrChange>,
    pub risk_level: PrRiskLevel,
    pub risk_reasons: Vec<String>,
    /// **Reserved.** Always `[]` today; not a live warning channel. See module docs.
    pub analysis_warnings: Vec<String>,
    /// Commits walked for history enrichment (bounded).
    pub history_window_commits: u32,
    /// `true` if the history walk stopped at the max-commit bound.
    pub history_truncated: bool,
    /// Change-set structural test gaps (0115). Always present on schema v2;
    /// `status: unavailable` when no index DB (honest CI default).
    pub test_gaps: TestGapsReport,
}

/// Identity + cleanliness inputs for [`PrScanReport::new`].
#[derive(Debug, Clone)]
pub struct PrScanContext {
    pub base_ref: String,
    pub head_ref: String,
    pub head_hash: Option<String>,
    pub branch_name: Option<String>,
    pub tree_clean: bool,
}

impl PrScanReport {
    /// Build a deterministic PR scan report from the parsed git diff.
    ///
    /// `changes` are sorted by path. `risk_reasons` and `analysis_warnings` are
    /// sorted alphabetically. `generated_at` is set to the current UTC time.
    ///
    /// Pass [`HistoryEnrichment::empty`] when history signals are not needed
    /// (risk-only consumers). Full `scan --pr` should pass enrichment collected
    /// from the repository root via
    /// [`crate::git::metadata::collect_path_history`].
    pub fn new(
        ctx: PrScanContext,
        changes: &[FileChange],
        warnings: &[String],
        history: &HistoryEnrichment,
    ) -> Self {
        Self::new_with_test_gaps(
            ctx,
            changes,
            warnings,
            history,
            TestGapsReport::unavailable(),
        )
    }

    /// Build a report with an explicit test-gaps payload (production PR path).
    pub fn new_with_test_gaps(
        ctx: PrScanContext,
        changes: &[FileChange],
        warnings: &[String],
        history: &HistoryEnrichment,
        test_gaps: TestGapsReport,
    ) -> Self {
        let mut pr_changes: Vec<PrChange> = changes
            .iter()
            .map(|c| PrChange::from_file_change(c, history))
            .collect();
        pr_changes.sort_by(|a, b| a.path.cmp(&b.path));

        let change_count = pr_changes.len() as u32;
        let (risk_level, mut risk_reasons) = derive_risk(change_count, &pr_changes);

        // Reserved field: callers may pass warnings, but the PR CI path always
        // supplies `&[]` today. Sorted + deduped for determinism if that changes.
        let mut analysis_warnings: Vec<String> = warnings.to_vec();
        analysis_warnings = analysis_warnings
            .into_iter()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();

        // risk_reasons are sorted alphabetically for determinism.
        risk_reasons.sort();

        Self {
            schema_version: PR_SCAN_SCHEMA_VERSION,
            generated_at: Utc::now().to_rfc3339(),
            base_ref: ctx.base_ref,
            head_ref: ctx.head_ref,
            head_hash: ctx.head_hash,
            branch_name: ctx.branch_name,
            tree_clean: ctx.tree_clean,
            change_count,
            changes: pr_changes,
            risk_level,
            risk_reasons,
            analysis_warnings,
            history_window_commits: history.history_window_commits,
            history_truncated: history.history_truncated,
            test_gaps,
        }
    }
}

fn forward_slash_normalize(path: &str) -> String {
    path.replace('\\', "/")
}

/// Sensitive path patterns. A match bumps the risk level to `High`.
///
/// - File-name patterns (no trailing `/`) require an exact match of the last
///   path component. This prevents sub-string false positives such as
///   `crypto_utils.rs` matching `crypto.rs`.
/// - Directory-prefix patterns (trailing `/`) match when the forward-slash
///   normalized path starts with that prefix. Use these for paths containing a
///   `/` that should match any file under that directory (e.g. `.ledgerful/`
///   covers all ledgerful state files including `config.toml`).
const SENSITIVE_PATH_PATTERNS: &[&str] = &[
    "Cargo.toml",
    "Cargo.lock",
    ".github/workflows/",
    "crypto.rs",
    "migrations/",
    ".ledgerful/",
    "deny.toml",
    "SECURITY.md",
];

fn is_sensitive_path(path: &str) -> bool {
    let normalized = forward_slash_normalize(path);
    SENSITIVE_PATH_PATTERNS.iter().any(|pattern| {
        if pattern.ends_with('/') {
            normalized.starts_with(pattern)
        } else {
            std::path::Path::new(&normalized)
                .file_name()
                .is_some_and(|name| name.to_str() == Some(pattern))
        }
    })
}

fn derive_risk(change_count: u32, changes: &[PrChange]) -> (PrRiskLevel, Vec<String>) {
    let mut reasons: Vec<String> = Vec::new();
    let mut level = PrRiskLevel::Low;

    if change_count >= 10 {
        level = PrRiskLevel::Medium;
        reasons.push(format!("{} files changed (>= 10)", change_count));
    }

    for change in changes {
        if change.is_sensitive {
            level = PrRiskLevel::High;
            reasons.push(format!("sensitive path touched: {}", change.path));
        }
    }

    if change_count >= 30 {
        level = PrRiskLevel::High;
        reasons.push(format!("{} files changed (>= 30)", change_count));
    }

    (level, reasons)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn make_change(path: &str, change_type: ChangeType) -> FileChange {
        FileChange {
            path: PathBuf::from(path),
            change_type,
            is_staged: true,
        }
    }

    fn empty_history() -> HistoryEnrichment {
        HistoryEnrichment::empty()
    }

    fn report_with(
        changes: &[FileChange],
        head_hash: Option<String>,
        branch_name: Option<String>,
        warnings: &[String],
        history: &HistoryEnrichment,
    ) -> PrScanReport {
        PrScanReport::new(
            PrScanContext {
                base_ref: "main".into(),
                head_ref: "HEAD".into(),
                head_hash,
                branch_name,
                tree_clean: false,
            },
            changes,
            warnings,
            history,
        )
    }

    #[test]
    fn low_risk_for_small_change_set() {
        let changes = vec![make_change("src/lib.rs", ChangeType::Modified)];
        let report = report_with(
            &changes,
            Some("abc".into()),
            Some("feature".into()),
            &[],
            &empty_history(),
        );
        assert_eq!(report.risk_level, PrRiskLevel::Low);
        assert!(report.risk_reasons.is_empty());
        assert_eq!(report.schema_version, PR_SCAN_SCHEMA_VERSION);
        assert_eq!(report.schema_version, 2);
    }

    #[test]
    fn medium_risk_for_ten_or_more_changes() {
        let changes: Vec<FileChange> = (0..10)
            .map(|i| make_change(&format!("src/file{}.rs", i), ChangeType::Modified))
            .collect();
        let report = report_with(&changes, None, None, &[], &empty_history());
        assert_eq!(report.risk_level, PrRiskLevel::Medium);
        assert!(
            report
                .risk_reasons
                .iter()
                .any(|r| r.contains("10 files changed"))
        );
    }

    #[test]
    fn high_risk_for_sensitive_path() {
        let changes = vec![make_change("Cargo.toml", ChangeType::Modified)];
        let report = report_with(&changes, None, None, &[], &empty_history());
        assert_eq!(report.risk_level, PrRiskLevel::High);
        assert!(
            report
                .risk_reasons
                .iter()
                .any(|r| r.contains("sensitive path touched: Cargo.toml"))
        );
        assert!(report.changes[0].is_sensitive);
    }

    #[test]
    fn is_sensitive_flag_on_pr_change() {
        let changes = vec![
            make_change("Cargo.toml", ChangeType::Modified),
            make_change("src/lib.rs", ChangeType::Modified),
        ];
        let report = report_with(&changes, None, None, &[], &empty_history());
        let by_path: HashMap<_, _> = report
            .changes
            .iter()
            .map(|c| (c.path.as_str(), c.is_sensitive))
            .collect();
        assert_eq!(by_path.get("Cargo.toml"), Some(&true));
        assert_eq!(by_path.get("src/lib.rs"), Some(&false));
    }

    #[test]
    fn sensitive_path_matches_any_crypto_rs_file() {
        let changes = vec![make_change("src/crypto.rs", ChangeType::Modified)];
        let report = report_with(&changes, None, None, &[], &empty_history());
        assert_eq!(report.risk_level, PrRiskLevel::High);
        assert!(
            report
                .risk_reasons
                .iter()
                .any(|r| r.contains("sensitive path touched: src/crypto.rs"))
        );
        assert!(report.changes[0].is_sensitive);
    }

    #[test]
    fn similar_paths_do_not_match_sensitive_file_names() {
        let changes = vec![
            make_change("crypto_utils.rs", ChangeType::Modified),
            make_change("Cargo.toml.bak", ChangeType::Modified),
            make_change("SECURITY.md.bak", ChangeType::Modified),
            make_change("my_crypto.rs", ChangeType::Modified),
        ];
        let report = report_with(&changes, None, None, &[], &empty_history());
        assert_eq!(report.risk_level, PrRiskLevel::Low);
        assert!(report.risk_reasons.is_empty());
        assert!(report.changes.iter().all(|c| !c.is_sensitive));
    }

    #[test]
    fn directory_prefix_pattern_requires_full_prefix() {
        assert!(is_sensitive_path(".github/workflows/ci.yml"));
        assert!(!is_sensitive_path("my.github/workflows/ci.yml"));
        assert!(is_sensitive_path("migrations/001_init.sql"));
        assert!(!is_sensitive_path("not_migrations/001_init.sql"));
    }

    #[test]
    fn ledgerful_state_directory_is_sensitive() {
        assert!(is_sensitive_path(".ledgerful/config.toml"));
        assert!(is_sensitive_path(".ledgerful/state/ledger.db"));
        assert!(is_sensitive_path(".ledgerful/keys/private.key"));
        assert!(!is_sensitive_path("my-ledgerful/config.toml"));
    }

    #[test]
    fn high_risk_for_thirty_or_more_changes() {
        let changes: Vec<FileChange> = (0..30)
            .map(|i| make_change(&format!("src/file{}.rs", i), ChangeType::Modified))
            .collect();
        let report = report_with(&changes, None, None, &[], &empty_history());
        assert_eq!(report.risk_level, PrRiskLevel::High);
    }

    #[test]
    fn renamed_change_includes_old_path() {
        let changes = vec![FileChange {
            path: PathBuf::from("src/new.rs"),
            change_type: ChangeType::Renamed {
                old_path: PathBuf::from("src/old.rs"),
            },
            is_staged: true,
        }];
        let report = report_with(&changes, None, None, &[], &empty_history());
        assert_eq!(report.changes.len(), 1);
        let change = &report.changes[0];
        assert_eq!(change.change_type, "renamed");
        assert_eq!(change.old_path.as_deref(), Some("src/old.rs"));
    }

    #[test]
    fn changes_are_sorted_by_path() {
        let changes = vec![
            make_change("src/z.rs", ChangeType::Modified),
            make_change("src/a.rs", ChangeType::Added),
            make_change("src/m.rs", ChangeType::Deleted),
        ];
        let report = report_with(&changes, None, None, &[], &empty_history());
        let paths: Vec<&str> = report.changes.iter().map(|c| c.path.as_str()).collect();
        assert_eq!(paths, vec!["src/a.rs", "src/m.rs", "src/z.rs"]);
    }

    #[test]
    fn warnings_are_sorted_and_deduplicated() {
        let changes = vec![make_change("src/lib.rs", ChangeType::Modified)];
        let report = report_with(
            &changes,
            None,
            None,
            &["zzz".into(), "aaa".into(), "zzz".into()],
            &empty_history(),
        );
        assert_eq!(report.analysis_warnings, vec!["aaa", "zzz"]);
    }

    #[test]
    fn null_head_identity_omitted_from_json() {
        let changes = vec![make_change("src/lib.rs", ChangeType::Modified)];
        let report = report_with(&changes, None, None, &[], &empty_history());
        let json = serde_json::to_string(&report).expect("serialize");
        // Assert on parsed object keys — not a global "null" substring ban
        // (paths/reasons could legitimately contain that word).
        let value: serde_json::Value = serde_json::from_str(&json).expect("parse");
        let obj = value.as_object().expect("object");
        assert!(
            !obj.contains_key("headHash"),
            "headHash key must be omitted when None, got: {json}"
        );
        assert!(
            !obj.contains_key("branchName"),
            "branchName key must be omitted when None, got: {json}"
        );
        // Absent means absent, not null — if either key were present it would
        // fail the contains_key checks above (serde skip_serializing_if).
        assert!(
            !json.contains("\"headHash\":null") && !json.contains("\"branchName\":null"),
            "head identity must not serialize as JSON null: {json}"
        );
    }

    #[test]
    fn present_head_identity_emitted_in_json() {
        let changes = vec![make_change("src/lib.rs", ChangeType::Modified)];
        let report = report_with(
            &changes,
            Some("abc123".into()),
            Some("feature/x".into()),
            &[],
            &empty_history(),
        );
        let json = serde_json::to_string(&report).expect("serialize");
        assert!(json.contains("\"headHash\":\"abc123\""));
        assert!(json.contains("\"branchName\":\"feature/x\""));
    }

    #[test]
    fn enrichment_fields_applied_to_changes() {
        let mut by_path = HashMap::new();
        by_path.insert(
            "src/lib.rs".into(),
            PathHistoryEntry {
                churn: 7,
                last_commit_at: "2024-06-01T12:00:00+00:00".into(),
            },
        );
        let history = HistoryEnrichment {
            by_path,
            history_window_commits: 42,
            history_truncated: true,
        };
        let changes = vec![
            make_change("src/lib.rs", ChangeType::Modified),
            make_change("src/other.rs", ChangeType::Added),
        ];
        let report = report_with(&changes, None, None, &[], &history);
        assert_eq!(report.history_window_commits, 42);
        assert!(report.history_truncated);
        assert_eq!(report.changes[0].path, "src/lib.rs");
        assert_eq!(report.changes[0].churn, 7);
        assert_eq!(
            report.changes[0].last_commit_at.as_deref(),
            Some("2024-06-01T12:00:00+00:00")
        );
        assert_eq!(report.changes[1].path, "src/other.rs");
        assert_eq!(report.changes[1].churn, 0);
        assert!(report.changes[1].last_commit_at.is_none());

        let json = serde_json::to_string(&report).expect("serialize");
        assert!(json.contains("\"historyWindowCommits\":42"));
        assert!(json.contains("\"historyTruncated\":true"));
        assert!(json.contains("\"churn\":7"));
        assert!(json.contains("\"isSensitive\""));
        // No author names in the PR report path.
        assert!(!json.to_lowercase().contains("author"));
        assert!(!json.contains("lastContributor"));
        assert!(!json.contains("contributor"));
    }

    #[test]
    fn last_commit_at_omitted_when_none() {
        let changes = vec![make_change("src/lib.rs", ChangeType::Modified)];
        let report = report_with(&changes, None, None, &[], &empty_history());
        let json = serde_json::to_string(&report).expect("serialize");
        assert!(
            !json.contains("\"lastCommitAt\""),
            "lastCommitAt must be omitted when unknown: {json}"
        );
        assert!(json.contains("\"churn\":0"));
    }

    #[test]
    fn v2_report_emits_history_window_fields() {
        let changes = vec![make_change("src/lib.rs", ChangeType::Modified)];
        let report = report_with(&changes, None, None, &[], &empty_history());
        let value = serde_json::to_value(&report).expect("serialize");
        assert_eq!(value["schemaVersion"], 2);
        assert_eq!(value["historyWindowCommits"], 0);
        assert_eq!(value["historyTruncated"], false);
        assert_eq!(value["changes"][0]["churn"], 0);
        assert_eq!(value["changes"][0]["isSensitive"], false);
    }

    #[test]
    fn test_gaps_always_present_default_unavailable() {
        let changes = vec![make_change("src/lib.rs", ChangeType::Modified)];
        let report = report_with(&changes, None, None, &[], &empty_history());
        assert_eq!(report.schema_version, 2);
        assert_eq!(
            report.test_gaps.status,
            crate::impact::enrichment::test_gaps::TestGapsStatus::Unavailable
        );
        let value = serde_json::to_value(&report).expect("serialize");
        assert!(value.get("testGaps").is_some(), "testGaps must always emit");
        assert_eq!(value["testGaps"]["status"], "unavailable");
        assert!(value["testGaps"]["notes"].as_array().unwrap().len() >= 2);
    }

    #[test]
    fn test_gaps_available_payload_serializes_camel_case() {
        use crate::impact::enrichment::test_gaps::{
            LCOV_NOTE, MappedSampleEntry, STRUCTURAL_NOTE, TestGapsReport, TestGapsStatus,
            UnmappedGapEntry,
        };
        let gaps = TestGapsReport {
            status: TestGapsStatus::Available,
            source_seed_count: 2,
            mapped_count: 0,
            file_mapped_count: 1,
            unmapped_count: 1,
            unmapped_capped: false,
            unmapped_total: 1,
            unmapped: vec![UnmappedGapEntry {
                symbol: String::new(),
                file: "src/bare.rs".into(),
                qualified_name: None,
                mapping_kind: "none".into(),
            }],
            mapped_sample: vec![MappedSampleEntry {
                symbol: String::new(),
                file: "src/foo.rs".into(),
                covering_test_count: 1,
                mapping_kind: "file".into(),
            }],
            notes: vec![STRUCTURAL_NOTE.into(), LCOV_NOTE.into()],
        };
        let changes = vec![make_change("src/lib.rs", ChangeType::Modified)];
        let report = PrScanReport::new_with_test_gaps(
            PrScanContext {
                base_ref: "main".into(),
                head_ref: "HEAD".into(),
                head_hash: None,
                branch_name: None,
                tree_clean: false,
            },
            &changes,
            &[],
            &empty_history(),
            gaps,
        );
        let value = serde_json::to_value(&report).expect("serialize");
        assert_eq!(value["schemaVersion"], 2);
        assert_eq!(value["testGaps"]["status"], "available");
        assert_eq!(value["testGaps"]["unmappedCount"], 1);
        assert_eq!(value["testGaps"]["fileMappedCount"], 1);
        assert_eq!(value["testGaps"]["unmapped"][0]["file"], "src/bare.rs");
    }
}
