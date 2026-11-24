//! Wire DTO for the independently bound verification section.
use super::attempt_projection::{AttemptKind, Label, Prediction, Projection, Sample, Scope, Step};
use crate::state::storage::change_evidence::attempts::{
    AttemptHistory, AttemptState, ObservedAttempt,
};
use crate::verify::selection_evidence::{SelectionBasis, StepKindJson, StepState};
use chrono::{DateTime, TimeDelta, Utc};
use serde::Serialize;
use uuid::Uuid;

#[derive(Serialize)]
#[cfg_attr(feature = "web", derive(utoipa::ToSchema))]
#[serde(tag = "status", rename_all = "kebab-case")]
pub(crate) enum VerificationSection {
    Recorded {
        attempts: Vec<VerificationAttempt>,
        sample: HistorySample,
    },
    NotRecorded {
        #[serde(rename = "reasonCode")]
        reason_code: NotRecordedReason,
    },
    Unavailable {
        #[serde(rename = "reasonCode")]
        reason_code: UnavailableReason,
    },
}

#[derive(Serialize)]
#[cfg_attr(feature = "web", derive(utoipa::ToSchema))]
#[serde(rename_all = "kebab-case")]
pub(crate) enum NotRecordedReason {
    RevisionNotCaptured,
}

#[derive(Serialize)]
#[cfg_attr(feature = "web", derive(utoipa::ToSchema))]
#[serde(rename_all = "kebab-case")]
pub(crate) enum UnavailableReason {
    AttemptsUnavailable,
}

#[derive(Serialize)]
#[cfg_attr(feature = "web", derive(utoipa::ToSchema))]
pub(crate) struct HistorySample {
    complete: bool,
}

impl VerificationSection {
    pub(crate) fn from_history(
        history: Option<AttemptHistory>,
        now: DateTime<Utc>,
        stale_after: TimeDelta,
    ) -> Self {
        let Some(history) = history else {
            return Self::Unavailable {
                reason_code: UnavailableReason::AttemptsUnavailable,
            };
        };
        if history.attempts.is_empty() {
            return Self::NotRecorded {
                reason_code: NotRecordedReason::RevisionNotCaptured,
            };
        }
        let Some(attempts) = history
            .attempts
            .into_iter()
            .map(|attempt| VerificationAttempt::from_storage(attempt, now, stale_after))
            .collect()
        else {
            return Self::Unavailable {
                reason_code: UnavailableReason::AttemptsUnavailable,
            };
        };
        Self::Recorded {
            attempts,
            sample: HistorySample {
                complete: history.complete,
            },
        }
    }
}

#[derive(Serialize)]
#[cfg_attr(feature = "web", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub(crate) struct VerificationAttempt {
    #[cfg_attr(feature = "web", schema(value_type = String, format = "uuid"))]
    attempt_id: Uuid,
    attempt_kind: AttemptKind,
    #[cfg_attr(feature = "web", schema(value_type = Option<String>, format = DateTime, required = true))]
    observed_from: Option<DateTime<Utc>>,
    #[cfg_attr(feature = "web", schema(value_type = Option<String>, format = DateTime, required = true))]
    observed_to: Option<DateTime<Utc>>,
    age_status: AgeStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    age_reason: Option<AgeReason>,
    state: AttemptStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    scope_requested: Option<Scope>,
    #[serde(skip_serializing_if = "Option::is_none")]
    scope_executed: Option<Scope>,
    #[serde(skip_serializing_if = "Option::is_none")]
    run_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tx_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    selection_evidence: Option<SelectionEvidence>,
    #[serde(skip_serializing_if = "Option::is_none")]
    outcomes: Option<Vec<Outcome>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    sample: Option<Sample>,
}

#[derive(Serialize)]
#[cfg_attr(feature = "web", derive(utoipa::ToSchema))]
#[serde(rename_all = "kebab-case")]
enum AgeStatus {
    Current,
    Stale,
    Unknown,
}
#[derive(Serialize)]
#[cfg_attr(feature = "web", derive(utoipa::ToSchema))]
#[serde(rename_all = "kebab-case")]
enum AgeReason {
    AttemptTimeInvalid,
    AttemptTimeFuture,
}
#[derive(Serialize)]
#[cfg_attr(feature = "web", derive(utoipa::ToSchema))]
#[serde(rename_all = "kebab-case")]
enum AttemptStatus {
    Recorded,
    Partial,
    Planned,
    Refused,
    RunRecordLost,
}

