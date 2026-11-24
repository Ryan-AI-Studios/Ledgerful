use crate::change_evidence::pack_store::baseline_producer::{
    BaselineFacts, PreparationVerdict, attest, prepare,
};
use crate::cli::args::ChangeEvidenceCommands;
use crate::state::layout::get_layout_with_state_origin;
use chrono::{DateTime, Utc};
use miette::Result;
use serde::Serialize;

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
enum BaselineStatus {
    Attested,
    Prepared,
    Incomparable,
    Unavailable,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BaselineAcknowledgement<'a> {
    schema_version: u8,
    kind: &'static str,
    status: BaselineStatus,
    reason_codes: Vec<String>,
    baseline_name: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    repo_scope: Option<uuid::Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    base: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    head: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    baseline_digest: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    observed_at: Option<DateTime<Utc>>,
}

pub(crate) fn execute(command: &ChangeEvidenceCommands) -> Result<()> {
    let (name, json) = match command {
        ChangeEvidenceCommands::AttestBaseline { name, json }
        | ChangeEvidenceCommands::Prepare { name, json } => (name, *json),
    };
    let kind = match command {
        ChangeEvidenceCommands::AttestBaseline { .. } => "changeEvidenceBaselineAttestation",
        ChangeEvidenceCommands::Prepare { .. } => "changeEvidencePreparation",
    };
    let mut facts = BaselineFacts::default();
    let result = get_layout_with_state_origin()
        .map_err(|_| vec!["scope-unavailable".to_string()])
        .and_then(|(layout, origin)| match command {
            ChangeEvidenceCommands::AttestBaseline { .. } => {
                attest(&layout, origin, name, &mut facts)
                    .map(|observed| (BaselineStatus::Attested, Vec::new(), observed))
            }
            ChangeEvidenceCommands::Prepare { .. } => prepare(&layout, origin, name, &mut facts)
                .map(|(observed, verdict)| {
                    let (status, reasons) = match verdict {
                        PreparationVerdict::Prepared => (BaselineStatus::Prepared, Vec::new()),
                        PreparationVerdict::Incomparable(reasons) => {
                            (BaselineStatus::Incomparable, reasons)
                        }
                        PreparationVerdict::Unavailable(reasons) => {
                            (BaselineStatus::Unavailable, reasons)
                        }
                    };
                    (status, reasons, observed)
                }),
        });
    let (status, mut reason_codes, observed_at) = match result {
        Ok((status, reasons, observed)) => (status, reasons, Some(observed)),
        Err(reasons) => (BaselineStatus::Unavailable, reasons, None),
    };
    reason_codes.sort_unstable();
    reason_codes.dedup();
    let ack = BaselineAcknowledgement {
        schema_version: 1,
        kind,
        status,
        reason_codes,
        baseline_name: name,
        repo_scope: facts.repo_scope,
        base: facts.base,
        head: facts.head,
        baseline_digest: facts.baseline_digest,
        observed_at,
    };
    if json {
        crate::output::json::emit(&ack)?;
    }
    match ack.status {
        BaselineStatus::Attested | BaselineStatus::Prepared => {
            if !json {
                let verb = if matches!(ack.status, BaselineStatus::Attested) {
                    "Attested"
                } else {
                    "Prepared"
                };
                println!("{verb} baseline {name}");
            }
            Ok(())
        }
        BaselineStatus::Incomparable | BaselineStatus::Unavailable => Err(miette::miette!(
            "Change evidence was not prepared ({})",
            ack.reason_codes.join(", ")
        )),
    }
}
