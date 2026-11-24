//! Closed capture projection shared by the writer and passive reader.
use crate::index::surface_freshness::SurfaceFreshnessStatus;
use crate::verify::selection_evidence::{PredictionState, SelectionBasis, StepKindJson, StepState};
use serde::{Deserialize, Deserializer, Serialize};

pub(crate) const PAYLOAD_CAP: usize = 16 * 1024;
pub(crate) const OMISSION_CODES: &[&str] = &[
    "refused-no-commands-run",
    "fallback-to-full",
    "no-test-command-planned",
    "steps-not-run",
    "result-correspondence-unknown",
];
pub(crate) const LIMITATION_CODES: &[&str] = &[
    "selection-soundness-not-established",
    "impact-stale",
    "impact-corrupt",
    "impact-missing",
    "impact-freshness-not-evaluated",
    "prediction-not-run",
    "prediction-warnings-present",
];

fn decode_omissions<'de, D: Deserializer<'de>>(decoder: D) -> Result<Vec<&'static str>, D::Error> {
    decode_codes(decoder, OMISSION_CODES)
}
fn decode_limitations<'de, D: Deserializer<'de>>(
    decoder: D,
) -> Result<Vec<&'static str>, D::Error> {
    decode_codes(decoder, LIMITATION_CODES)
}
fn decode_codes<'de, D: Deserializer<'de>>(
    decoder: D,
    allowed: &[&'static str],
) -> Result<Vec<&'static str>, D::Error> {
    let raw = Vec::<String>::deserialize(decoder)?;
    closed_codes(raw.iter().map(String::as_str), allowed)
        .ok_or_else(|| serde::de::Error::custom("invalid capture codes"))
}

pub(crate) fn closed_codes<'a>(
    codes: impl Iterator<Item = &'a str>,
    allowed: &[&'static str],
) -> Option<Vec<&'static str>> {
    let mut output = Vec::new();
    for code in codes {
        let code = allowed
            .iter()
            .copied()
            .find(|candidate| *candidate == code)?;
        if output.contains(&code) {
            return None;
        }
        output.push(code);
    }
    Some(output)
}

pub(crate) fn payload_digest(json: &str) -> String {
    format!("blake3:{}", blake3::hash(json.as_bytes()).to_hex())
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "web", derive(utoipa::ToSchema))]
#[serde(rename_all = "kebab-case")]
pub(crate) enum AttemptKind {
    Executed,
    DryRun,
    Refused,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "web", derive(utoipa::ToSchema))]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Scope {
    Fast,
    Full,
    Refused,
}

#[derive(Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "web", derive(utoipa::ToSchema))]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Label {
    #[serde(rename = "git --version")]
    GitVersion,
    Withheld,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Projection {
    pub(crate) schema_version: u32,
    pub(crate) selection_schema_version: u32,
    pub(crate) attempt_kind: AttemptKind,
    pub(crate) scope_requested: Scope,
    pub(crate) scope_executed: Scope,
    pub(crate) basis: SelectionBasis,
    pub(crate) prediction: Prediction,
    pub(crate) steps: Vec<Step>,
    #[serde(deserialize_with = "decode_omissions")]
    pub(crate) omissions: Vec<&'static str>,
    #[serde(deserialize_with = "decode_limitations")]
    pub(crate) limitation_codes: Vec<&'static str>,
    pub(crate) partial: bool,
    pub(crate) sample: Sample,
}

#[derive(Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "web", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Prediction {
    pub(crate) state: PredictionState,
    pub(crate) warning_count: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) impact_freshness: Option<SurfaceFreshnessStatus>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Step {
    pub(crate) index: u64,
    pub(crate) command: Label,
    pub(crate) kind: StepKindJson,
    pub(crate) state: StepState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) exit_code: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) duration_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) timed_out: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "web", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Sample {
    pub(crate) complete: bool,
    pub(crate) omitted: usize,
}
