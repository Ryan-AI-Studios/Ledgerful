use crate::verify::plan::VerifyScope;
use crate::verify::results::{VerificationReport, VerificationResult};
use miette::{IntoDiagnostic, Result};
use serde::{Deserialize, Serialize};

/// Stable schema version for `verify --json` CLI wire contract (track 0093).
/// Distinct from the persisted `VerificationReport` / `latest-verify.json`.
pub const VERIFY_JSON_SCHEMA_VERSION: u32 = 1;

/// Versioned machine-readable payload for `ledgerful verify --json`.
/// Built *from* `VerificationReport` — does not extend the persisted report.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct VerifyCliJson {
    pub schema_version: u32,
    pub ok: bool,
    pub scope_requested: String,
    pub scope_executed: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fallback_reason: Option<String>,
    /// Plan order (probability-ordered), not sorted alphabetically.
    pub steps: Vec<VerifyCliStepJson>,
    pub timestamp: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tx_id: Option<String>,
    /// Count of plan steps whose step key was present in the Bayesian
    /// probability map (0140). Omitted when ordering was not attempted.
    /// schemaVersion stays **1** (additive optional field).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matched_steps: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct VerifyCliStepJson {
    pub name: String,
    pub command: String,
    /// `"pass"` when `exitCode == 0`, else `"fail"`. Defined once so it cannot
    /// disagree with `ok`.
    pub status: String,
    pub exit_code: i32,
    pub duration_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure_detail: Option<String>,
    /// Best-effort formatter paths (0121). Omitted when empty / pass.
    /// `schemaVersion` stays **1** (additive optional field).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failed_paths: Option<Vec<String>>,
}

/// Derive step status from exit code — single definition for DoD-10.
pub fn step_status_from_exit_code(exit_code: i32) -> &'static str {
    if exit_code == 0 { "pass" } else { "fail" }
}

impl VerifyCliJson {
    /// Build the CLI wire payload from a completed `VerificationReport`.
    ///
    /// Three-way `scope_executed` (0135):
    /// - `plan.refused` → `"refused"`
    /// - else `fallback_reason.is_some()` → `"full"`
    /// - else requested scope string
    ///
    /// `ok` is false when refused (vacuous-pass guard: empty results would
    /// otherwise make `overall_pass` true).
    ///
    /// `matched_steps` is the Bayesian apply hit count (0140); pass `None`
    /// when ordering was not attempted (no history / cold-start / no storage).
    pub fn from_report(
        report: &VerificationReport,
        scope_requested: VerifyScope,
        matched_steps: Option<usize>,
    ) -> Self {
        let plan = report.plan.as_ref();
        let refused = plan.is_some_and(|p| p.refused);
        let fallback_reason = plan.and_then(|p| p.fallback_reason.clone());
        let scope_executed = if refused {
            "refused".to_string()
        } else if fallback_reason.is_some() {
            "full".to_string()
        } else {
            scope_requested.to_string()
        };

        let steps: Vec<VerifyCliStepJson> = if refused {
            Vec::new()
        } else {
            report
                .results
                .iter()
                .map(VerifyCliStepJson::from_result)
                .collect()
        };

        Self {
            schema_version: VERIFY_JSON_SCHEMA_VERSION,
            // Belt-and-suspenders: refuse must never report ok:true even if
            // results is empty (vacuous `.all()` is true).
            ok: report.overall_pass && !refused,
            scope_requested: scope_requested.to_string(),
            scope_executed,
            fallback_reason,
            steps,
            timestamp: report.timestamp.clone(),
            tx_id: report.tx_id.clone(),
            matched_steps,
        }
    }

    /// Serialize deterministically for agent consumers (pretty JSON).
    pub fn to_json_string(&self) -> Result<String> {
        serde_json::to_string_pretty(self).into_diagnostic()
    }
}

impl VerifyCliStepJson {
    pub fn from_result(result: &VerificationResult) -> Self {
        let status = step_status_from_exit_code(result.exit_code).to_string();
        let failure_detail = if result.exit_code != 0 {
            Some(crate::verify::fail_block::failure_detail_from_result(
                result,
            ))
        } else {
            None
        };
        let failed_paths = if result.exit_code != 0 {
            let paths = crate::verify::fail_block::extract_formatter_paths(
                &result.command,
                &result.stdout_summary,
                &result.stderr_summary,
            );
            if paths.is_empty() { None } else { Some(paths) }
        } else {
            None
        };
        // Prefer description-like name from command; command is the full string.
        let name = crate::verify::fail_block::step_name_from_command(&result.command);
        Self {
            name,
            command: result.command.clone(),
            status,
            exit_code: result.exit_code,
            duration_ms: result.duration_ms,
            failure_detail,
            failed_paths,
        }
    }
}
