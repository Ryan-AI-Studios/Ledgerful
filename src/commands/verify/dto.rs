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

#[cfg(test)]
mod verify_cli_json_tests {
    use super::*;
    use crate::verify::plan::{PlanSource, VerificationPlan};
    use crate::verify::results::VerificationReport;

    use super::super::test_support::{sample_report, sample_report_with_refused, sample_result};

    #[test]
    fn schema_version_and_ok_from_report() {
        let report = sample_report(vec![sample_result("cargo test", 0)], None);
        let payload = VerifyCliJson::from_report(&report, VerifyScope::Full, None);
        assert_eq!(payload.schema_version, VERIFY_JSON_SCHEMA_VERSION);
        assert!(payload.ok);
        assert_eq!(payload.scope_requested, "full");
        assert_eq!(payload.scope_executed, "full");
        assert!(payload.fallback_reason.is_none());
        assert_eq!(payload.steps.len(), 1);
        assert_eq!(payload.steps[0].status, "pass");
        assert_eq!(payload.steps[0].exit_code, 0);
        assert!(payload.steps[0].failure_detail.is_none());
        assert!(payload.matched_steps.is_none());
    }

    #[test]
    fn fallback_sets_scope_executed_full() {
        let report = sample_report(
            vec![sample_result("cargo test", 0)],
            Some("fast scope unavailable — empty test_mapping; running full (~5-8 min)"),
        );
        let payload = VerifyCliJson::from_report(&report, VerifyScope::Fast, None);
        assert_eq!(payload.scope_requested, "fast");
        assert_eq!(payload.scope_executed, "full");
        assert!(payload.fallback_reason.as_ref().unwrap().contains("empty"));
    }

    #[test]
    fn refused_sets_scope_executed_refused_ok_false_empty_steps() {
        let report = sample_report_with_refused(
            vec![],
            Some(
                "fast scope unavailable — test_mapping is stale or empty; refusing full suite (~5-8 min)",
            ),
            true,
        );
        // Vacuous overall_pass on empty results would be true without the force.
        assert!(!report.overall_pass || report.results.is_empty());
        let payload = VerifyCliJson::from_report(&report, VerifyScope::Fast, None);
        assert!(!payload.ok, "refused must never report ok:true");
        assert_eq!(payload.scope_requested, "fast");
        assert_eq!(payload.scope_executed, "refused");
        assert!(payload.steps.is_empty());
        assert!(
            payload
                .fallback_reason
                .as_ref()
                .unwrap()
                .contains("refusing full suite")
        );
    }

    #[test]
    fn refused_guards_vacuous_overall_pass_in_from_report() {
        // Even if overall_pass is left true on empty results, from_report forces ok:false.
        let plan = VerificationPlan {
            source: Some(PlanSource::AutoPolicy),
            steps: vec![],
            fallback_reason: Some(
                "fast scope unavailable — no mappings; refusing full suite (~5-8 min)".into(),
            ),
            refused: true,
        };
        let mut report = VerificationReport::new(Some(plan), vec![]);
        report.timestamp = "2026-01-01T00:00:00Z".into();
        // Vacuous .all() on empty results is true — the bug 0135 guards against.
        assert!(report.overall_pass);
        let payload = VerifyCliJson::from_report(&report, VerifyScope::Fast, None);
        assert!(!payload.ok);
        assert_eq!(payload.scope_executed, "refused");
        assert!(payload.steps.is_empty());
    }

    #[test]
    fn ok_false_when_step_fails_and_status_agrees() {
        let report = sample_report(
            vec![
                sample_result("cargo fmt", 0),
                sample_result("cargo clippy", 1),
            ],
            None,
        );
        let payload = VerifyCliJson::from_report(&report, VerifyScope::Fast, None);
        assert!(!payload.ok);
        assert_eq!(payload.steps[0].status, "pass");
        assert_eq!(payload.steps[1].status, "fail");
        assert_eq!(payload.steps[1].failure_detail.as_deref(), Some("boom"));
        // DoD-10: ok == all steps pass == (would be exit 0)
        assert_eq!(payload.ok, payload.steps.iter().all(|s| s.exit_code == 0));
    }

    #[test]
    fn failed_paths_additive_on_fmt_fail_schema_v1() {
        let mut result = sample_result("cargo fmt --all -- --check", 1);
        result.stderr_summary = "Diff in src/lib.rs:\nDiff in src/main.rs:\n".into();
        let report = sample_report(vec![result], None);
        let payload = VerifyCliJson::from_report(&report, VerifyScope::Fast, None);
        assert_eq!(payload.schema_version, 1);
        assert!(!payload.ok);
        let paths = payload.steps[0].failed_paths.as_ref().expect("paths");
        assert_eq!(
            paths,
            &vec!["src/lib.rs".to_string(), "src/main.rs".to_string()]
        );
        let json = payload.to_json_string().unwrap();
        assert!(json.contains("\"failedPaths\""));
        assert!(json.contains("src/lib.rs"));
        // Pass steps omit the field.
        let pass = VerifyCliJson::from_report(
            &sample_report(vec![sample_result("cargo test", 0)], None),
            VerifyScope::Full,
            None,
        );
        assert!(pass.steps[0].failed_paths.is_none());
        assert!(!pass.to_json_string().unwrap().contains("failedPaths"));
    }

