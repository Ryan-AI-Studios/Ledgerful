//! Per-run verification selection evidence (0600).
//!
//! Says how a verify run was assembled (basis), whether prediction ran, how
//! fresh the cached impact report was, and for each planned step whether it
//! was only planned, passed, failed, never run, or cannot be matched to a
//! recorded result. It never claims selection soundness, completeness, or
//! test-case counts. `passed` means the recorded command exited 0, nothing
//! more. No I/O.

use serde::{Deserialize, Serialize};

use super::assurance::{CommandClass, classify_step_kind};
use super::plan::{PlanSource, VerificationStep, VerifyScope};
use super::results::VerificationResult;
use super::runner::{VERIFY_STEP_TIMEOUT_EXIT_CODE, split_command_string};
use crate::index::surface_freshness::{SurfaceFreshness, impact_freshness_row};
use crate::state::reports::ImpactFreshness;

pub const SELECTION_EVIDENCE_SCHEMA_VERSION: u32 = 1;

/// Constant limitation code present in every packet.
pub const SELECTION_SOUNDNESS_LIMITATION: &str = "selection-soundness-not-established";

/// How the run was assembled. Not a claim that the selection is sufficient.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "web", derive(utoipa::ToSchema))]
#[serde(rename_all = "kebab-case")]
pub enum SelectionBasis {
    FullRequested,
    ScopedSelection,
    FastNoTestStep,
    FallbackFull,
    Refused,
    ExplicitSteps,
    ManualCommand,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "web", derive(utoipa::ToSchema))]
#[serde(rename_all = "kebab-case")]
pub enum PredictionState {
    Ran,
    Skipped,
    Disabled,
    NotApplicable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "web", derive(utoipa::ToSchema))]
#[serde(rename_all = "kebab-case")]
pub enum StepKindJson {
    TestRun,
    NonTest,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "web", derive(utoipa::ToSchema))]
