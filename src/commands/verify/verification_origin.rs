use super::super::diagnostic::{
    VerifyDryRunJson, VerifyDryRunStepJson, diagnostic_schema_version, verify_dry_run_kind,
};
use super::super::dto::VerifyCliJson;
use super::capture_ack::{CaptureAck, root_json};
use super::{SelectionInvocation, WorkingTreeMaterial, execution_assurance_for};
use crate::change_evidence::attempt_projection::{
    AttemptKind, LIMITATION_CODES, Label, OMISSION_CODES, PAYLOAD_CAP, Prediction, Projection,
    Sample, Scope, Step, closed_codes, payload_digest,
};
use crate::change_evidence::pack_store::capture_db::QualifiedCaptureStorage;
use crate::state::layout::Layout;
use crate::state::storage::verification::{
    AttemptIdentity, CaptureUnavailable, PlanningCaptureCommit, PreparedExecutedAttempt,
};
use crate::verify::assurance::ExecutionAssuranceJson;
use crate::verify::engine::{
    CompletedExecution, ExecutePostflight, ExecutionTerminal, MAX_CAPTURE_POSITIONS,
};
use crate::verify::plan::{VerificationPlan, VerificationStep};
use crate::verify::results::VerificationReport;
use crate::verify::selection_evidence::{
    SelectionBasis, SelectionEvidenceJson, StepKindJson, StepState,
};
use miette::Result as CommandResult;
use std::cell::Cell;

pub(super) type CaptureObserver<'a> = dyn FnMut(Result<Projection, ProjectionError>) + 'a;

pub(super) struct PlanningCaptureAuthority<'a> {
    pub(super) prove: Box<ExecutedCapture<'a>>,
    pub(super) completed: Box<dyn FnOnce(PlanningCaptureCommit) + 'a>,
}

pub(super) fn persist_planning_projection(
    layout: &Layout,
    projection: Result<Projection, ProjectionError>,
    authority: PlanningCaptureAuthority<'_>,
) -> PlanningCaptureCommit {
    let result = (|| {
        let payload = projection
            .map_err(|_| CaptureUnavailable::InvalidOrigin)?
            .into_payload()?;
        let proof = (authority.prove)(layout)?;
        Ok(proof.storage.write_planning(proof.identity, payload))
    })();
    let committed = result.unwrap_or_else(PlanningCaptureCommit::Unavailable);
    (authority.completed)(committed);
    committed
}

/// The future proof must retain the exact existed-at-qualification writable
/// connection. The persistence path never opens or provisions a replacement.
pub(super) struct CaptureAuthority<'a> {
    pub(super) identity: AttemptIdentity,
    pub(super) storage: QualifiedCaptureStorage<'a>,
}

pub(super) type ExecutedCapture<'a> =
    dyn FnOnce(&Layout) -> Result<CaptureAuthority<'a>, CaptureUnavailable> + 'a;

pub(crate) struct AttemptPayload {
    json: String,
    digest: String,
    version: u32,
    positions: Vec<u64>,
    kind: AttemptKind,
}

impl AttemptPayload {
    #[cfg(test)]
    pub(crate) fn from_storage_test_projection(projection: Projection) -> Self {
        projection.into_payload().unwrap()
    }

    pub(crate) fn kind(&self) -> &'static str {
        match self.kind {
            AttemptKind::Executed => "executed",
            AttemptKind::DryRun => "dry-run",
            AttemptKind::Refused => "refused",
        }
    }

    pub(crate) fn json(&self) -> &str {
        &self.json
    }
    pub(crate) fn digest(&self) -> &str {
        &self.digest
    }
    pub(crate) fn version(&self) -> u32 {
        self.version
    }
    pub(crate) fn positions(&self) -> &[u64] {
        &self.positions
    }
}

pub(super) fn prepare_executed<'call, 'scope: 'call>(
    selection: SelectionInvocation<'call>,
    matched_steps: Option<usize>,
    capture: Box<ExecutedCapture<'scope>>,
) -> Box<ExecutePostflight<'call, 'scope>> {
    Box::new(move |layout, execution| {
        let authority = capture(layout)?;
        let report = execution.report();
        let assurance = execution_assurance_for(
            execution.steps(),
            &report.results,
            report.plan.as_ref().and_then(|plan| plan.source.clone()),
            execution.manual_requested(),
            false,
            false,
        );
        let root = executed_json(execution, selection, matched_steps, assurance, None);
        let projection = CaptureRoot::Executed(&root, execution)
            .project()
            .map_err(|_| CaptureUnavailable::InvalidOrigin)?;
        let attempt = PreparedExecutedAttempt::new(authority.identity, projection.into_payload()?)?;
        Ok((authority.storage, attempt))
    })
}

