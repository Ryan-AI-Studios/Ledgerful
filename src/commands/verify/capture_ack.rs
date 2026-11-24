use crate::state::storage::verification::{
    CaptureUnavailable, PlanningAttemptKind, PlanningCaptureCommit, VerificationCaptureCommit,
};
use miette::{IntoDiagnostic, Result};
use serde::Serialize;
use uuid::Uuid;

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(super) enum AttemptKind {
    Executed,
    DryRun,
    Refused,
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(super) enum ReasonCode {
    QualificationUnavailable,
    CaptureBudget,
    RevisionChanged,
    CheckoutUnavailable,
    PlatformUnavailable,
    PlanningStorageUnavailable,
    InvalidOrigin,
    StorageUnavailable,
    ScopeUnavailable,
    CapacityReached,
    AttemptWriteFailed,
    TransactionRolledBack,
    CommitUnconfirmed,
}

impl ReasonCode {
    fn as_str(self) -> &'static str {
        match self {
            Self::QualificationUnavailable => "qualification-unavailable",
            Self::CaptureBudget => "capture-budget",
            Self::RevisionChanged => "revision-changed",
            Self::CheckoutUnavailable => "checkout-unavailable",
            Self::PlatformUnavailable => "platform-unavailable",
            Self::PlanningStorageUnavailable => "planning-storage-unavailable",
            Self::InvalidOrigin => "invalid-origin",
            Self::StorageUnavailable => "storage-unavailable",
            Self::ScopeUnavailable => "scope-unavailable",
            Self::CapacityReached => "capacity-reached",
            Self::AttemptWriteFailed => "attempt-write-failed",
            Self::TransactionRolledBack => "transaction-rolled-back",
            Self::CommitUnconfirmed => "commit-unconfirmed",
        }
    }
}

impl From<CaptureUnavailable> for ReasonCode {
    fn from(value: CaptureUnavailable) -> Self {
        match value {
            CaptureUnavailable::InvalidOrigin => Self::InvalidOrigin,
            CaptureUnavailable::CaptureBudget => Self::CaptureBudget,
            CaptureUnavailable::RevisionChanged => Self::RevisionChanged,
            CaptureUnavailable::CheckoutUnavailable => Self::CheckoutUnavailable,
            CaptureUnavailable::PlatformUnavailable => Self::PlatformUnavailable,
            CaptureUnavailable::PlanningStorageUnavailable => Self::PlanningStorageUnavailable,
            CaptureUnavailable::InvalidQualification => Self::QualificationUnavailable,
            CaptureUnavailable::StorageUnavailable => Self::StorageUnavailable,
            CaptureUnavailable::ScopeUnavailable => Self::ScopeUnavailable,
            CaptureUnavailable::CapacityReached => Self::CapacityReached,
            CaptureUnavailable::AttemptWriteFailed => Self::AttemptWriteFailed,
        }
    }
}

#[derive(Clone, Copy, Serialize)]
#[serde(
    tag = "status",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase"
)]
pub(super) enum CaptureAck {
    Recorded {
        schema_version: u32,
        attempt_id: Uuid,
        attempt_kind: AttemptKind,
        #[serde(skip_serializing_if = "Option::is_none")]
        run_id: Option<i64>,
    },
    Unavailable {
        schema_version: u32,
        reason_code: ReasonCode,
    },
}

impl CaptureAck {
    fn unavailable(reason_code: ReasonCode) -> Self {
        Self::Unavailable {
            schema_version: 1,
            reason_code,
        }
    }

    pub(super) fn from_executed(commit: Option<VerificationCaptureCommit>) -> Self {
        match commit {
            Some(VerificationCaptureCommit::Recorded {
                attempt_id, run_id, ..
            }) => Self::Recorded {
                schema_version: 1,
                attempt_id,
                attempt_kind: AttemptKind::Executed,
                run_id: Some(run_id),
            },
            Some(VerificationCaptureCommit::Unavailable(reason)) => {
                Self::unavailable(reason.into())
            }
            Some(VerificationCaptureCommit::RolledBack(_)) => {
                Self::unavailable(ReasonCode::TransactionRolledBack)
            }
            Some(VerificationCaptureCommit::Unconfirmed(_)) => {
                Self::unavailable(ReasonCode::CommitUnconfirmed)
            }
            None => Self::unavailable(ReasonCode::QualificationUnavailable),
        }
    }

    pub(super) fn from_planning(commit: PlanningCaptureCommit) -> Self {
        match commit {
            PlanningCaptureCommit::Recorded { attempt_id, kind } => Self::Recorded {
                schema_version: 1,
                attempt_id,
                attempt_kind: match kind {
                    PlanningAttemptKind::DryRun => AttemptKind::DryRun,
                    PlanningAttemptKind::Refused => AttemptKind::Refused,
                },
                run_id: None,
            },
            PlanningCaptureCommit::Unavailable(reason) => Self::unavailable(reason.into()),
            PlanningCaptureCommit::Unconfirmed(_) => {
                Self::unavailable(ReasonCode::CommitUnconfirmed)
            }
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CapturedRoot<'a, Root> {
    #[serde(flatten)]
    root: &'a Root,
    change_evidence_capture: CaptureAck,
}

pub(super) fn root_json(root: &impl Serialize, ack: CaptureAck) -> Result<String> {
    serde_json::to_string_pretty(&CapturedRoot {
        root,
        change_evidence_capture: ack,
    })
    .into_diagnostic()
}

pub(super) fn print_human(ack: CaptureAck) {
    match ack {
        CaptureAck::Recorded { attempt_id, .. } => {
            eprintln!("Change evidence recorded ({attempt_id})")
        }
        CaptureAck::Unavailable { reason_code, .. } => {
            eprintln!("Change evidence unavailable ({})", reason_code.as_str())
        }
    }
}
