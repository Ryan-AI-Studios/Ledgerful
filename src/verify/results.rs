use crate::state::StateError;
use crate::state::layout::Layout;
use chrono::Utc;
use miette::{IntoDiagnostic, Result};
use serde::{Deserialize, Serialize};
use std::fs;

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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerifyHistoryRecord {
    pub timestamp: String,
    pub passed: bool,
    pub duration_secs: u64,
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
        }
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

fn update_verify_history(layout: &Layout, report: &VerificationReport) -> Result<()> {
    let history_path = layout.reports_dir().join(VERIFY_HISTORY);
    let mut history: Vec<VerifyHistoryRecord> = if history_path.exists() {
        let content = fs::read_to_string(&history_path).into_diagnostic()?;
        serde_json::from_str(&content).unwrap_or_default()
    } else {
        Vec::new()
    };

    let total_duration_ms: u64 = report.results.iter().map(|r| r.duration_ms).sum();
    history.push(VerifyHistoryRecord {
        timestamp: report.timestamp.clone(),
        passed: report.overall_pass,
        duration_secs: total_duration_ms / 1000,
    });

    if history.len() > 100 {
        history.remove(0);
    }

    let json = serde_json::to_string_pretty(&history).into_diagnostic()?;
    fs::write(history_path, json).into_diagnostic()?;

    Ok(())
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
}
