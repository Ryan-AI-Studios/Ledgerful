//! Execution assurance for one verify plan (0555).
//!
//! Says which commands recorded results, whether a recognized test command
//! was attempted, and what stays unknown. It does not claim test-case counts
//! or selection completeness. No I/O.

use serde::{Deserialize, Serialize};

use super::plan::{PlanSource, VerificationStep};
use super::results::VerificationResult;
use super::runner::split_command_string;

pub const EXECUTION_ASSURANCE_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AssurancePlanSource {
    ExplicitConfig,
    AutoPolicy,
    HistoricalRulesFallback,
    Manual,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ExecutionState {
    Refused,
    Planned,
    NoSteps,
    ResultsRecorded,
    Partial,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TestCommandEvidence {
    NotInvoked,
    Attempted,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TestCaseCountStatus {
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SelectionCompleteness {
    NotEstablished,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ExclusionsStatus {
    NotInventoried,
}

/// Nested packet. `testCasesRun` is always present and null in v1.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionAssuranceJson {
    pub schema_version: u32,
    pub plan_source: AssurancePlanSource,
    pub execution: ExecutionState,
    pub planned_steps: u64,
    pub recorded_steps: u64,
    pub passed_steps: u64,
    pub failed_steps: u64,
    pub test_command_evidence: TestCommandEvidence,
    /// Missing on a legacy inner object deserializes as `None`, never zero.
    /// Producers always write JSON null. No `skip_serializing_if`.
    #[serde(default)]
    pub test_cases_run: Option<u64>,
    pub test_case_count_status: TestCaseCountStatus,
    pub selection_completeness: SelectionCompleteness,
    pub exclusions_status: ExclusionsStatus,
    pub limitation_codes: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct AssuranceInputs<'a> {
    pub planned_steps: &'a [VerificationStep],
    pub results: &'a [VerificationResult],
    pub plan_source: Option<PlanSource>,
    pub manual_requested: bool,
    pub preview: bool,
    pub refused: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CommandClass {
    TestRun,
    NonTest,
    Unknown,
}

/// The only recorded command that this track calls cheap.
pub const CHEAP_GIT_VERSION_COMMAND: &str = "git --version";

/// Human sentence for an executed, passing, all-`git --version` recording.
pub const CHEAP_COMMAND_RECORDED_SENTENCE: &str =
    "Cheap command recorded: git --version. Not a project-suite pass.";

/// Human sentence for an executed, failing, all-`git --version` recording.
pub const CHEAP_COMMAND_FAILED_SENTENCE: &str =
    "Cheap command failed: git --version. Not a project-suite result.";

/// True only when the list is non-empty and every trimmed command is `git --version`.
pub fn recorded_commands_are_only_git_version(commands: &[impl AsRef<str>]) -> bool {
    !commands.is_empty()
        && commands
            .iter()
            .all(|command| command.as_ref().trim() == CHEAP_GIT_VERSION_COMMAND)
}

pub fn build_execution_assurance(input: AssuranceInputs<'_>) -> ExecutionAssuranceJson {
    let planned = len_u64(input.planned_steps.len());
    let recorded = len_u64(input.results.len());
    let passed_steps = len_u64(input.results.iter().filter(|r| r.exit_code == 0).count());
    let failed_steps = len_u64(input.results.iter().filter(|r| r.exit_code != 0).count());
    let plan_source = map_plan_source(input.manual_requested, input.plan_source);

    let invalid_empty_state = (input.preview || input.refused) && !input.results.is_empty();
    let (execution, correspondence_failed) = if invalid_empty_state {
        (ExecutionState::Unknown, true)
    } else if input.refused {
        (ExecutionState::Refused, false)
    } else if input.preview {
        (ExecutionState::Planned, false)
    } else if input.planned_steps.is_empty() && input.results.is_empty() {
        (ExecutionState::NoSteps, false)
    } else if input.results.is_empty() {
        (ExecutionState::Unknown, true)
    } else {
        classify_prefix(input.planned_steps, input.results)
    };

    let test_command_evidence = if correspondence_failed {
        TestCommandEvidence::Unknown
    } else if input.results.is_empty() {
        TestCommandEvidence::NotInvoked
    } else if input.manual_requested {
        TestCommandEvidence::Unknown
    } else {
        evidence_from_matched(input.planned_steps, input.results.len())
    };

    let runner_reported_no_tests = !correspondence_failed
        && input
            .results
            .iter()
            .enumerate()
            .take(input.planned_steps.len())
            .any(|(i, result)| {
                result.exit_code == 4 && is_direct_nextest_run(&input.planned_steps[i])
            });

    let mut limitation_codes = vec![
        "affected-tests-not-proven-complete".to_string(),
        "case-counts-not-collected".to_string(),
        "exclusions-not-inventoried".to_string(),
    ];
    match execution {
        ExecutionState::Unknown => {
            limitation_codes.push("result-correspondence-unknown".to_string());
        }
        ExecutionState::Planned | ExecutionState::Refused | ExecutionState::NoSteps => {
            limitation_codes.push("commands-not-executed".to_string());
        }
        ExecutionState::Partial => {
            limitation_codes.push("partial-command-results".to_string());
        }
        ExecutionState::ResultsRecorded => {}
    }
    if execution == ExecutionState::NoSteps {
        limitation_codes.push("no-verification-steps".to_string());
    }
    if test_command_evidence == TestCommandEvidence::Unknown && execution != ExecutionState::Unknown
    {
        limitation_codes.push("unknown-command-semantics".to_string());
    }
    if runner_reported_no_tests {
        limitation_codes.push("runner-reported-no-tests".to_string());
    }
    let recorded_commands: Vec<&str> = input
        .results
        .iter()
        .map(|result| result.command.as_str())
        .collect();
    if recorded_commands_are_only_git_version(&recorded_commands) {
        limitation_codes.push("cheap-command".to_string());
    }
    limitation_codes.sort();
    limitation_codes.dedup();

    ExecutionAssuranceJson {
        schema_version: EXECUTION_ASSURANCE_SCHEMA_VERSION,
        plan_source,
        execution,
        planned_steps: planned,
        recorded_steps: recorded,
        passed_steps,
        failed_steps,
        test_command_evidence,
        test_cases_run: None,
        test_case_count_status: TestCaseCountStatus::Unavailable,
        selection_completeness: SelectionCompleteness::NotEstablished,
        exclusions_status: ExclusionsStatus::NotInventoried,
        limitation_codes,
    }
}

/// One ASCII line, at most 512 bytes. `results recorded` is not pass wording.
pub fn format_assurance_line(packet: &ExecutionAssuranceJson) -> String {
    let state = match packet.execution {
        ExecutionState::ResultsRecorded => "results recorded",
        ExecutionState::Planned => "preview; no verification commands executed",
        ExecutionState::Refused => "refused; no verification commands executed",
        ExecutionState::NoSteps => "no verification steps planned; nothing executed",
        ExecutionState::Partial => "partial",
        ExecutionState::Unknown => "result correspondence unknown",
    };
    let evidence = match packet.test_command_evidence {
        TestCommandEvidence::NotInvoked => "not invoked",
        TestCommandEvidence::Attempted => "attempted (case execution unproven)",
        TestCommandEvidence::Unknown => "unknown",
    };
    let suffix = if packet
        .limitation_codes
        .iter()
        .any(|code| code == "runner-reported-no-tests")
    {
        "; runner reported no tests"
    } else {
        ""
    };
    format!(
        "Assurance: {recorded}/{planned} command results ({state}); test commands {evidence}; test-case count unavailable; affected-test completeness not established; exclusions not inventoried{suffix}.",
        recorded = packet.recorded_steps,
        planned = packet.planned_steps,
    )
}

fn classify_prefix(
    planned: &[VerificationStep],
    results: &[VerificationResult],
) -> (ExecutionState, bool) {
    let prefix = planned.len().min(results.len());
    let prefix_matches = (0..prefix).all(|i| planned[i].command == results[i].command);
    if !prefix_matches || results.len() > planned.len() {
        (ExecutionState::Unknown, true)
    } else if results.len() < planned.len() {
        (ExecutionState::Partial, false)
    } else {
        (ExecutionState::ResultsRecorded, false)
    }
}

fn evidence_from_matched(planned: &[VerificationStep], recorded: usize) -> TestCommandEvidence {
    let mut saw_test = false;
    let mut saw_unknown = false;
    for step in planned.iter().take(recorded) {
        match classify_step(step) {
            CommandClass::TestRun => saw_test = true,
            CommandClass::NonTest => {}
            CommandClass::Unknown => saw_unknown = true,
        }
    }
    if saw_unknown {
        TestCommandEvidence::Unknown
    } else if saw_test {
        TestCommandEvidence::Attempted
    } else {
        TestCommandEvidence::NotInvoked
    }
}

fn classify_step(step: &VerificationStep) -> CommandClass {
    if step.shell {
        return CommandClass::Unknown;
    }
    let Some(argv) = split_command_string(&step.command) else {
        return CommandClass::Unknown;
    };
    if argv.is_empty() || has_global_negative(&argv) {
        return CommandClass::Unknown;
    }
    if is_test_run_prefix(&argv) {
        if has_test_family_negative(&argv) {
            return CommandClass::Unknown;
        }
        return CommandClass::TestRun;
    }
    if is_closed_non_test(&argv) {
        CommandClass::NonTest
    } else {
        CommandClass::Unknown
    }
}

fn is_direct_nextest_run(step: &VerificationStep) -> bool {
    if step.shell {
        return false;
    }
    let Some(argv) = split_command_string(&step.command) else {
        return false;
    };
    if has_global_negative(&argv) || has_test_family_negative(&argv) {
        return false;
    }
    is_nextest_run_prefix(&argv)
}

fn is_test_run_prefix(argv: &[String]) -> bool {
    matches!(
        argv_head(argv),
        ("cargo", Some("test"), _) | ("cargo", Some("nextest"), Some("run"))
    )
}

fn is_nextest_run_prefix(argv: &[String]) -> bool {
    matches!(argv_head(argv), ("cargo", Some("nextest"), Some("run")))
}

fn is_closed_non_test(argv: &[String]) -> bool {
    match argv_head(argv) {
        ("cargo", Some("fmt" | "clippy" | "build" | "check"), _) => true,
        _ => {
            argv_exact(argv, &["git", "diff", "--check"])
                || argv_exact(argv, &["git", "diff", "--cached", "--check"])
        }
    }
}

fn argv_head(argv: &[String]) -> (&str, Option<&str>, Option<&str>) {
    (
        argv.first().map(String::as_str).unwrap_or(""),
        argv.get(1).map(String::as_str),
        argv.get(2).map(String::as_str),
    )
}

fn argv_exact(argv: &[String], expected: &[&str]) -> bool {
    argv.len() == expected.len() && argv.iter().zip(expected).all(|(got, want)| got == want)
}

fn has_global_negative(argv: &[String]) -> bool {
    argv.iter()
        .any(|token| matches!(token.as_str(), "--help" | "-h" | "--version" | "-V"))
}

fn has_test_family_negative(argv: &[String]) -> bool {
    argv.iter()
        .any(|token| matches!(token.as_str(), "--no-run" | "--list"))
}

fn map_plan_source(manual_requested: bool, source: Option<PlanSource>) -> AssurancePlanSource {
    if manual_requested {
        return AssurancePlanSource::Manual;
    }
    match source {
        Some(PlanSource::ExplicitConfig) => AssurancePlanSource::ExplicitConfig,
        Some(PlanSource::AutoPolicy) => AssurancePlanSource::AutoPolicy,
        Some(PlanSource::HistoricalRulesFallback) => AssurancePlanSource::HistoricalRulesFallback,
        Some(PlanSource::Manual) => AssurancePlanSource::Manual,
        None => AssurancePlanSource::Unknown,
    }
}

fn len_u64(len: usize) -> u64 {
    u64::try_from(len).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    fn step(command: &str) -> VerificationStep {
        VerificationStep {
            command: command.to_string(),
            timeout_secs: 60,
            description: "step".into(),
            shell: false,
            budget_source: None,
        }
    }

    fn shell_step(command: &str) -> VerificationStep {
        let mut built = step(command);
        built.shell = true;
        built
    }

    fn result(command: &str, exit: i32) -> VerificationResult {
        VerificationResult {
            command: command.to_string(),
            exit_code: exit,
            duration_ms: 1,
            stdout_summary: String::new(),
            stderr_summary: String::new(),
            truncated: false,
            timestamp: "2026-01-01T00:00:00Z".into(),
        }
    }

    fn pair(command: &str, exit: i32) -> (VerificationStep, VerificationResult) {
        (step(command), result(command, exit))
    }

    struct Built {
        steps: Vec<VerificationStep>,
        results: Vec<VerificationResult>,
        source: Option<PlanSource>,
        manual: bool,
        preview: bool,
        refused: bool,
    }

    fn executed(pairs: &[(&str, i32)]) -> Built {
        let mut steps = Vec::new();
        let mut results = Vec::new();
        for (command, exit) in pairs {
            let (s, r) = pair(command, *exit);
            steps.push(s);
            results.push(r);
        }
        Built {
            steps,
            results,
            source: Some(PlanSource::AutoPolicy),
            manual: false,
            preview: false,
            refused: false,
        }
    }

    fn build(built: &Built) -> ExecutionAssuranceJson {
        build_execution_assurance(AssuranceInputs {
            planned_steps: &built.steps,
            results: &built.results,
            plan_source: built.source.clone(),
            manual_requested: built.manual,
            preview: built.preview,
            refused: built.refused,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn assert_core(
        packet: &ExecutionAssuranceJson,
        execution: ExecutionState,
        planned: u64,
        recorded: u64,
        passed: u64,
        failed: u64,
        evidence: TestCommandEvidence,
        source: AssurancePlanSource,
        codes: &[&str],
    ) {
        assert_eq!(packet.schema_version, 1);
        assert_eq!(packet.execution, execution);
        assert_eq!(packet.planned_steps, planned);
        assert_eq!(packet.recorded_steps, recorded);
        assert_eq!(packet.passed_steps, passed);
        assert_eq!(packet.failed_steps, failed);
        assert_eq!(packet.test_command_evidence, evidence);
        assert_eq!(packet.plan_source, source);
        assert_eq!(packet.test_cases_run, None);
        assert_eq!(
            packet.test_case_count_status,
            TestCaseCountStatus::Unavailable
        );
        assert_eq!(
            packet.selection_completeness,
            SelectionCompleteness::NotEstablished
        );
        assert_eq!(packet.exclusions_status, ExclusionsStatus::NotInventoried);
        let got: Vec<&str> = packet.limitation_codes.iter().map(String::as_str).collect();
        assert_eq!(got, codes);
        let value = serde_json::to_value(packet).expect("packet json");
        assert_eq!(
            value.get("testCasesRun"),
            Some(&serde_json::Value::Null),
            "testCasesRun must be present and null"
        );
        assert!(
            value
                .as_object()
                .expect("object")
                .contains_key("testCasesRun")
        );
    }

    const BASE: &[&str] = &[
        "affected-tests-not-proven-complete",
        "case-counts-not-collected",
        "exclusions-not-inventoried",
    ];

    fn with_extra(extra: &[&'static str]) -> Vec<&'static str> {
        let mut codes = BASE.to_vec();
        codes.extend_from_slice(extra);
        codes.sort_unstable();
        codes
    }

    #[rstest]
    #[case("cheap")]
    #[case("zero_executed")]
    #[case("zero_preview")]
    #[case("refused_empty")]
    #[case("refused_synthetic")]
    #[case("nextest_attempt")]
    #[case("exit4")]
    #[case("no_tests_pass")]
    #[case("short_prefix_124")]
    #[case("full_prefix_124")]
    #[case("failing_prefix")]
    #[case("excess")]
    #[case("mismatch")]
    #[case("reorder")]
    #[case("zero_planned_results")]
    #[case("absent_source")]
    #[case("planned_without_results")]
    #[case("invalid_preview")]
    #[case("invalid_refused")]
    #[case("manual_git")]
    #[case("shell_echo")]
    #[case("full_auto")]
    #[case("rules_build")]
    #[case("opaque_nextest")]
    #[case("npm_test_ci")]
    #[case("source_override")]
    fn assurance_matrix(#[case] id: &str) {
        match id {
            "cheap" => {
                let built = executed(&[
                    ("cargo fmt --all -- --check", 0),
                    (
                        "cargo clippy --all-targets --all-features -- -D warnings",
                        0,
                    ),
                ]);
                let packet = build(&built);
                assert_core(
                    &packet,
                    ExecutionState::ResultsRecorded,
                    2,
                    2,
                    2,
                    0,
                    TestCommandEvidence::NotInvoked,
                    AssurancePlanSource::AutoPolicy,
                    BASE,
                );
                let line = format_assurance_line(&packet);
                assert!(line.contains("results recorded"));
                assert!(!line.contains("Verification passed"));
            }
            "zero_executed" => {
                let built = executed(&[]);
                let packet = build(&built);
                assert_core(
                    &packet,
                    ExecutionState::NoSteps,
                    0,
                    0,
                    0,
                    0,
                    TestCommandEvidence::NotInvoked,
                    AssurancePlanSource::AutoPolicy,
                    &with_extra(&["commands-not-executed", "no-verification-steps"]),
                );
                assert!(
                    format_assurance_line(&packet)
                        .contains("no verification steps planned; nothing executed")
                );
            }
            "zero_preview" => {
                let mut built = executed(&[]);
                built.preview = true;
                let packet = build(&built);
                assert_core(
                    &packet,
                    ExecutionState::Planned,
                    0,
                    0,
                    0,
                    0,
                    TestCommandEvidence::NotInvoked,
                    AssurancePlanSource::AutoPolicy,
                    &with_extra(&["commands-not-executed"]),
                );
                assert!(
                    !packet
                        .limitation_codes
                        .iter()
                        .any(|code| code == "no-verification-steps")
                );
            }
            "refused_empty" => {
                let mut built = executed(&[]);
                built.refused = true;
                let packet = build(&built);
                assert_core(
                    &packet,
                    ExecutionState::Refused,
                    0,
                    0,
                    0,
                    0,
                    TestCommandEvidence::NotInvoked,
                    AssurancePlanSource::AutoPolicy,
                    &with_extra(&["commands-not-executed"]),
                );
                assert_eq!(
                    format_assurance_line(&packet),
                    "Assurance: 0/0 command results (refused; no verification commands executed); test commands not invoked; test-case count unavailable; affected-test completeness not established; exclusions not inventoried."
                );
            }
            "refused_synthetic" => {
                let mut built = Built {
                    steps: vec![step("cargo fmt --all -- --check"), step("cargo clippy")],
                    results: Vec::new(),
                    source: Some(PlanSource::AutoPolicy),
                    manual: false,
                    preview: false,
                    refused: true,
                };
                built.refused = true;
                let packet = build(&built);
                assert_eq!(packet.execution, ExecutionState::Refused);
                assert_eq!(packet.planned_steps, 2);
                assert_eq!(packet.recorded_steps, 0);
                assert!(format_assurance_line(&packet).starts_with("Assurance: 0/2 "));
            }
            "nextest_attempt" => {
                let built = executed(&[(
                    "cargo nextest run --workspace --all-features --profile ci",
                    0,
                )]);
                let packet = build(&built);
                assert_eq!(packet.execution, ExecutionState::ResultsRecorded);
                assert_eq!(packet.test_command_evidence, TestCommandEvidence::Attempted);
                assert_eq!(
                    packet.selection_completeness,
                    SelectionCompleteness::NotEstablished
                );
                assert!(
                    !packet
                        .limitation_codes
                        .iter()
                        .any(|code| code == "runner-reported-no-tests")
                );
            }
            "exit4" => {
                let built = executed(&[("cargo nextest run -E test(foo)", 4)]);
                let packet = build(&built);
                assert_core(
                    &packet,
                    ExecutionState::ResultsRecorded,
                    1,
                    1,
                    0,
                    1,
                    TestCommandEvidence::Attempted,
                    AssurancePlanSource::AutoPolicy,
                    &with_extra(&["runner-reported-no-tests"]),
                );
                let line = format_assurance_line(&packet);
                assert!(line.contains("results recorded"));
                assert!(line.contains("runner reported no tests"));
                assert!(!line.contains("excluded"));
                assert!(!line.contains("Verification passed"));
            }
            "no_tests_pass" => {
                let built = executed(&[("cargo nextest run --no-tests=pass", 0)]);
                let packet = build(&built);
                assert_eq!(packet.test_command_evidence, TestCommandEvidence::Attempted);
                assert_eq!(packet.failed_steps, 0);
                assert_eq!(packet.test_cases_run, None);
                assert!(
                    !packet
                        .limitation_codes
                        .iter()
                        .any(|code| code == "runner-reported-no-tests")
                );
            }
            "short_prefix_124" => {
                let mut built = executed(&[("cargo fmt --all -- --check", 124)]);
                built.steps.push(step("cargo nextest run --workspace"));
                let packet = build(&built);
                assert_eq!(packet.execution, ExecutionState::Partial);
                assert_eq!(packet.failed_steps, 1);
                assert_eq!(packet.recorded_steps, 1);
                assert_eq!(packet.planned_steps, 2);
                assert!(
                    packet
                        .limitation_codes
                        .iter()
                        .any(|code| code == "partial-command-results")
                );
            }
            "full_prefix_124" => {
                let built = executed(&[("cargo fmt --all -- --check", 124)]);
                let packet = build(&built);
                assert_eq!(packet.execution, ExecutionState::ResultsRecorded);
                assert_eq!(packet.failed_steps, 1);
                assert!(
                    !packet
                        .limitation_codes
                        .iter()
                        .any(|code| code == "partial-command-results")
                );
            }
            "failing_prefix" => {
                let built = executed(&[("cargo fmt --all -- --check", 1)]);
                let packet = build(&built);
                assert_eq!(packet.execution, ExecutionState::ResultsRecorded);
                assert!(packet.failed_steps >= 1);
                let line = format_assurance_line(&packet);
                assert!(line.contains("results recorded"));
                assert!(!line.contains("Verification passed"));
                assert_eq!(line.matches("results recorded").count(), 1);
            }
            "excess" => {
                let mut built = executed(&[("cargo fmt --all -- --check", 0)]);
                built.results.push(result("cargo clippy", 1));
                let packet = build(&built);
                assert_core(
                    &packet,
                    ExecutionState::Unknown,
                    1,
                    2,
                    1,
                    1,
                    TestCommandEvidence::Unknown,
                    AssurancePlanSource::AutoPolicy,
                    &with_extra(&["result-correspondence-unknown"]),
                );
                assert!(
                    !format_assurance_line(&packet).contains("no verification commands executed")
                );
            }
            "mismatch" => {
                let mut built = executed(&[("cargo fmt --all -- --check", 0)]);
                built.results = vec![result("cargo clippy --all-targets", 1)];
                let packet = build(&built);
                assert_core(
                    &packet,
                    ExecutionState::Unknown,
                    1,
                    1,
                    0,
                    1,
                    TestCommandEvidence::Unknown,
                    AssurancePlanSource::AutoPolicy,
                    &with_extra(&["result-correspondence-unknown"]),
                );
            }
            "reorder" => {
                let built = executed(&[
                    ("cargo fmt --all -- --check", 0),
                    (
                        "cargo clippy --all-targets --all-features -- -D warnings",
                        0,
                    ),
                ]);
                let mut reordered = built;
                reordered.results.reverse();
                let packet = build(&reordered);
                assert_core(
                    &packet,
                    ExecutionState::Unknown,
                    2,
                    2,
                    2,
                    0,
                    TestCommandEvidence::Unknown,
                    AssurancePlanSource::AutoPolicy,
                    &with_extra(&["result-correspondence-unknown"]),
                );
            }
            "zero_planned_results" => {
                let mut built = executed(&[]);
                built.results.push(result("cargo fmt", 0));
                let packet = build(&built);
                assert_eq!(packet.execution, ExecutionState::Unknown);
                assert_eq!(
                    (
                        packet.planned_steps,
                        packet.recorded_steps,
                        packet.passed_steps,
                        packet.failed_steps
                    ),
                    (0, 1, 1, 0)
                );
            }
            "absent_source" => {
                let mut built = executed(&[("cargo fmt --all -- --check", 0)]);
                built.source = None;
                let packet = build(&built);
                assert_eq!(packet.execution, ExecutionState::ResultsRecorded);
                assert_eq!(packet.plan_source, AssurancePlanSource::Unknown);
                assert!(
                    !packet
                        .limitation_codes
                        .iter()
                        .any(|code| code == "result-correspondence-unknown")
                );
            }
            "planned_without_results" => {
                let built = Built {
                    steps: vec![step("cargo nextest run --workspace")],
                    results: Vec::new(),
                    source: Some(PlanSource::AutoPolicy),
                    manual: false,
                    preview: false,
                    refused: false,
                };
                let packet = build(&built);
                assert_eq!(packet.execution, ExecutionState::Unknown);
                assert_eq!(packet.planned_steps, 1);
                assert_eq!(packet.recorded_steps, 0);
                assert_eq!(packet.test_command_evidence, TestCommandEvidence::Unknown);
            }
            "invalid_preview" => {
                let mut built = executed(&[("cargo fmt --all -- --check", 0)]);
                built.preview = true;
                let packet = build(&built);
                assert_eq!(packet.execution, ExecutionState::Unknown);
                assert_eq!(packet.recorded_steps, 1);
                assert!(
                    !format_assurance_line(&packet).contains("no verification commands executed")
                );
            }
            "invalid_refused" => {
                let mut built = executed(&[("cargo fmt --all -- --check", 1)]);
                built.refused = true;
                let packet = build(&built);
                assert_eq!(packet.execution, ExecutionState::Unknown);
                assert_eq!(packet.failed_steps, 1);
                assert!(
                    !format_assurance_line(&packet).contains("no verification commands executed")
                );
            }
            "manual_git" => {
                let mut built = executed(&[("git --version", 0)]);
                built.manual = true;
                built.steps[0].shell = true;
                built.source = Some(PlanSource::ExplicitConfig);
                let packet = build(&built);
                assert_eq!(packet.execution, ExecutionState::ResultsRecorded);
                assert_eq!(packet.test_command_evidence, TestCommandEvidence::Unknown);
                assert_eq!(packet.plan_source, AssurancePlanSource::Manual);
            }
            "shell_echo" => {
                let mut built = executed(&[("echo cargo test", 0)]);
                built.steps = vec![shell_step("echo cargo test")];
                let packet = build(&built);
                assert_eq!(packet.test_command_evidence, TestCommandEvidence::Unknown);
                assert!(
                    packet
                        .limitation_codes
                        .iter()
                        .any(|code| code == "unknown-command-semantics")
                );
            }
            "full_auto" => {
                let built = executed(&[
                    ("git diff --check", 0),
                    ("git diff --cached --check", 0),
                    ("cargo fmt --all -- --check", 0),
                    (
                        "cargo clippy --all-targets --all-features -- -D warnings",
                        0,
                    ),
                    (
                        "cargo nextest run --workspace --all-features --profile ci",
                        0,
                    ),
                ]);
                let packet = build(&built);
                assert_eq!(packet.test_command_evidence, TestCommandEvidence::Attempted);
                assert!(
                    !packet
                        .limitation_codes
                        .iter()
                        .any(|code| code == "unknown-command-semantics")
                );
            }
            "rules_build" => {
                let mut built = executed(&[
                    ("cargo build --workspace", 0),
                    ("cargo check --workspace", 0),
                    ("cargo nextest run --workspace", 0),
                ]);
                built.source = Some(PlanSource::ExplicitConfig);
                let packet = build(&built);
                assert_eq!(packet.test_command_evidence, TestCommandEvidence::Attempted);
                assert_eq!(packet.plan_source, AssurancePlanSource::ExplicitConfig);
            }
            "opaque_nextest" => {
                let built =
                    executed(&[("npm run test:ci", 0), ("cargo nextest run --workspace", 0)]);
                let packet = build(&built);
                assert_eq!(packet.test_command_evidence, TestCommandEvidence::Unknown);
                assert!(
                    packet
                        .limitation_codes
                        .iter()
                        .any(|code| code == "unknown-command-semantics")
                );
            }
            "npm_test_ci" => {
                let built = executed(&[("npm run test:ci", 0)]);
                let packet = build(&built);
                assert_eq!(packet.test_command_evidence, TestCommandEvidence::Unknown);
                assert!(format_assurance_line(&packet).contains("test commands unknown"));
            }
            "source_override" => {
                let mut built = executed(&[("cargo fmt --all -- --check", 0)]);
                built.manual = true;
                built.source = Some(PlanSource::HistoricalRulesFallback);
                let packet = build(&built);
                assert_eq!(packet.plan_source, AssurancePlanSource::Manual);
            }
            other => panic!("uncovered assurance_matrix case {other}"),
        }
    }

    #[rstest]
    #[case("cargo nextest list", false)]
    #[case("cargo nextest archive", false)]
    #[case("cargo nextest show", false)]
    #[case("cargo test --no-run", false)]
    #[case("cargo nextest run --no-run", false)]
    #[case("cargo nextest run --help", false)]
    #[case("cargo nextest run -h", false)]
    #[case("cargo nextest run --list", false)]
    #[case("cargo nextest run --version", false)]
    #[case("cargo nextest run -V", false)]
    #[case("cargo nextest run -- --list", false)]
    #[case("cargo fmt --help", false)]
    #[case("cargo fmt --version", false)]
    #[case("cargo clippy -h", false)]
    #[case("cargo build -V", false)]
    #[case("cargo check --help", false)]
    #[case("cargo test \"unterminated", false)]
    #[case("deno task test:ci", false)]
    #[case("yarn run test:ci", false)]
    #[case("pnpm run test:ci", false)]
    #[case("git diff --check --stat", false)]
    #[case("cargo nextest run --workspace", true)]
    #[case("cargo test --workspace", true)]
    #[case("git diff --check", false)]
    fn assurance_command_classification(#[case] command: &str, #[case] attempted: bool) {
        let built = executed(&[(command, 0)]);
        let packet = build(&built);
        if attempted {
            assert_eq!(
                packet.test_command_evidence,
                TestCommandEvidence::Attempted,
                "{command}"
            );
        } else if command.starts_with("git diff --check") && !command.contains("--stat") {
            assert_eq!(
                packet.test_command_evidence,
                TestCommandEvidence::NotInvoked,
                "{command}"
            );
        } else {
            assert_eq!(
                packet.test_command_evidence,
                TestCommandEvidence::Unknown,
                "{command}"
            );
        }
        assert_eq!(packet.test_cases_run, None, "{command}");
    }

    #[test]
    fn assurance_serialization() {
        let built = executed(&[("cargo fmt --all -- --check", 0)]);
        let first = build(&built);
        let second = build(&built);
        let a = serde_json::to_string(&first).expect("json");
        let b = serde_json::to_string(&second).expect("json");
        assert_eq!(a, b);
        assert!(a.contains("\"testCasesRun\":null"));
        assert!(!a.contains("\"testCasesRun\":0"));

        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct LegacyInner {
            #[serde(default)]
            test_cases_run: Option<u64>,
            schema_version: u32,
        }
        let missing = r#"{"schemaVersion":1,"planSource":"auto-policy","execution":"results-recorded","plannedSteps":1,"recordedSteps":1,"passedSteps":1,"failedSteps":0,"testCommandEvidence":"not-invoked","testCaseCountStatus":"unavailable","selectionCompleteness":"not-established","exclusionsStatus":"not-inventoried","limitationCodes":["affected-tests-not-proven-complete"]}"#;
        let legacy: LegacyInner = serde_json::from_str(missing).expect("legacy inner");
        assert_eq!(legacy.schema_version, 1);
        assert_eq!(legacy.test_cases_run, None);
        assert_ne!(legacy.test_cases_run, Some(0));

        let max = ExecutionAssuranceJson {
            planned_steps: u64::MAX,
            recorded_steps: u64::MAX,
            ..first
        };
        let line = format_assurance_line(&max);
        assert!(line.is_ascii());
        assert!(!line.contains('\n') && !line.contains('\r'));
        assert!(line.len() <= 512, "len {}", line.len());
    }

    #[test]
    fn assurance_matrix_plan_source_spellings() {
        for (source, want) in [
            (
                PlanSource::ExplicitConfig,
                AssurancePlanSource::ExplicitConfig,
            ),
            (PlanSource::AutoPolicy, AssurancePlanSource::AutoPolicy),
            (
                PlanSource::HistoricalRulesFallback,
                AssurancePlanSource::HistoricalRulesFallback,
            ),
            (PlanSource::Manual, AssurancePlanSource::Manual),
        ] {
            let mut built = executed(&[("cargo fmt --all -- --check", 0)]);
            built.source = Some(source);
            assert_eq!(build(&built).plan_source, want);
        }
    }

    #[test]
    fn assurance_command_classification_cargo_test_exit_4_is_not_nextest_code() {
        let built = executed(&[("cargo test --workspace", 4)]);
        let packet = build(&built);
        assert_eq!(packet.failed_steps, 1);
        assert_eq!(packet.test_command_evidence, TestCommandEvidence::Attempted);
        assert!(
            !packet
                .limitation_codes
                .iter()
                .any(|code| code == "runner-reported-no-tests")
        );
    }

    fn line_packet(
        execution: ExecutionState,
        evidence: TestCommandEvidence,
        planned: u64,
        recorded: u64,
        extra: &[&'static str],
    ) -> ExecutionAssuranceJson {
        let mut codes = with_extra(extra);
        codes.retain(|code| !code.is_empty());
        ExecutionAssuranceJson {
            schema_version: 1,
            plan_source: AssurancePlanSource::AutoPolicy,
            execution,
            planned_steps: planned,
            recorded_steps: recorded,
            passed_steps: 0,
            failed_steps: 0,
            test_command_evidence: evidence,
            test_cases_run: None,
            test_case_count_status: TestCaseCountStatus::Unavailable,
            selection_completeness: SelectionCompleteness::NotEstablished,
            exclusions_status: ExclusionsStatus::NotInventoried,
            limitation_codes: codes.into_iter().map(str::to_string).collect(),
        }
    }

    #[test]
    fn format_assurance_line_is_exact_for_each_state() {
        let cases = [
            (
                line_packet(
                    ExecutionState::ResultsRecorded,
                    TestCommandEvidence::NotInvoked,
                    2,
                    2,
                    &[],
                ),
                "Assurance: 2/2 command results (results recorded); test commands not invoked; test-case count unavailable; affected-test completeness not established; exclusions not inventoried.",
            ),
            (
                line_packet(
                    ExecutionState::ResultsRecorded,
                    TestCommandEvidence::Attempted,
                    3,
                    3,
                    &[],
                ),
                "Assurance: 3/3 command results (results recorded); test commands attempted (case execution unproven); test-case count unavailable; affected-test completeness not established; exclusions not inventoried.",
            ),
            (
                line_packet(
                    ExecutionState::ResultsRecorded,
                    TestCommandEvidence::Unknown,
                    1,
                    1,
                    &["unknown-command-semantics"],
                ),
                "Assurance: 1/1 command results (results recorded); test commands unknown; test-case count unavailable; affected-test completeness not established; exclusions not inventoried.",
            ),
            (
                line_packet(
                    ExecutionState::Planned,
                    TestCommandEvidence::NotInvoked,
                    1,
                    0,
                    &["commands-not-executed"],
                ),
                "Assurance: 0/1 command results (preview; no verification commands executed); test commands not invoked; test-case count unavailable; affected-test completeness not established; exclusions not inventoried.",
            ),
            (
                line_packet(
                    ExecutionState::Refused,
                    TestCommandEvidence::NotInvoked,
                    0,
                    0,
                    &["commands-not-executed"],
                ),
                "Assurance: 0/0 command results (refused; no verification commands executed); test commands not invoked; test-case count unavailable; affected-test completeness not established; exclusions not inventoried.",
            ),
            (
                line_packet(
                    ExecutionState::NoSteps,
                    TestCommandEvidence::NotInvoked,
                    0,
                    0,
                    &["commands-not-executed", "no-verification-steps"],
                ),
                "Assurance: 0/0 command results (no verification steps planned; nothing executed); test commands not invoked; test-case count unavailable; affected-test completeness not established; exclusions not inventoried.",
            ),
            (
                line_packet(
                    ExecutionState::Partial,
                    TestCommandEvidence::NotInvoked,
                    2,
                    1,
                    &["partial-command-results"],
                ),
                "Assurance: 1/2 command results (partial); test commands not invoked; test-case count unavailable; affected-test completeness not established; exclusions not inventoried.",
            ),
            (
                line_packet(
                    ExecutionState::Unknown,
                    TestCommandEvidence::Unknown,
                    1,
                    1,
                    &["result-correspondence-unknown"],
                ),
                "Assurance: 1/1 command results (result correspondence unknown); test commands unknown; test-case count unavailable; affected-test completeness not established; exclusions not inventoried.",
            ),
            (
                line_packet(
                    ExecutionState::ResultsRecorded,
                    TestCommandEvidence::Attempted,
                    1,
                    1,
                    &["runner-reported-no-tests"],
                ),
                "Assurance: 1/1 command results (results recorded); test commands attempted (case execution unproven); test-case count unavailable; affected-test completeness not established; exclusions not inventoried; runner reported no tests.",
            ),
        ];
        for (packet, expected) in cases {
            let line = format_assurance_line(&packet);
            assert_eq!(line, expected);
            assert!(line.is_ascii());
            assert!(!line.contains('\n') && !line.contains('\r'));
            assert!(line.len() <= 512);
        }
    }

    #[test]
    fn cheap_command_only_when_every_result_is_git_version() {
        let mut built = executed(&[("git --version", 0)]);
        built.manual = true;
        let packet = build(&built);
        assert!(
            packet
                .limitation_codes
                .iter()
                .any(|code| code == "cheap-command")
        );
        assert!(
            packet
                .limitation_codes
                .iter()
                .any(|code| code == "unknown-command-semantics")
        );
        assert_eq!(packet.test_command_evidence, TestCommandEvidence::Unknown);
        assert_eq!(packet.test_cases_run, None);
        assert_eq!(packet.schema_version, EXECUTION_ASSURANCE_SCHEMA_VERSION);
        assert!(
            !format_assurance_line(&packet).contains("Cheap command"),
            "the assurance line does not grow the cheap-command words"
        );

        let trimmed = executed(&[("  git --version  ", 0)]);
        assert!(
            build(&trimmed)
                .limitation_codes
                .iter()
                .any(|code| code == "cheap-command")
        );

        for pairs in [
            &[("git --version --build-options", 0)][..],
            &[],
            &[("cargo fmt", 0)],
            &[("git --version", 0), ("cargo fmt", 0)],
        ] {
            let packet = build(&executed(pairs));
            assert!(
                !packet
                    .limitation_codes
                    .iter()
                    .any(|code| code == "cheap-command"),
                "not an all-git-version recording: {pairs:?} codes={:?}",
                packet.limitation_codes
            );
        }
    }
}
