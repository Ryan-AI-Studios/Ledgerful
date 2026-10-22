//! Shared verify unit-test fixtures (0251).
//!
//! Dto schema tests and execute tests both import from here so dto does not
//! depend on execute and the fixtures are not duplicated.

use crate::verify::assurance::{
    AssuranceInputs, ExecutionAssuranceJson, build_execution_assurance,
};
use crate::verify::plan::{PlanSource, VerificationPlan, VerificationStep};
use crate::verify::results::{VerificationReport, VerificationResult};

pub(crate) fn sample_result(command: &str, exit: i32) -> VerificationResult {
    VerificationResult {
        command: command.to_string(),
        exit_code: exit,
        duration_ms: 10,
        stdout_summary: if exit == 0 {
            "ok".into()
        } else {
            String::new()
        },
        stderr_summary: if exit != 0 {
            "boom".into()
        } else {
            String::new()
        },
        truncated: false,
        timestamp: "2026-01-01T00:00:00Z".into(),
    }
}

pub(crate) fn sample_report(
    results: Vec<VerificationResult>,
    fallback: Option<&str>,
) -> VerificationReport {
    sample_report_with_refused(results, fallback, false)
}

pub(crate) fn sample_report_with_refused(
    results: Vec<VerificationResult>,
    fallback: Option<&str>,
    refused: bool,
) -> VerificationReport {
    let plan = VerificationPlan {
        source: Some(PlanSource::AutoPolicy),
        steps: results.iter().map(|r| test_step(&r.command)).collect(),
        fallback_reason: fallback.map(|s| s.to_string()),
        refused,
    };
    let mut report = VerificationReport::new(Some(plan), results);
    // Stabilize timestamp for byte-identity.
    report.timestamp = "2026-01-01T00:00:00Z".into();
    report.tx_id = Some("tx-abc".into());
    if refused {
        report.overall_pass = false;
    }
    report
}

/// Test-only assurance for a sample report. Normal tests still call
/// `VerifyCliJson::from_report` with this required context.
pub(crate) fn fixture_assurance(report: &VerificationReport) -> ExecutionAssuranceJson {
    let empty = Vec::new();
    let steps = report
        .plan
        .as_ref()
        .map(|plan| plan.steps.as_slice())
        .unwrap_or(empty.as_slice());
    let plan_source = report.plan.as_ref().and_then(|plan| plan.source.clone());
    let refused = report.plan.as_ref().is_some_and(|plan| plan.refused);
    build_execution_assurance(AssuranceInputs {
        planned_steps: steps,
        results: &report.results,
        plan_source,
        manual_requested: false,
        preview: false,
        refused,
    })
}

/// Test-only step builder for dto/execute fixtures (not production `manual_step`).
pub(crate) fn test_step(command: &str) -> VerificationStep {
    VerificationStep {
        command: command.to_string(),
        timeout_secs: 60,
        description: "step".into(),
        shell: false,
        budget_source: None,
    }
}