    #[test]
    fn matched_steps_additive_schema_v1() {
        let report = sample_report(vec![sample_result("cargo test", 0)], None);
        let with = VerifyCliJson::from_report(&report, VerifyScope::Full, Some(2));
        assert_eq!(with.schema_version, 1);
        assert_eq!(with.matched_steps, Some(2));
        let json = with.to_json_string().unwrap();
        assert!(json.contains("\"matchedSteps\": 2"));
        let without = VerifyCliJson::from_report(&report, VerifyScope::Full, None);
        assert!(without.matched_steps.is_none());
        assert!(!without.to_json_string().unwrap().contains("matchedSteps"));
        // Vacuous honesty: matched_steps=0 still serializes (ordering ran, zero hits).
        let zero = VerifyCliJson::from_report(&report, VerifyScope::Full, Some(0));
        assert_eq!(zero.matched_steps, Some(0));
        assert!(
            zero.to_json_string()
                .unwrap()
                .contains("\"matchedSteps\": 0")
        );
    }

    #[test]
    fn non_code_cheap_json_scope_executed_fast_no_fallback() {
        // DoD-6: NonCodeCheap (fallback_reason=None, refused=false, fmt+clippy)
        // must keep scopeExecuted fast. schemaVersion stays 1.
        let report = sample_report(
            vec![
                sample_result("cargo fmt --all -- --check", 0),
                sample_result(
                    "cargo clippy --all-targets --all-features -- -D warnings",
                    0,
                ),
            ],
            None,
        );
        let payload = VerifyCliJson::from_report(&report, VerifyScope::Fast, None);
        assert_eq!(payload.schema_version, 1);
        assert_eq!(payload.schema_version, VERIFY_JSON_SCHEMA_VERSION);
        assert_eq!(payload.scope_requested, "fast");
        assert_eq!(payload.scope_executed, "fast");
        assert!(payload.fallback_reason.is_none());
        let json = payload.to_json_string().unwrap();
        assert!(
            !json.contains("fallbackReason"),
            "skip_serializing_if must omit fallbackReason: {json}"
        );
        assert!(json.contains("\"schemaVersion\": 1"));
        assert!(payload.ok);
    }

    #[test]
    fn format_fail_block_header_from_report() {
        let mut result = sample_result("cargo fmt --all -- --check", 1);
        result.stderr_summary = "Diff in a.rs:\n".into();
        let report = sample_report(vec![result], None);
        let block =
            crate::verify::fail_block::format_fail_block_from_report(&report).expect("fail block");
        assert!(block.starts_with("[Ledgerful] verify failed\n"));
        assert!(block.contains("exitCode: 1"));
        assert!(block.contains("failedPaths: a.rs"));
    }

    #[test]
    fn byte_identical_across_two_builds() {
        let report = sample_report(
            vec![
                sample_result("cargo fmt --all -- --check", 0),
                sample_result("cargo nextest run --lib", 0),
            ],
            None,
        );
        let a = VerifyCliJson::from_report(&report, VerifyScope::Fast, None)
            .to_json_string()
            .unwrap();
        let b = VerifyCliJson::from_report(&report, VerifyScope::Fast, None)
            .to_json_string()
            .unwrap();
        assert_eq!(a, b);
        let parsed: VerifyCliJson = serde_json::from_str(&a).unwrap();
        assert_eq!(parsed.schema_version, 1);
        assert!(parsed.ok);
    }

    #[test]
    fn step_status_from_exit_code_matrix() {
        assert_eq!(step_status_from_exit_code(0), "pass");
        assert_eq!(step_status_from_exit_code(1), "fail");
        assert_eq!(step_status_from_exit_code(3), "fail");
    }

    #[test]
    fn ok_matches_sig_exit_matrix_for_gate() {
        // Process exit 0 ↔ ok true; non-zero validation rejection ↔ ok false.
        for (exit, expect_ok) in [(0, true), (1, false), (3, false)] {
            let results = if exit == 0 {
                vec![sample_result("cargo test", 0)]
            } else {
                vec![sample_result("cargo test", exit)]
            };
            let report = sample_report(results, None);
            let payload = VerifyCliJson::from_report(&report, VerifyScope::Full, None);
            assert_eq!(payload.ok, expect_ok, "exit={exit}");
            assert_eq!(payload.ok, exit == 0);
        }
    }
}