pub(super) fn executed_json(
    execution: &CompletedExecution<'_>,
    selection: SelectionInvocation<'_>,
    matched_steps: Option<usize>,
    assurance: ExecutionAssuranceJson,
    capture: Option<&mut CaptureObserver<'_>>,
) -> VerifyCliJson {
    let selection = SelectionInvocation {
        scope: execution.scope(),
        manual_requested: execution.manual_requested(),
        ..selection
    };
    let report = execution.report();
    let source = selection.build(
        execution.steps(),
        &report.results,
        report.plan.as_ref(),
        false,
        report.prediction_warnings.len(),
    );
    let root = VerifyCliJson::from_report(report, selection.scope, matched_steps, assurance)
        .with_selection_evidence(source);
    offer_capture(capture, CaptureRoot::Executed(&root, execution));
    root
}

pub(super) fn refused_json(
    report: &VerificationReport,
    steps: &[VerificationStep],
    selection: SelectionInvocation<'_>,
    warning_count: usize,
    matched_steps: Option<usize>,
    assurance: ExecutionAssuranceJson,
    capture: Option<&mut CaptureObserver<'_>>,
) -> VerifyCliJson {
    let source = selection.build(
        steps,
        &report.results,
        report.plan.as_ref(),
        false,
        warning_count,
    );
    let root = VerifyCliJson::from_report(report, selection.scope, matched_steps, assurance)
        .with_selection_evidence(source);
    offer_capture(capture, CaptureRoot::Refused(&root, report));
    root
}

#[allow(clippy::too_many_arguments)]
pub(super) fn emit_verify_dry_run_json(
    steps: &[VerificationStep],
    plan: Option<&VerificationPlan>,
    selection: SelectionInvocation<'_>,
    warning_count: usize,
    matched_steps: Option<usize>,
    dataset_keys: Option<usize>,
    material: WorkingTreeMaterial,
    prediction_gate: bool,
    isolated_prediction: bool,
    assurance: &crate::verify::assurance::ExecutionAssuranceJson,
    capture: Option<&mut CaptureObserver<'_>>,
    capture_ack: Option<&Cell<CaptureAck>>,
    json: bool,
) -> CommandResult<()> {
    let refused = plan.is_some_and(|plan| plan.refused);
    let fallback_reason = plan
        .and_then(|plan| plan.fallback_reason.clone())
        .or_else(|| refused.then(|| "fast scope unavailable; refusing full suite".to_string()));
    let scope = selection.scope;
    let git_available = !matches!(material, WorkingTreeMaterial::Unavailable);
    let clean_tree = match material {
        WorkingTreeMaterial::Clean => Some(true),
        WorkingTreeMaterial::Dirty => Some(false),
        WorkingTreeMaterial::Unavailable => None,
    };
    // Caller already applied the 0288 gate (Clean or Unavailable skip predict).
    // Do not re-AND with Clean — that would report predictionSkipped:false after
    // an Unavailable skip. Reason is cleanTree only when the tree is actually clean.
    let prediction_skipped = prediction_gate || isolated_prediction;
    let prediction_skip_reason = if prediction_gate
        && !isolated_prediction
        && matches!(material, WorkingTreeMaterial::Clean)
    {
        Some("cleanTree".to_string())
    } else {
        None
    };
    let scope_executed = if refused {
        "refused".to_string()
    } else if fallback_reason.is_some() {
        "full".to_string()
    } else {
        scope.to_string()
    };
    let planned: Vec<VerifyDryRunStepJson> = if refused {
        Vec::new()
    } else {
        steps
            .iter()
            .map(|s| VerifyDryRunStepJson {
                name: crate::verify::fail_block::step_name_from_command(&s.command),
                command: s.command.clone(),
                status: "planned".to_string(),
                timeout_secs: s.timeout_secs,
                budget_source: s.budget_source.clone(),
            })
            .collect()
    };
    let payload = VerifyDryRunJson {
        schema_version: diagnostic_schema_version(),
        kind: verify_dry_run_kind(),
        scope_requested: scope.to_string(),
        scope_executed,
        fallback_reason,
        refused,
        executed: false,
        git_available,
        clean_tree,
        prediction_skipped,
        prediction_skip_reason,
        matched_steps,
        dataset_keys,
        steps: planned,
        execution_assurance: Some(assurance.clone()),
        selection_evidence: Some(selection.build(steps, &[], plan, true, warning_count)),
    };
    offer_capture(capture, CaptureRoot::Preview(&payload));
    if json {
        if let Some(ack) = capture_ack {
            println!("{}", root_json(&payload, ack.get())?);
        } else {
            println!("{}", payload.to_json_string()?);
        }
    }
    Ok(())
}

