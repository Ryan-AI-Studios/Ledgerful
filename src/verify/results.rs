use crate::git::repo::{get_head_info, open_repo};
use crate::git::status::get_repo_status;
use crate::state::StateError;
use crate::state::layout::Layout;
use chrono::Utc;
use miette::{IntoDiagnostic, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

use super::plan::VerificationPlan;

pub const LATEST_VERIFY_REPORT: &str = "latest-verify.json";
pub const VERIFY_HISTORY: &str = "verify-history.json";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct VerificationResult {
    pub command: String,
    pub exit_code: i32,
    pub duration_ms: u64,
    pub stdout_summary: String,
    pub stderr_summary: String,
    pub truncated: bool,
    pub timestamp: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct VerificationReport {
    pub plan: Option<VerificationPlan>,
    pub results: Vec<VerificationResult>,
    #[serde(default)]
    pub prediction_warnings: Vec<String>,
    #[serde(default)]
    pub suggested_actions: Vec<crate::verify::suggestions::Suggestion>,
    pub overall_pass: bool,
    pub timestamp: String,
    #[serde(default)]
    pub tx_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerifyHistoryRecord {
    pub timestamp: String,
    pub passed: bool,
    pub duration_secs: u64,
    #[serde(default)]
    pub tx_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head: Option<String>,
}

impl VerificationReport {
    pub fn new(plan: Option<VerificationPlan>, results: Vec<VerificationResult>) -> Self {
        let overall_pass = results.iter().all(|result| result.exit_code == 0);
        Self {
            plan,
            results,
            prediction_warnings: Vec::new(),
            suggested_actions: Vec::new(),
            overall_pass,
            timestamp: Utc::now().to_rfc3339(),
            tx_id: None,
        }
    }

    pub fn with_tx_id(mut self, tx_id: Option<String>) -> Self {
        self.tx_id = tx_id;
        self
    }

    pub fn with_warnings(mut self, warnings: Vec<String>) -> Self {
        self.prediction_warnings = warnings;
        self
    }

    pub fn with_suggested_actions(
        mut self,
        suggestions: Vec<crate::verify::suggestions::Suggestion>,
    ) -> Self {
        self.suggested_actions = suggestions;
        self
    }
}

pub fn write_verify_report(layout: &Layout, report: &VerificationReport) -> Result<()> {
    layout.ensure_state_dir()?;
    let report_path = layout.reports_dir().join(LATEST_VERIFY_REPORT);
    let json = serde_json::to_string_pretty(report)
        .map_err(std::io::Error::other)
        .map_err(|e| StateError::WriteReportFailed {
            path: report_path.to_string(),
            source: e,
        })?;

    fs::write(&report_path, json).map_err(|e| StateError::WriteReportFailed {
        path: report_path.to_string(),
        source: e,
    })?;

    // Update history
    update_verify_history(layout, report)?;

    Ok(())
}

/// Decode on-disk `VERIFY_HISTORY` JSON. Parse failure is an error — callers
/// must not treat it as an empty list (that looks like "none" and invites a wipe).
pub fn parse_verify_history(content: &str) -> Result<Vec<VerifyHistoryRecord>, serde_json::Error> {
    serde_json::from_str(content)
}

fn update_verify_history(layout: &Layout, report: &VerificationReport) -> Result<()> {
    let history_path = layout.reports_dir().join(VERIFY_HISTORY);
    let mut history: Vec<VerifyHistoryRecord> = if history_path.exists() {
        let content = fs::read_to_string(&history_path).into_diagnostic()?;
        match parse_verify_history(&content) {
            Ok(h) => h,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    path = %history_path,
                    "failed to parse verify history JSON; skipping update to avoid wiping the file"
                );
                return Ok(());
            }
        }
    } else {
        Vec::new()
    };

    let total_duration_ms: u64 = report.results.iter().map(|r| r.duration_ms).sum();
    history.push(VerifyHistoryRecord {
        timestamp: report.timestamp.clone(),
        passed: report.overall_pass,
        duration_secs: total_duration_ms / 1000,
        tx_id: report.tx_id.clone(),
        head: resolve_verify_history_head(layout),
    });

    if history.len() > 100 {
        history.remove(0);
    }

    let json = serde_json::to_string_pretty(&history).into_diagnostic()?;
    fs::write(history_path, json).into_diagnostic()?;

    Ok(())
}

/// Bind only a porcelain-clean worktree HEAD. Omit when dirty, not a git
/// repo, or HEAD is unresolvable so a dirty/pre-commit verify cannot look
/// like proof of the last commit.
fn resolve_verify_history_head(layout: &Layout) -> Option<String> {
    let root = layout.root.as_std_path();
    let repo = open_repo(root).ok()?;
    let changes = get_repo_status(&repo).ok()?;
    if product_tree_dirty(layout, &changes) {
        return None;
    }
    let (hash, _) = get_head_info(&repo).ok()?;
    hash.filter(|h| !h.is_empty())
        .map(|h| h.to_ascii_lowercase())
}

fn product_tree_dirty(layout: &Layout, changes: &[crate::git::FileChange]) -> bool {
    changes
        .iter()
        .any(|change| !path_is_state_dir(layout, &change.path))
}

fn path_is_state_dir(layout: &Layout, path: &Path) -> bool {
    let state = layout.state_dir.as_std_path();
    path.starts_with(state)
        || path
            .components()
            .any(|c| c.as_os_str() == crate::state::layout::STATE_DIR)
}

#[cfg(test)]
mod tests {
    use super::*;
    use camino::Utf8Path;
    use tempfile::tempdir;

    #[test]
    fn test_write_verify_report() {
        let tmp = tempdir().unwrap();
        let root = Utf8Path::from_path(tmp.path()).unwrap();
        let layout = Layout::new(root);
        let report = VerificationReport::new(
            None,
            vec![VerificationResult {
                command: "cargo test".to_string(),
                exit_code: 0,
                duration_ms: 123,
                stdout_summary: "ok".to_string(),
                stderr_summary: String::new(),
                truncated: false,
                timestamp: "2026-01-01T00:00:00Z".to_string(),
            }],
        );

        write_verify_report(&layout, &report).unwrap();
        let saved = fs::read_to_string(layout.reports_dir().join(LATEST_VERIFY_REPORT)).unwrap();
        let loaded: VerificationReport = serde_json::from_str(&saved).unwrap();
        assert!(loaded.overall_pass);
        assert_eq!(loaded.results[0].command, "cargo test");
    }

    #[test]
    fn verify_history_corrupt_parse_does_not_wipe_file() {
        let tmp = tempdir().unwrap();
        let root = Utf8Path::from_path(tmp.path()).unwrap();
        let layout = Layout::new(root);
        layout.ensure_state_dir().unwrap();
        let history_path = layout.reports_dir().join(VERIFY_HISTORY);
        let corrupt = "{not-valid-verify-history";
        fs::write(&history_path, corrupt).unwrap();

        let report = VerificationReport::new(
            None,
            vec![VerificationResult {
                command: "cargo test".to_string(),
                exit_code: 0,
                duration_ms: 1,
                stdout_summary: "ok".to_string(),
                stderr_summary: String::new(),
                truncated: false,
                timestamp: "2026-01-01T00:00:00Z".to_string(),
            }],
        );

        write_verify_report(&layout, &report).unwrap();

        let after = fs::read_to_string(&history_path).unwrap();
        assert_eq!(
            after, corrupt,
            "corrupt verify-history.json must not be replaced with []+new row"
        );
    }

    fn sample_report() -> VerificationReport {
        VerificationReport::new(
            None,
            vec![VerificationResult {
                command: "cargo test".to_string(),
                exit_code: 0,
                duration_ms: 40,
                stdout_summary: "ok".to_string(),
                stderr_summary: String::new(),
                truncated: false,
                timestamp: "2026-09-11T00:00:00Z".to_string(),
            }],
        )
    }

    fn git(dir: &std::path::Path, args: &[&str]) -> std::process::Output {
        std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.com")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.com")
            .output()
            .expect("git")
    }

    fn init_clean_git(dir: &std::path::Path) {
        assert!(git(dir, &["init", "-b", "main"]).status.success());
        git(dir, &["config", "user.email", "t@example.com"]);
        git(dir, &["config", "user.name", "t"]);
        fs::write(dir.join("README.md"), "one\n").unwrap();
        assert!(git(dir, &["add", "README.md"]).status.success());
        assert!(git(dir, &["commit", "-m", "first"]).status.success());
    }

    #[test]
    fn verify_history_write_includes_head_when_resolvable() {
        let tmp = tempdir().unwrap();
        init_clean_git(tmp.path());
        let root = Utf8Path::from_path(tmp.path()).unwrap();
        let layout = Layout::new(root);
        let old = r#"[{"timestamp":"2026-01-01T00:00:00Z","passed":true,"duration_secs":1,"tx_id":"old"}]"#;
        layout.ensure_state_dir().unwrap();
        fs::write(layout.reports_dir().join(VERIFY_HISTORY), old).unwrap();
        let parsed = parse_verify_history(old).expect("old row without head still parses");
        assert_eq!(parsed.len(), 1);
        assert!(parsed[0].head.is_none());

        write_verify_report(&layout, &sample_report()).unwrap();
        let after = fs::read_to_string(layout.reports_dir().join(VERIFY_HISTORY)).unwrap();
        let recs = parse_verify_history(&after).expect("history");
        let newest = recs.last().expect("new row");
        let head = newest.head.as_deref().expect("clean repo writes head");
        assert_eq!(head.len(), 40, "{head}");
        assert!(
            head.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
            "{head}"
        );
        let expected = String::from_utf8_lossy(&git(tmp.path(), &["rev-parse", "HEAD"]).stdout)
            .trim()
            .to_ascii_lowercase();
        assert_eq!(head, expected);
    }

    #[test]
    fn verify_history_write_omits_head_when_dirty() {
        let tmp = tempdir().unwrap();
        init_clean_git(tmp.path());
        fs::write(tmp.path().join("dirty.rs"), "fn x() {}\n").unwrap();
        let root = Utf8Path::from_path(tmp.path()).unwrap();
        let layout = Layout::new(root);
        write_verify_report(&layout, &sample_report()).unwrap();
        let after = fs::read_to_string(layout.reports_dir().join(VERIFY_HISTORY)).unwrap();
        let recs = parse_verify_history(&after).expect("history");
        assert_eq!(recs.len(), 1);
        assert!(recs[0].head.is_none(), "{after}");
    }
}
