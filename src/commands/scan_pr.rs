//! PR scan machine-readable report for the GitHub Action surface (track 0047).
//!
//! This module produces a narrow, versioned, deterministic JSON schema that the
//! `ledgerful-action` wrapper pins. It intentionally does **not** perform the
//! full impact analysis: that path includes indexing, enrichment, and optional
//! LLM calls and is too heavy for a fast CI-runner report. Instead it reports
//! the git diff plus a lightweight, deterministic risk level.

use crate::git::{ChangeType, FileChange};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// Stable schema version for `PrScanReport`. Breaking changes bump this.
pub const PR_SCAN_SCHEMA_VERSION: u32 = 1;

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

/// A single changed file in a PR diff.
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
}

impl PrChange {
    fn from_file_change(change: &FileChange) -> Self {
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
        Self {
            path,
            change_type,
            old_path,
        }
    }
}

/// Narrow, versioned, deterministic report for `scan --pr --format json`.
///
/// The output is byte-identical for the same `(base_ref, head_hash, repo_state)`
/// except for the volatile `generated_at` field. All collections are sorted.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PrScanReport {
    pub schema_version: u32,
    pub generated_at: String,
    pub base_ref: String,
    pub head_ref: String,
    pub head_hash: Option<String>,
    pub branch_name: Option<String>,
    pub tree_clean: bool,
    pub change_count: u32,
    pub changes: Vec<PrChange>,
    pub risk_level: PrRiskLevel,
    pub risk_reasons: Vec<String>,
    pub analysis_warnings: Vec<String>,
}

impl PrScanReport {
    /// Build a deterministic PR scan report from the parsed git diff.
    ///
    /// `changes` are sorted by path. `risk_reasons` and `analysis_warnings` are
    /// sorted alphabetically. `generated_at` is set to the current UTC time.
    pub fn new(
        base_ref: String,
        head_ref: String,
        head_hash: Option<String>,
        branch_name: Option<String>,
        tree_clean: bool,
        changes: &[FileChange],
        warnings: &[String],
    ) -> Self {
        let mut pr_changes: Vec<PrChange> =
            changes.iter().map(PrChange::from_file_change).collect();
        pr_changes.sort_by(|a, b| a.path.cmp(&b.path));

        let change_count = pr_changes.len() as u32;
        let (risk_level, mut risk_reasons) = derive_risk(change_count, &pr_changes);

        let mut analysis_warnings: Vec<String> = warnings.to_vec();
        analysis_warnings.sort();
        // Deduplicate while preserving deterministic order.
        analysis_warnings = analysis_warnings
            .into_iter()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        analysis_warnings.sort();

        // risk_reasons are sorted alphabetically for determinism.
        risk_reasons.sort();

        Self {
            schema_version: PR_SCAN_SCHEMA_VERSION,
            generated_at: Utc::now().to_rfc3339(),
            base_ref,
            head_ref,
            head_hash,
            branch_name,
            tree_clean,
            change_count,
            changes: pr_changes,
            risk_level,
            risk_reasons,
            analysis_warnings,
        }
    }
}

fn forward_slash_normalize(path: &str) -> String {
    path.replace('\\', "/")
}

/// Sensitive path patterns. A match bumps the risk level to `High`.
const SENSITIVE_PATH_PATTERNS: &[&str] = &[
    "Cargo.toml",
    "Cargo.lock",
    ".github/workflows/",
    "crypto.rs",
    "src/crypto.rs",
    "migrations/",
    ".ledgerful/config.toml",
    "deny.toml",
    "SECURITY.md",
];

fn is_sensitive_path(path: &str) -> bool {
    let normalized = forward_slash_normalize(path);
    SENSITIVE_PATH_PATTERNS
        .iter()
        .any(|pattern| normalized.contains(pattern))
}

fn derive_risk(change_count: u32, changes: &[PrChange]) -> (PrRiskLevel, Vec<String>) {
    let mut reasons: Vec<String> = Vec::new();
    let mut level = PrRiskLevel::Low;

    if change_count >= 10 {
        level = PrRiskLevel::Medium;
        reasons.push(format!("{} files changed (>= 10)", change_count));
    }

    for change in changes {
        if is_sensitive_path(&change.path) {
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

    #[test]
    fn low_risk_for_small_change_set() {
        let changes = vec![make_change("src/lib.rs", ChangeType::Modified)];
        let report = PrScanReport::new(
            "main".into(),
            "HEAD".into(),
            Some("abc".into()),
            Some("feature".into()),
            false,
            &changes,
            &[],
        );
        assert_eq!(report.risk_level, PrRiskLevel::Low);
        assert!(report.risk_reasons.is_empty());
        assert_eq!(report.schema_version, PR_SCAN_SCHEMA_VERSION);
    }

    #[test]
    fn medium_risk_for_ten_or_more_changes() {
        let changes: Vec<FileChange> = (0..10)
            .map(|i| make_change(&format!("src/file{}.rs", i), ChangeType::Modified))
            .collect();
        let report = PrScanReport::new(
            "main".into(),
            "HEAD".into(),
            None,
            None,
            false,
            &changes,
            &[],
        );
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
        let report = PrScanReport::new(
            "main".into(),
            "HEAD".into(),
            None,
            None,
            false,
            &changes,
            &[],
        );
        assert_eq!(report.risk_level, PrRiskLevel::High);
        assert!(
            report
                .risk_reasons
                .iter()
                .any(|r| r.contains("sensitive path touched: Cargo.toml"))
        );
    }

    #[test]
    fn high_risk_for_thirty_or_more_changes() {
        let changes: Vec<FileChange> = (0..30)
            .map(|i| make_change(&format!("src/file{}.rs", i), ChangeType::Modified))
            .collect();
        let report = PrScanReport::new(
            "main".into(),
            "HEAD".into(),
            None,
            None,
            false,
            &changes,
            &[],
        );
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
        let report = PrScanReport::new(
            "main".into(),
            "HEAD".into(),
            None,
            None,
            false,
            &changes,
            &[],
        );
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
        let report = PrScanReport::new(
            "main".into(),
            "HEAD".into(),
            None,
            None,
            false,
            &changes,
            &[],
        );
        let paths: Vec<&str> = report.changes.iter().map(|c| c.path.as_str()).collect();
        assert_eq!(paths, vec!["src/a.rs", "src/m.rs", "src/z.rs"]);
    }

    #[test]
    fn warnings_are_sorted_and_deduplicated() {
        let changes = vec![make_change("src/lib.rs", ChangeType::Modified)];
        let report = PrScanReport::new(
            "main".into(),
            "HEAD".into(),
            None,
            None,
            false,
            &changes,
            &["zzz".into(), "aaa".into(), "zzz".into()],
        );
        assert_eq!(report.analysis_warnings, vec!["aaa", "zzz"]);
    }
}