enum CaptureRoot<'a> {
    Executed(&'a VerifyCliJson, &'a CompletedExecution<'a>),
    Refused(&'a VerifyCliJson, &'a VerificationReport),
    Preview(&'a VerifyDryRunJson),
}

fn offer_capture(observer: Option<&mut CaptureObserver<'_>>, root: CaptureRoot<'_>) {
    if let Some(observer) = observer {
        observer(root.project());
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ProjectionError {
    UnsupportedSchema,
    InvalidScope,
    InvalidEvidence,
    InvalidTerminal,
}

impl Projection {
    fn into_payload(self) -> Result<AttemptPayload, CaptureUnavailable> {
        let positions = self
            .steps
            .iter()
            .filter(|step| step.exit_code.is_some())
            .map(|step| step.index)
            .collect();
        let json = serde_json::to_string(&self).map_err(|_| CaptureUnavailable::InvalidOrigin)?;
        // The producer permits at most 32 closed-label rows and fixed code lists.
        if json.len() > PAYLOAD_CAP {
            return Err(CaptureUnavailable::InvalidOrigin);
        }
        let digest = payload_digest(&json);
        Ok(AttemptPayload {
            json,
            digest,
            version: self.schema_version,
            positions,
            kind: self.attempt_kind,
        })
    }
}

impl CaptureRoot<'_> {
    fn project(self) -> Result<Projection, ProjectionError> {
        match self {
            Self::Executed(root, execution) => {
                let terminal = execution
                    .terminal()
                    .ok_or(ProjectionError::InvalidTerminal)?;
                let report = execution.report();
                if !terminal.is_consistent()
                    || terminal.planned() != execution.steps().len()
                    || terminal.completed() != report.results.len()
                    || root.steps.len() != report.results.len()
                    || root.ok != report.overall_pass
                    || report.plan.as_ref().is_some_and(|p| p.refused)
                {
                    return Err(ProjectionError::InvalidTerminal);
                }
                let source = root
                    .selection_evidence
                    .as_ref()
                    .ok_or(ProjectionError::InvalidEvidence)?;
                if source.steps.len() != terminal.planned()
                    || source.prediction.warning_count != report.prediction_warnings.len() as u64
                {
                    return Err(ProjectionError::InvalidEvidence);
                }
                project(
                    RootFields {
                        version: root.schema_version,
                        requested: &root.scope_requested,
                        executed: &root.scope_executed,
                        fallback: root.fallback_reason.is_some(),
                        source,
                    },
                    Branch::Executed(terminal),
                )
            }
            Self::Refused(root, report) => {
                if !report.plan.as_ref().is_some_and(|p| p.refused)
                    || report.overall_pass
                    || !report.results.is_empty()
                    || !root.steps.is_empty()
                    || root.ok
                {
                    return Err(ProjectionError::InvalidEvidence);
                }
                project(
                    RootFields {
                        version: root.schema_version,
                        requested: &root.scope_requested,
                        executed: &root.scope_executed,
                        fallback: root.fallback_reason.is_some(),
                        source: root
                            .selection_evidence
                            .as_ref()
                            .ok_or(ProjectionError::InvalidEvidence)?,
                    },
                    Branch::Refused,
                )
            }
            Self::Preview(root) => {
                let source = root
                    .selection_evidence
                    .as_ref()
                    .ok_or(ProjectionError::InvalidEvidence)?;
                if root.executed || root.steps.len() != source.steps.len() {
                    return Err(ProjectionError::InvalidEvidence);
                }
                project(
                    RootFields {
                        version: root.schema_version,
                        requested: &root.scope_requested,
                        executed: &root.scope_executed,
                        fallback: root.fallback_reason.is_some(),
                        source,
                    },
                    if root.refused {
                        Branch::Refused
                    } else {
                        Branch::DryRun
                    },
                )
            }
        }
    }
}

struct RootFields<'a> {
    version: u32,
    requested: &'a str,
    executed: &'a str,
    fallback: bool,
    source: &'a SelectionEvidenceJson,
}

enum Branch<'a> {
    Executed(&'a ExecutionTerminal),
    DryRun,
    Refused,
}

fn project(root: RootFields<'_>, branch: Branch<'_>) -> Result<Projection, ProjectionError> {
    let source = root.source;
    if root.version != 1 || source.schema_version != 1 {
        return Err(ProjectionError::UnsupportedSchema);
    }
    let requested = match root.requested {
        "fast" => Scope::Fast,
        "full" => Scope::Full,
        _ => return Err(ProjectionError::InvalidScope),
    };
    let refused = matches!(branch, Branch::Refused);
    let executed = if refused {
        Scope::Refused
    } else if root.fallback {
        Scope::Full
    } else {
        requested
    };
    let expected_scope = match executed {
        Scope::Fast => "fast",
        Scope::Full => "full",
        Scope::Refused => "refused",
    };
    if root.executed != expected_scope {
        return Err(ProjectionError::InvalidScope);
    }
    validate_source(source, &branch, requested, root.fallback)?;
    let attempt_kind = match branch {
        Branch::Executed(_) => AttemptKind::Executed,
        Branch::DryRun => AttemptKind::DryRun,
        Branch::Refused => AttemptKind::Refused,
    };
    let terminal = match branch {
        Branch::Executed(terminal) => Some(terminal),
        _ => None,
    };
    let partial = terminal.is_some_and(ExecutionTerminal::partial);
    let mut omissions = closed_codes(
        source.omissions.iter().map(|o| o.code.as_str()),
        OMISSION_CODES,
    )
    .ok_or(ProjectionError::InvalidEvidence)?;
    // 0600's legacy matching uses displayed command text. Keep its basis
    // and classifications. The capture outcome and suffix use engine proof.
    omissions.retain(|code| !matches!(*code, "steps-not-run" | "result-correspondence-unknown"));
    if partial {
        omissions.push("steps-not-run");
    }
    let limitation_codes = closed_codes(
        source.limitation_codes.iter().map(String::as_str),
        LIMITATION_CODES,
    )
    .ok_or(ProjectionError::InvalidEvidence)?;
    if !limitation_codes.contains(&"selection-soundness-not-established")
        || omissions.contains(&"refused-no-commands-run") != refused
        || omissions.contains(&"fallback-to-full") != (!refused && root.fallback)
    {
        return Err(ProjectionError::InvalidEvidence);
    }
    let steps: Vec<_> = source
        .steps
        .iter()
        .take(MAX_CAPTURE_POSITIONS)
        .enumerate()
        .map(|(position, step)| {
            let outcome = terminal.and_then(|terminal| terminal.outcomes().get(position));
            let state = match (terminal, outcome) {
                (_, Some(result)) if result.exit_code == 0 => StepState::Passed,
                (_, Some(_)) => StepState::Failed,
                (Some(_), None) => StepState::NotRun,
                (None, None) => StepState::Planned,
            };
            Step {
                index: step.index,
                command: if step.command == "git --version" {
                    Label::GitVersion
                } else {
                    Label::Withheld
                },
                kind: step.kind,
                state,
                exit_code: outcome.map(|result| result.exit_code),
                duration_ms: outcome.map(|result| result.duration_ms),
                timed_out: outcome.map(|result| result.timed_out),
            }
        })
        .collect();
    let omitted = source.steps.len() - steps.len();
    Ok(Projection {
        schema_version: 1,
        selection_schema_version: source.schema_version,
        attempt_kind,
        scope_requested: requested,
        scope_executed: executed,
        basis: source.basis,
        prediction: Prediction {
            state: source.prediction.state,
            warning_count: source.prediction.warning_count,
            impact_freshness: source
                .prediction
                .impact_freshness
                .as_ref()
                .map(|row| row.status),
        },
        steps,
        omissions,
        limitation_codes,
        partial,
        sample: Sample {
            complete: omitted == 0,
            omitted,
        },
    })
}

fn validate_source(
    source: &SelectionEvidenceJson,
    branch: &Branch<'_>,
    requested: Scope,
    fallback: bool,
) -> Result<(), ProjectionError> {
    let has_test = source
        .steps
        .iter()
        .any(|step| step.kind == StepKindJson::TestRun);
    let basis_ok = match source.basis {
        SelectionBasis::Refused => matches!(branch, Branch::Refused) && source.steps.is_empty(),
        _ if matches!(branch, Branch::Refused) => false,
        SelectionBasis::ManualCommand | SelectionBasis::ExplicitSteps => true,
        SelectionBasis::FallbackFull => fallback,
        SelectionBasis::FullRequested => !fallback && requested == Scope::Full,
        SelectionBasis::FastNoTestStep => !fallback && requested == Scope::Fast && !has_test,
        SelectionBasis::ScopedSelection | SelectionBasis::Unknown => {
            !fallback && requested == Scope::Fast && has_test
        }
    };
    if !basis_ok
        || source.steps.iter().enumerate().any(|(index, step)| {
            step.index != index as u64
                || match step.state {
                    StepState::Planned => {
                        !matches!(branch, Branch::DryRun) || step.exit_code.is_some()
                    }
                    StepState::Passed | StepState::NotRun | StepState::Unknown => {
                        !matches!(branch, Branch::Executed(_)) || step.exit_code.is_some()
                    }
                    StepState::Failed => {
                        !matches!(branch, Branch::Executed(_))
                            || step.exit_code.is_none_or(|exit| exit == 0)
                    }
                }
        })
    {
        return Err(ProjectionError::InvalidEvidence);
    }
    Ok(())
}