#[derive(Serialize)]
#[cfg_attr(feature = "web", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
struct SelectionEvidence {
    schema_version: u32,
    basis: SelectionBasis,
    prediction: Prediction,
    steps: Vec<SelectionStep>,
    omissions: Vec<&'static str>,
    limitation_codes: Vec<&'static str>,
}
#[derive(Serialize)]
#[cfg_attr(feature = "web", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
struct SelectionStep {
    index: u64,
    command: Label,
    kind: StepKindJson,
    state: StepState,
}
#[derive(Serialize)]
#[cfg_attr(feature = "web", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
struct Outcome {
    index: u64,
    exit_code: i32,
    duration_ms: u64,
    timed_out: bool,
}

impl VerificationAttempt {
    fn from_storage(
        attempt: ObservedAttempt,
        now: DateTime<Utc>,
        stale_after: TimeDelta,
    ) -> Option<Self> {
        let (age_status, age_reason) = match (attempt.observed_from, attempt.observed_to) {
            (Some(from), Some(to)) if from <= to => {
                if to > now {
                    (AgeStatus::Unknown, Some(AgeReason::AttemptTimeFuture))
                } else if now - to > stale_after {
                    (AgeStatus::Stale, None)
                } else {
                    (AgeStatus::Current, None)
                }
            }
            _ => (AgeStatus::Unknown, Some(AgeReason::AttemptTimeInvalid)),
        };
        let (kind, state, run_id, tx_id) = match attempt.selected.state {
            AttemptState::NotRecorded => return None,
            AttemptState::Planned {
                kind: AttemptKind::DryRun,
            } => (AttemptKind::DryRun, AttemptStatus::Planned, None, None),
            AttemptState::Planned {
                kind: AttemptKind::Refused,
            } => (AttemptKind::Refused, AttemptStatus::Refused, None, None),
            AttemptState::Planned {
                kind: AttemptKind::Executed,
            } => return None,
            AttemptState::RunRecordLost { kind } => {
                (kind, AttemptStatus::RunRecordLost, None, None)
            }
            AttemptState::Recorded { run_id, tx_id, .. } => {
                let state = if attempt.projection.as_ref()?.partial {
                    AttemptStatus::Partial
                } else {
                    AttemptStatus::Recorded
                };
                (
                    AttemptKind::Executed,
                    state,
                    Some(run_id),
                    tx_id.map(|id| id.as_str().to_owned()),
                )
            }
        };
        let mut dto = Self {
            attempt_id: attempt.selected.attempt_id,
            attempt_kind: kind,
            observed_from: attempt.observed_from,
            observed_to: attempt.observed_to,
            age_status,
            age_reason,
            state,
            run_id,
            tx_id,
            scope_requested: None,
            scope_executed: None,
            selection_evidence: None,
            outcomes: None,
            sample: None,
        };
        if let Some(projection) = attempt.projection {
            let Projection {
                selection_schema_version,
                scope_requested,
                scope_executed,
                basis,
                prediction,
                steps,
                omissions,
                limitation_codes,
                sample,
                ..
            } = projection;
            let mut outcomes = Vec::with_capacity(attempt.outcomes.len());
            let mut selected_steps = Vec::with_capacity(steps.len());
            for Step {
                index,
                command,
                kind,
                state,
                exit_code,
                duration_ms,
                timed_out,
            } in steps
            {
                if let (Some(exit_code), Some(duration_ms), Some(timed_out)) =
                    (exit_code, duration_ms, timed_out)
                {
                    // The SQLite snapshot already checked these values against the canonical
                    // outcome for this exact run and position.
                    outcomes.push(Outcome {
                        index,
                        exit_code,
                        duration_ms,
                        timed_out,
                    });
                }
                selected_steps.push(SelectionStep {
                    index,
                    command,
                    kind,
                    state,
                });
            }
            dto.scope_requested = Some(scope_requested);
            dto.scope_executed = Some(scope_executed);
            dto.sample = Some(sample);
            dto.outcomes = Some(outcomes);
            dto.selection_evidence = Some(SelectionEvidence {
                schema_version: selection_schema_version,
                basis,
                prediction,
                steps: selected_steps,
                omissions,
                limitation_codes,
            });
        }
        Some(dto)
    }
}