#[serde(rename_all = "kebab-case")]
pub enum StepState {
    Planned,
    Passed,
    Failed,
    NotRun,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PredictionEvidenceJson {
    pub state: PredictionState,
    pub warning_count: u64,
    /// Describes `latest-impact.json`, not the SQLite snapshot packet that
    /// prediction loads. Absent when freshness was not evaluated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub impact_freshness: Option<SurfaceFreshness>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectionStepJson {
    /// Zero-based position in the plan.
    pub index: u64,
    pub command: String,
    pub kind: StepKindJson,
    pub state: StepState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OmissionJson {
    pub code: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectionEvidenceJson {
    pub schema_version: u32,
    pub basis: SelectionBasis,
    pub prediction: PredictionEvidenceJson,
    /// Plan order.
    pub steps: Vec<SelectionStepJson>,
    pub omissions: Vec<OmissionJson>,
    /// Sorted, deduplicated.
    pub limitation_codes: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct SelectionEvidenceInputs<'a> {
    pub planned_steps: &'a [VerificationStep],
    pub results: &'a [VerificationResult],
    pub plan_source: Option<PlanSource>,
    pub manual_requested: bool,
    pub preview: bool,
    pub refused: bool,
    pub scope_requested: VerifyScope,
    /// Copied into omission `detail` only. Classification uses `.is_some()`.
    pub fallback_reason: Option<&'a str>,
    pub prediction_state: PredictionState,
    pub warning_count: usize,
    pub impact_freshness: Option<&'a ImpactFreshness>,
}

/// First match wins: not-applicable (manual or config plan), disabled
/// (`--no-predict`), skipped (0288 clean-tree dry-run skip), else ran.
pub fn prediction_state_for(
    manual_requested: bool,
    config_plan_used: bool,
    no_predict: bool,
    clean_tree_skip: bool,
) -> PredictionState {
    if manual_requested || config_plan_used {
        PredictionState::NotApplicable
    } else if no_predict {
        PredictionState::Disabled
    } else if clean_tree_skip {
        PredictionState::Skipped
    } else {
        PredictionState::Ran
    }
}

pub fn build_selection_evidence(input: SelectionEvidenceInputs<'_>) -> SelectionEvidenceJson {
    let kinds: Vec<StepKindJson> = input
        .planned_steps
        .iter()
        .map(|step| match classify_step_kind(step) {
            CommandClass::TestRun => StepKindJson::TestRun,
            CommandClass::NonTest => StepKindJson::NonTest,
            CommandClass::Unknown => StepKindJson::Unknown,
        })
        .collect();

    let basis = derive_basis(&input, &kinds);
    let (steps, correspondence_unknown, not_run) = build_steps(&input, &kinds);

    let mut omissions = Vec::new();
    if input.refused {
        omissions.push(OmissionJson {
            code: "refused-no-commands-run".to_string(),
            detail: input.fallback_reason.map(str::to_string),
        });
    } else if input.fallback_reason.is_some() {
        omissions.push(OmissionJson {
            code: "fallback-to-full".to_string(),
            detail: input.fallback_reason.map(str::to_string),
        });
    }
    if !input.refused && !kinds.contains(&StepKindJson::TestRun) {
        omissions.push(OmissionJson {
            code: "no-test-command-planned".to_string(),
            detail: None,
        });
    }
    if not_run > 0 {
        let mut detail = format!(
            "{not_run} of {} planned steps have no recorded result",
            input.planned_steps.len()
        );
        if input
            .results
            .last()
            .is_some_and(|last| last.exit_code == VERIFY_STEP_TIMEOUT_EXIT_CODE)
        {
            detail.push_str(&format!(
                "; last recorded exit code was {VERIFY_STEP_TIMEOUT_EXIT_CODE}"
            ));
        }
        omissions.push(OmissionJson {
            code: "steps-not-run".to_string(),
            detail: Some(detail),
        });
    }
    if correspondence_unknown {
        omissions.push(OmissionJson {
            code: "result-correspondence-unknown".to_string(),
            detail: None,
        });
    }

    let mut limitation_codes = vec![SELECTION_SOUNDNESS_LIMITATION.to_string()];
    match input.impact_freshness {
        Some(ImpactFreshness::Stale { .. }) => limitation_codes.push("impact-stale".to_string()),
        Some(ImpactFreshness::Corrupt { .. }) => {
            limitation_codes.push("impact-corrupt".to_string())
        }
        Some(ImpactFreshness::Missing) => limitation_codes.push("impact-missing".to_string()),
        Some(ImpactFreshness::CurrentClean | ImpactFreshness::CurrentDirty) => {}
        None => limitation_codes.push("impact-freshness-not-evaluated".to_string()),
    }
    if input.prediction_state != PredictionState::Ran {
        limitation_codes.push("prediction-not-run".to_string());
    }
    if input.warning_count > 0 {
        limitation_codes.push("prediction-warnings-present".to_string());
    }
    limitation_codes.sort();
    limitation_codes.dedup();

    SelectionEvidenceJson {
        schema_version: SELECTION_EVIDENCE_SCHEMA_VERSION,
        basis,
        prediction: PredictionEvidenceJson {
            state: input.prediction_state,
            warning_count: len_u64(input.warning_count),
            impact_freshness: input
                .impact_freshness
                .map(|freshness| impact_freshness_row(freshness, false)),
        },
        steps,
        omissions,
        limitation_codes,
    }
}

/// Spec 3b precedence. Reads no free text: `fallback_reason` is only tested
/// with `.is_some()` and step descriptions are never consulted.
fn derive_basis(input: &SelectionEvidenceInputs<'_>, kinds: &[StepKindJson]) -> SelectionBasis {
    if input.refused {
        return SelectionBasis::Refused;
    }
    if input.manual_requested {
        return SelectionBasis::ManualCommand;
    }
    if input.plan_source == Some(PlanSource::ExplicitConfig) {
        return SelectionBasis::ExplicitSteps;
    }
    if input.fallback_reason.is_some() {
        return SelectionBasis::FallbackFull;
    }
    if input.scope_requested == VerifyScope::Full {
        return SelectionBasis::FullRequested;
    }
    let has_test_step = kinds.contains(&StepKindJson::TestRun);
    let has_filterset = input
        .planned_steps
        .iter()
        .zip(kinds)
        .any(|(step, kind)| *kind == StepKindJson::TestRun && has_filterset_token(step));
    if has_filterset {
        SelectionBasis::ScopedSelection
    } else if !has_test_step {
        SelectionBasis::FastNoTestStep
    } else {
        SelectionBasis::Unknown
    }
}

/// Exact argv token `-E` from the shared tokenizer (D6).
fn has_filterset_token(step: &VerificationStep) -> bool {
    split_command_string(&step.command).is_some_and(|argv| argv.iter().any(|token| token == "-E"))
}

/// Returns `(steps, correspondence_unknown, not_run_count)`.
fn build_steps(
    input: &SelectionEvidenceInputs<'_>,
    kinds: &[StepKindJson],
) -> (Vec<SelectionStepJson>, bool, usize) {
    let planned = input.planned_steps;
    let results = input.results;

    let make = |states: Vec<(StepState, Option<i32>)>| -> Vec<SelectionStepJson> {
        planned
            .iter()
            .zip(kinds)
            .zip(states)
            .enumerate()
            .map(
                |(index, ((step, kind), (state, exit_code)))| SelectionStepJson {
                    index: len_u64(index),
                    command: step.command.clone(),
                    kind: *kind,
                    state,
                    exit_code,
                },
            )
            .collect()
    };
    let all = |state: StepState| make(planned.iter().map(|_| (state, None)).collect());

    if input.refused {
        // A refused plan has no steps; results with a refusal are invalid.
        return (Vec::new(), !results.is_empty(), 0);
    }
    if input.preview {
        if results.is_empty() {
            return (all(StepState::Planned), false, 0);
        }
        return (all(StepState::Unknown), true, 0);
    }
    if results.is_empty() {
        if planned.is_empty() {
            return (Vec::new(), false, 0);
        }
        return (all(StepState::Unknown), true, 0);
    }
    let prefix_matches = results.len() <= planned.len()
        && planned
            .iter()
            .zip(results)
            .all(|(step, result)| step.command == result.command);
    if !prefix_matches {
        return (all(StepState::Unknown), true, 0);
    }
    let states = planned
        .iter()
        .enumerate()
        .map(|(index, _)| match results.get(index) {
            Some(result) if result.exit_code == 0 => (StepState::Passed, None),
            Some(result) => (StepState::Failed, Some(result.exit_code)),
            None => (StepState::NotRun, None),
        })
        .collect();
    (make(states), false, planned.len() - results.len())
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

    fn steps_of(commands: &[&str]) -> Vec<VerificationStep> {
        commands.iter().map(|c| step(c)).collect()
    }

    fn results_of(pairs: &[(&str, i32)]) -> Vec<VerificationResult> {
        pairs.iter().map(|(c, e)| result(c, *e)).collect()
    }

    /// Executed, auto-policy, fast, no fallback, prediction ran, freshness
    /// not evaluated. Tests override the fields they care about.
    fn inputs<'a>(
        steps: &'a [VerificationStep],
        results: &'a [VerificationResult],
    ) -> SelectionEvidenceInputs<'a> {
        SelectionEvidenceInputs {
            planned_steps: steps,
            results,
            plan_source: Some(PlanSource::AutoPolicy),
            manual_requested: false,
            preview: false,
            refused: false,
            scope_requested: VerifyScope::Fast,
            fallback_reason: None,
            prediction_state: PredictionState::Ran,
            warning_count: 0,
            impact_freshness: None,
        }
    }

    fn omission_codes(evidence: &SelectionEvidenceJson) -> Vec<&str> {
        evidence
            .omissions
            .iter()
            .map(|omission| omission.code.as_str())
            .collect()
    }

    const FMT: &str = "cargo fmt --all -- --check";
    const CLIPPY: &str = "cargo clippy --workspace --all-targets -- -D warnings";
    const SCOPED: &str = "cargo nextest run --workspace --all-features -E 'test(cli_scan)'";
    const FULL_NEXTEST: &str = "cargo nextest run --workspace --all-features";

    #[rstest]
    #[case::scoped(&[FMT, CLIPPY, SCOPED], VerifyScope::Fast, Some(PlanSource::AutoPolicy), false, false, None, SelectionBasis::ScopedSelection)]
    #[case::fast_cheap(&[FMT, CLIPPY], VerifyScope::Fast, Some(PlanSource::AutoPolicy), false, false, None, SelectionBasis::FastNoTestStep)]
    #[case::fast_no_steps(&[], VerifyScope::Fast, Some(PlanSource::AutoPolicy), false, false, None, SelectionBasis::FastNoTestStep)]
    #[case::fallback(&[FMT, CLIPPY, FULL_NEXTEST], VerifyScope::Fast, Some(PlanSource::AutoPolicy), false, false, Some("shared infra changed"), SelectionBasis::FallbackFull)]
    #[case::full(&[FMT, CLIPPY, FULL_NEXTEST], VerifyScope::Full, Some(PlanSource::AutoPolicy), false, false, None, SelectionBasis::FullRequested)]
    #[case::refused(&[], VerifyScope::Fast, Some(PlanSource::AutoPolicy), false, true, Some("refusing full suite"), SelectionBasis::Refused)]
    #[case::explicit_steps_fast_scope(&["cargo test --workspace"], VerifyScope::Fast, Some(PlanSource::ExplicitConfig), false, false, None, SelectionBasis::ExplicitSteps)]
    #[case::manual(&["git --version"], VerifyScope::Fast, Some(PlanSource::Manual), true, false, None, SelectionBasis::ManualCommand)]
    #[case::unknown_no_source(&[FULL_NEXTEST], VerifyScope::Fast, None, false, false, None, SelectionBasis::Unknown)]
    #[case::filterset_flag_is_not_detected(&["cargo nextest run --filterset test(x)"], VerifyScope::Fast, Some(PlanSource::AutoPolicy), false, false, None, SelectionBasis::Unknown)]
    #[case::non_test_step_with_e_token(&["cargo clippy -E x"], VerifyScope::Fast, Some(PlanSource::AutoPolicy), false, false, None, SelectionBasis::FastNoTestStep)]
    fn selection_evidence_basis_matrix(
        #[case] commands: &[&str],
        #[case] scope: VerifyScope,
        #[case] source: Option<PlanSource>,
        #[case] manual: bool,
        #[case] refused: bool,
        #[case] fallback: Option<&str>,
        #[case] expected: SelectionBasis,
    ) {
        let steps = steps_of(commands);
        let mut input = inputs(&steps, &[]);
        input.preview = true;
        input.scope_requested = scope;
        input.plan_source = source;
        input.manual_requested = manual;
        input.refused = refused;
        input.fallback_reason = fallback;
        assert_eq!(build_selection_evidence(input).basis, expected);
    }

    #[test]
    fn selection_evidence_basis_ignores_fallback_text() {
        let steps = steps_of(&[FMT, FULL_NEXTEST]);
        let mut a = inputs(&steps, &[]);
        a.preview = true;
        a.fallback_reason = Some("shared infra changed");
        let mut b = a.clone();
        b.fallback_reason = Some("a completely different, scoped selection -E sentence");
        let (a, b) = (build_selection_evidence(a), build_selection_evidence(b));
        assert_eq!(a.basis, SelectionBasis::FallbackFull);
        assert_eq!(a.basis, b.basis);
        assert_eq!(a.steps, b.steps);
        assert_eq!(a.limitation_codes, b.limitation_codes);

        // Step description text is never read either.
        let mut described = step(FULL_NEXTEST);
        described.description = "Scoped selection via -E filterset; fallback to full".into();
        let steps = vec![described];
        let mut input = inputs(&steps, &[]);
        input.preview = true;
        assert_eq!(
            build_selection_evidence(input).basis,
            SelectionBasis::Unknown
        );
    }

    #[test]
    fn selection_evidence_explicit_config_precedes_fallback() {
        let steps = steps_of(&["cargo test --workspace"]);
        let mut input = inputs(&steps, &[]);
        input.preview = true;
        input.plan_source = Some(PlanSource::ExplicitConfig);
        input.fallback_reason = Some("fast scope fell back");
        let evidence = build_selection_evidence(input);
        assert_eq!(evidence.basis, SelectionBasis::ExplicitSteps);
        assert!(omission_codes(&evidence).contains(&"fallback-to-full"));
    }

    #[test]
    fn selection_evidence_executed_step_states() {
        let steps = steps_of(&[FMT, CLIPPY, FULL_NEXTEST]);
        let results = results_of(&[(FMT, 0), (CLIPPY, 0), (FULL_NEXTEST, 0)]);
        let evidence = build_selection_evidence(inputs(&steps, &results));
        assert_eq!(evidence.steps.len(), 3);
        assert!(
            evidence
                .steps
                .iter()
                .all(|s| s.state == StepState::Passed && s.exit_code.is_none())
        );
        let kinds: Vec<StepKindJson> = evidence.steps.iter().map(|s| s.kind).collect();
        assert_eq!(
            kinds,
            vec![
                StepKindJson::NonTest,
                StepKindJson::NonTest,
                StepKindJson::TestRun
            ]
        );
        let indexes: Vec<u64> = evidence.steps.iter().map(|s| s.index).collect();
        assert_eq!(indexes, vec![0, 1, 2]);
        assert!(!omission_codes(&evidence).contains(&"no-test-command-planned"));
        assert!(!omission_codes(&evidence).contains(&"steps-not-run"));
    }

    #[test]
    fn selection_evidence_non_test_pass_is_not_test_evidence() {
        // fmt + clippy both exit 0.
        let steps = steps_of(&[FMT, CLIPPY]);
        let results = results_of(&[(FMT, 0), (CLIPPY, 0)]);
        let evidence = build_selection_evidence(inputs(&steps, &results));
        assert!(
            evidence
                .steps
                .iter()
                .all(|s| s.kind == StepKindJson::NonTest)
        );
        assert!(evidence.steps.iter().all(|s| s.state == StepState::Passed));
        assert!(omission_codes(&evidence).contains(&"no-test-command-planned"));
        assert!(
            !evidence
                .steps
                .iter()
                .any(|s| s.kind == StepKindJson::TestRun)
        );

        // npm run test:ci exits 0 but reads kind unknown.
        let steps = steps_of(&["npm run test:ci"]);
        let results = results_of(&[("npm run test:ci", 0)]);
        let evidence = build_selection_evidence(inputs(&steps, &results));
        assert_eq!(evidence.steps[0].kind, StepKindJson::Unknown);
        assert_eq!(evidence.steps[0].state, StepState::Passed);
        assert!(omission_codes(&evidence).contains(&"no-test-command-planned"));

        // A shell step never reads test-run.
        let steps = vec![shell_step("cargo test --workspace")];
        let results = results_of(&[("cargo test --workspace", 0)]);
        let evidence = build_selection_evidence(inputs(&steps, &results));
        assert_eq!(evidence.steps[0].kind, StepKindJson::Unknown);

        // Direct nextest exit 4 stays failed; this packet does not restate it.
        let steps = steps_of(&[FULL_NEXTEST]);
        let results = results_of(&[(FULL_NEXTEST, 4)]);
        let evidence = build_selection_evidence(inputs(&steps, &results));
        assert_eq!(evidence.steps[0].kind, StepKindJson::TestRun);
        assert_eq!(evidence.steps[0].state, StepState::Failed);
        assert_eq!(evidence.steps[0].exit_code, Some(4));
        assert!(
            !omission_codes(&evidence)
                .iter()
                .any(|code| code.contains("no-tests"))
        );
    }

    #[test]
    fn selection_evidence_not_run_after_timeout_is_partial() {
        let steps = steps_of(&[FMT, CLIPPY, FULL_NEXTEST]);
        let results = results_of(&[(FMT, 0), (CLIPPY, 124)]);
        let evidence = build_selection_evidence(inputs(&steps, &results));
        let states: Vec<StepState> = evidence.steps.iter().map(|s| s.state).collect();
        assert_eq!(
            states,
            vec![StepState::Passed, StepState::Failed, StepState::NotRun]
        );
        assert_eq!(evidence.steps[1].exit_code, Some(124));
        assert_eq!(evidence.steps[2].exit_code, None);
        let omission = evidence
            .omissions
            .iter()
            .find(|o| o.code == "steps-not-run")
            .expect("steps-not-run");
        assert_eq!(
            omission.detail.as_deref(),
            Some("1 of 3 planned steps have no recorded result; last recorded exit code was 124")
        );
        assert!(!omission_codes(&evidence).contains(&"result-correspondence-unknown"));

        // Same shape without a timeout exit has no 124 note.
        let results = results_of(&[(FMT, 0), (CLIPPY, 1)]);
        let evidence = build_selection_evidence(inputs(&steps, &results));
        let omission = evidence
            .omissions
            .iter()
            .find(|o| o.code == "steps-not-run")
            .expect("steps-not-run");
        assert_eq!(
            omission.detail.as_deref(),
            Some("1 of 3 planned steps have no recorded result")
        );
    }

    #[test]
    fn selection_evidence_correspondence_unknown_marks_every_step_unknown() {
        let steps = steps_of(&[FMT, CLIPPY]);
        let cases: Vec<Vec<VerificationResult>> = vec![
            // reordered
            results_of(&[(CLIPPY, 0), (FMT, 0)]),
            // extra result
            results_of(&[(FMT, 0), (CLIPPY, 0), (FULL_NEXTEST, 0)]),
            // mismatched command
            results_of(&[(FMT, 0), ("cargo build", 1)]),
        ];
        for results in cases {
            let evidence = build_selection_evidence(inputs(&steps, &results));
            assert_eq!(evidence.steps.len(), 2);
            assert!(
                evidence
                    .steps
                    .iter()
                    .all(|s| s.state == StepState::Unknown && s.exit_code.is_none()),
                "{evidence:?}"
            );
            assert!(omission_codes(&evidence).contains(&"result-correspondence-unknown"));
            assert!(!omission_codes(&evidence).contains(&"steps-not-run"));
        }
    }

    #[test]
    fn selection_evidence_zero_results_with_plan_is_unknown() {
        let steps = steps_of(&[FMT, FULL_NEXTEST]);
        let evidence = build_selection_evidence(inputs(&steps, &[]));
        assert!(evidence.steps.iter().all(|s| s.state == StepState::Unknown));
        assert!(omission_codes(&evidence).contains(&"result-correspondence-unknown"));
        assert!(!evidence.steps.iter().any(|s| s.state == StepState::NotRun));

        // Nothing planned and nothing recorded is simply empty, not unknown.
        let evidence = build_selection_evidence(inputs(&[], &[]));
        assert!(evidence.steps.is_empty());
        assert!(!omission_codes(&evidence).contains(&"result-correspondence-unknown"));
        assert!(omission_codes(&evidence).contains(&"no-test-command-planned"));
    }

    #[test]
    fn selection_evidence_preview_marks_every_step_planned() {
        let steps = steps_of(&[FMT, FULL_NEXTEST]);
        let mut input = inputs(&steps, &[]);
        input.preview = true;
        let evidence = build_selection_evidence(input);
        assert!(evidence.steps.iter().all(|s| s.state == StepState::Planned));
        assert!(evidence.steps.iter().all(|s| s.exit_code.is_none()));
        assert!(omission_codes(&evidence).is_empty());

        // D3: preview with recorded results is invalid; every step unknown.
        let results = results_of(&[(FMT, 0)]);
        let mut input = inputs(&steps, &results);
        input.preview = true;
        let evidence = build_selection_evidence(input);
        assert!(evidence.steps.iter().all(|s| s.state == StepState::Unknown));
        assert!(omission_codes(&evidence).contains(&"result-correspondence-unknown"));
    }

    #[test]
    fn selection_evidence_refused_has_no_steps_and_only_refusal_omission() {
        let steps = steps_of(&[FMT, FULL_NEXTEST]);
        let mut input = inputs(&steps, &[]);
        input.refused = true;
        input.fallback_reason = Some("refusing full suite");
        let evidence = build_selection_evidence(input);
        assert_eq!(evidence.basis, SelectionBasis::Refused);
        assert!(evidence.steps.is_empty());
        assert_eq!(omission_codes(&evidence), vec!["refused-no-commands-run"]);
        assert_eq!(
            evidence.omissions[0].detail.as_deref(),
            Some("refusing full suite")
        );

        // Refusal with recorded results is invalid: correspondence unknown.
        let results = results_of(&[(FMT, 0)]);
        let mut input = inputs(&steps, &results);
        input.refused = true;
        let evidence = build_selection_evidence(input);
        assert!(evidence.steps.is_empty());
        assert_eq!(
            omission_codes(&evidence),
            vec!["refused-no-commands-run", "result-correspondence-unknown"]
        );
    }

    #[rstest]
    #[case(true, true, true, true, PredictionState::NotApplicable)]
    #[case(true, false, true, true, PredictionState::NotApplicable)]
    #[case(false, true, true, true, PredictionState::NotApplicable)]
    #[case(false, false, true, true, PredictionState::Disabled)]
    #[case(false, false, true, false, PredictionState::Disabled)]
    #[case(false, false, false, true, PredictionState::Skipped)]
    #[case(false, false, false, false, PredictionState::Ran)]
    fn selection_evidence_prediction_state_matrix(
        #[case] manual: bool,
        #[case] config_plan: bool,
        #[case] no_predict: bool,
        #[case] clean_tree_skip: bool,
        #[case] expected: PredictionState,
    ) {
        assert_eq!(
            prediction_state_for(manual, config_plan, no_predict, clean_tree_skip),
            expected
        );
    }

    #[test]
    fn selection_evidence_impact_freshness_matrix() {
        use crate::index::surface_freshness::{SurfaceFreshnessSource, SurfaceFreshnessStatus};

        let steps = steps_of(&[FMT]);
        let stale = ImpactFreshness::Stale {
            reason: "HEAD has changed since impact scan".into(),
        };
        let corrupt = ImpactFreshness::Corrupt {
            reason: "bad json".into(),
        };
        let cases: Vec<(
            Option<&ImpactFreshness>,
            Option<SurfaceFreshnessStatus>,
            Option<&str>,
        )> = vec![
            (
                Some(&ImpactFreshness::CurrentClean),
                Some(SurfaceFreshnessStatus::Available),
                None,
            ),
            (
                Some(&ImpactFreshness::CurrentDirty),
                Some(SurfaceFreshnessStatus::Available),
                None,
            ),
            (
                Some(&stale),
                Some(SurfaceFreshnessStatus::Stale),
                Some("impact-stale"),
            ),
            (
                Some(&corrupt),
                Some(SurfaceFreshnessStatus::Unavailable),
                Some("impact-corrupt"),
            ),
            (
                Some(&ImpactFreshness::Missing),
                Some(SurfaceFreshnessStatus::Unavailable),
                Some("impact-missing"),
            ),
            (None, None, Some("impact-freshness-not-evaluated")),
        ];
        for (freshness, status, code) in cases {
            let mut input = inputs(&steps, &[]);
            input.preview = true;
            input.impact_freshness = freshness;
            let evidence = build_selection_evidence(input);
            assert_eq!(
                evidence
                    .prediction
                    .impact_freshness
                    .as_ref()
                    .map(|r| r.status),
                status,
                "{freshness:?}"
            );
            if let Some(row) = &evidence.prediction.impact_freshness {
                assert_eq!(row.source, SurfaceFreshnessSource::ImpactCache);
                assert_eq!(row.id, "impact");
            }
            let impact_codes: Vec<&String> = evidence
                .limitation_codes
                .iter()
                .filter(|c| c.starts_with("impact-"))
                .collect();
            match code {
                Some(code) => assert_eq!(impact_codes, vec![code], "{freshness:?}"),
                None => assert!(impact_codes.is_empty(), "{freshness:?}"),
            }
        }
    }

    #[test]
    fn selection_evidence_soundness_limitation_always_present() {
        let steps = steps_of(&[FMT, SCOPED]);
        let bases: Vec<SelectionEvidenceInputs<'_>> = {
            let mut v = Vec::new();
            let mut scoped = inputs(&steps, &[]);
            scoped.preview = true;
            v.push(scoped.clone());
            let mut full = scoped.clone();
            full.scope_requested = VerifyScope::Full;
            v.push(full);
            let mut fallback = scoped.clone();
            fallback.fallback_reason = Some("fallback");
            v.push(fallback);
            let mut refused = scoped.clone();
            refused.refused = true;
            v.push(refused);
            let mut explicit = scoped.clone();
            explicit.plan_source = Some(PlanSource::ExplicitConfig);
            v.push(explicit);
            let mut manual = scoped.clone();
            manual.manual_requested = true;
            v.push(manual);
            let mut unknown = scoped;
            unknown.plan_source = None;
            unknown.scope_requested = VerifyScope::Fast;
            v.push(unknown);
            v
        };
        for input in bases {
            let evidence = build_selection_evidence(input);
            assert!(
                evidence
                    .limitation_codes
                    .iter()
                    .any(|c| c == SELECTION_SOUNDNESS_LIMITATION),
                "{:?}",
                evidence.basis
            );
            assert!(
                !evidence
                    .limitation_codes
                    .iter()
                    .any(|c| c == "prediction-warnings-present")
            );
            let mut sorted = evidence.limitation_codes.clone();
            sorted.sort();
            sorted.dedup();
            assert_eq!(sorted, evidence.limitation_codes);
        }

        let mut warned = inputs(&steps, &[]);
        warned.preview = true;
        warned.warning_count = 2;
        let evidence = build_selection_evidence(warned);
        assert_eq!(evidence.prediction.warning_count, 2);
        assert!(
            evidence
                .limitation_codes
                .iter()
                .any(|c| c == "prediction-warnings-present")
        );

        let mut skipped = inputs(&steps, &[]);
        skipped.preview = true;
        skipped.prediction_state = PredictionState::Skipped;
        let evidence = build_selection_evidence(skipped);
        assert!(
            evidence
                .limitation_codes
                .iter()
                .any(|c| c == "prediction-not-run")
        );
    }
}
