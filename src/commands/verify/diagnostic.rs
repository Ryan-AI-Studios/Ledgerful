//! Diagnostic `verify --json` envelopes (0321).
//!
//! Sibling kinds of executed [`VerifyCliJson`]: required `kind`, schemaVersion 1.
//! Do not add `kind` to the plan-execution payload.

use miette::{IntoDiagnostic, Result};
use serde::{Deserialize, Serialize};

use super::dto::VERIFY_JSON_SCHEMA_VERSION;

const BREAK_CAP: usize = 20;
const INVALID_SAMPLE_CAP: usize = 20;

/// Planned dry-run step — never `pass`/`fail`; no duration/exit.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct VerifyDryRunStepJson {
    pub name: String,
    pub command: String,
    pub status: String,
}

/// `kind: "verifyDryRun"` — not a verification. No `ok`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct VerifyDryRunJson {
    pub schema_version: u32,
    pub kind: String,
    pub scope_requested: String,
    pub scope_executed: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fallback_reason: Option<String>,
    pub refused: bool,
    pub executed: bool,
    pub git_available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clean_tree: Option<bool>,
    pub prediction_skipped: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prediction_skip_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matched_steps: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dataset_keys: Option<usize>,
    pub steps: Vec<VerifyDryRunStepJson>,
}

impl VerifyDryRunJson {
    pub fn to_json_string(&self) -> Result<String> {
        serde_json::to_string_pretty(self).into_diagnostic()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct VerifyHealthToolJson {
    pub name: String,
    pub available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct VerifyHealthLedgerJson {
    pub clean: bool,
    pub unaudited_count: usize,
    pub stale_pending: bool,
    pub no_impact_report: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct VerifyHealthRunnerJson {
    pub selected: String,
    pub nextest_available: bool,
    pub prefer_nextest: bool,
}

/// `kind: "verifyHealth"` — CLI tool/ledger probe (not `GET /api/verify/health`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct VerifyHealthJson {
    pub schema_version: u32,
    pub kind: String,
    pub ok: bool,
    pub tools: Vec<VerifyHealthToolJson>,
    pub ledger: VerifyHealthLedgerJson,
    pub runner: VerifyHealthRunnerJson,
}

impl VerifyHealthJson {
    pub fn to_json_string(&self) -> Result<String> {
        serde_json::to_string_pretty(self).into_diagnostic()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ChainBreakKind {
    ExtraGenesis,
    Orphan,
    Fork,
    MissingPrev,
    HashMismatch,
    GenesisHasPrev,
}

impl ChainBreakKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ExtraGenesis => "extraGenesis",
            Self::Orphan => "orphan",
            Self::Fork => "fork",
            Self::MissingPrev => "missingPrev",
            Self::HashMismatch => "hashMismatch",
            Self::GenesisHasPrev => "genesisHasPrev",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct VerifyChainBreakJson {
    pub tx_id: String,
    pub kind: ChainBreakKind,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct VerifySignatureTrustJson {
    pub trusted: usize,
    pub unknown_key: usize,
    pub pin_empty: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct VerifySignaturesDimensionJson {
    pub checked: bool,
    pub valid: usize,
    pub invalid: usize,
    pub unsigned: usize,
    pub skipped: usize,
    pub federated_skip: usize,
    pub trust: VerifySignatureTrustJson,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub invalid_samples: Vec<String>,
}

impl VerifySignaturesDimensionJson {
    pub fn unchecked() -> Self {
        Self {
            checked: false,
            valid: 0,
            invalid: 0,
            unsigned: 0,
            skipped: 0,
            federated_skip: 0,
            trust: VerifySignatureTrustJson::default(),
            invalid_samples: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct VerifyChainDimensionJson {
    pub checked: bool,
    pub linked_entries: i64,
    pub extra_genesis_count: usize,
    pub break_count: usize,
    pub breaks: Vec<VerifyChainBreakJson>,
}

impl VerifyChainDimensionJson {
    pub fn unchecked() -> Self {
        Self {
            checked: false,
            linked_entries: 0,
            extra_genesis_count: 0,
            break_count: 0,
            breaks: Vec::new(),
        }
    }
}

pub use crate::ledger::chain_checkpoint::CheckpointResultKind;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct VerifyCheckpointJson {
    pub compared: bool,
    pub mode: String,
    pub result: CheckpointResultKind,
}

/// `kind: "verifySignatures"` — four dimensions; `ok` ⇔ exit 0.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct VerifySignaturesJson {
    pub schema_version: u32,
    pub kind: String,
    pub ok: bool,
    pub exit_code: i32,
    pub strict: bool,
    pub signatures: VerifySignaturesDimensionJson,
    pub chain: VerifyChainDimensionJson,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checkpoint: Option<VerifyCheckpointJson>,
}

impl VerifySignaturesJson {
    pub fn to_json_string(&self) -> Result<String> {
        serde_json::to_string_pretty(self).into_diagnostic()
    }
}

/// Cap + sort chain breaks by `txId` (full `break_count` kept separately).
pub fn cap_sorted_breaks(mut breaks: Vec<VerifyChainBreakJson>) -> Vec<VerifyChainBreakJson> {
    breaks.sort_by(|a, b| {
        a.tx_id
            .cmp(&b.tx_id)
            .then_with(|| a.kind.as_str().cmp(b.kind.as_str()))
    });
    breaks.truncate(BREAK_CAP);
    breaks
}

/// First 20 invalid/unsigned-fail txIds, sorted.
pub fn cap_sorted_invalid_samples(mut tx_ids: Vec<String>) -> Vec<String> {
    tx_ids.sort();
    tx_ids.dedup();
    tx_ids.truncate(INVALID_SAMPLE_CAP);
    tx_ids
}

pub fn verify_dry_run_kind() -> String {
    "verifyDryRun".to_string()
}

pub fn verify_health_kind() -> String {
    "verifyHealth".to_string()
}

pub fn verify_signatures_kind() -> String {
    "verifySignatures".to_string()
}

pub fn diagnostic_schema_version() -> u32 {
    VERIFY_JSON_SCHEMA_VERSION
}

/// Shared mix-refuse for dispatch (first) and execute (health+dry-run).
pub fn refuse_mixed_verify_diagnostics(
    health: bool,
    dry_run: bool,
    signatures: bool,
    chain: bool,
    against_export: bool,
) -> Result<()> {
    let evidence = signatures || chain || against_export;
    if health && dry_run {
        return Err(miette::miette!(
            "verify --health cannot be combined with --dry-run"
        ));
    }
    if health && evidence {
        return Err(miette::miette!(
            "verify --health cannot be combined with --signatures, --chain, or --against-export"
        ));
    }
    if dry_run && evidence {
        return Err(miette::miette!(
            "verify --dry-run cannot be combined with --signatures, --chain, or --against-export"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod diagnostic_dto_tests {
    use super::*;

    #[test]
    fn executed_verify_cli_json_still_has_no_kind() {
        use crate::commands::verify::test_support::{sample_report, sample_result};
        use crate::commands::verify::{VERIFY_JSON_SCHEMA_VERSION, VerifyCliJson};
        use crate::verify::plan::VerifyScope;

        let report = sample_report(vec![sample_result("cargo test", 0)], None);
        let payload = VerifyCliJson::from_report(&report, VerifyScope::Full, None);
        let json = payload.to_json_string().unwrap();
        assert_eq!(payload.schema_version, VERIFY_JSON_SCHEMA_VERSION);
        assert!(
            !json.contains("\"kind\""),
            "executed VerifyCliJson must not grow kind: {json}"
        );
    }

    #[test]
    fn dry_run_json_locks_kind_executed_false_no_ok() {
        let payload = VerifyDryRunJson {
            schema_version: diagnostic_schema_version(),
            kind: verify_dry_run_kind(),
            scope_requested: "fast".into(),
            scope_executed: "fast".into(),
            fallback_reason: None,
            refused: false,
            executed: false,
            git_available: true,
            clean_tree: Some(true),
            prediction_skipped: true,
            prediction_skip_reason: Some("cleanTree".into()),
            matched_steps: None,
            dataset_keys: None,
            steps: vec![VerifyDryRunStepJson {
                name: "fmt".into(),
                command: "cargo fmt --all -- --check".into(),
                status: "planned".into(),
            }],
        };
        let json = payload.to_json_string().unwrap();
        assert!(json.contains("\"kind\": \"verifyDryRun\""));
        assert!(json.contains("\"executed\": false"));
        assert!(json.contains("\"gitAvailable\": true"));
        assert!(json.contains("\"cleanTree\": true"));
        assert!(json.contains("\"predictionSkipped\": true"));
        assert!(!json.contains("\"ok\""), "dry-run must not emit ok: {json}");
        assert!(!json.contains("durationMs"), "{json}");
        assert!(!json.contains("exitCode"), "{json}");
        assert!(json.contains("\"status\": \"planned\""));
        assert!(!json.contains("matchedSteps"));
        assert!(!json.contains("datasetKeys"));
        assert!(!json.contains("fallbackReason"));
    }

    #[test]
    fn dry_run_omits_clean_tree_when_git_unavailable() {
        let payload = VerifyDryRunJson {
            schema_version: 1,
            kind: verify_dry_run_kind(),
            scope_requested: "fast".into(),
            scope_executed: "fast".into(),
            fallback_reason: None,
            refused: false,
            executed: false,
            git_available: false,
            clean_tree: None,
            prediction_skipped: false,
            prediction_skip_reason: None,
            matched_steps: None,
            dataset_keys: None,
            steps: vec![],
        };
        let json = payload.to_json_string().unwrap();
        assert!(json.contains("\"gitAvailable\": false"));
        assert!(
            !json.contains("cleanTree"),
            "omit cleanTree when git unavailable: {json}"
        );
        assert!(json.contains("\"predictionSkipped\": false"));
    }

    #[test]
    fn health_ok_is_tools_only() {
        let payload = VerifyHealthJson {
            schema_version: 1,
            kind: verify_health_kind(),
            ok: true,
            tools: vec![
                VerifyHealthToolJson {
                    name: "cargo".into(),
                    available: true,
                    hint: None,
                },
                VerifyHealthToolJson {
                    name: "git".into(),
                    available: true,
                    hint: None,
                },
            ],
            ledger: VerifyHealthLedgerJson {
                clean: false,
                unaudited_count: 1,
                stale_pending: false,
                no_impact_report: false,
            },
            runner: VerifyHealthRunnerJson {
                selected: "cargoTest".into(),
                nextest_available: true,
                prefer_nextest: false,
            },
        };
        assert!(payload.ok, "ledger NOTE must not flip ok");
        let json = payload.to_json_string().unwrap();
        assert!(json.contains("\"kind\": \"verifyHealth\""));
        assert!(!json.contains("\"hint\""));
    }

    #[test]
    fn signatures_exit_code_on_diagnostic_only() {
        let payload = VerifySignaturesJson {
            schema_version: 1,
            kind: verify_signatures_kind(),
            ok: false,
            exit_code: 1,
            strict: false,
            signatures: VerifySignaturesDimensionJson {
                checked: true,
                valid: 2,
                invalid: 0,
                unsigned: 0,
                skipped: 0,
                federated_skip: 0,
                trust: VerifySignatureTrustJson {
                    trusted: 0,
                    unknown_key: 2,
                    pin_empty: true,
                },
                invalid_samples: vec![],
            },
            chain: VerifyChainDimensionJson {
                checked: true,
                linked_entries: 0,
                extra_genesis_count: 2,
                break_count: 2,
                breaks: vec![VerifyChainBreakJson {
                    tx_id: "aaaa".into(),
                    kind: ChainBreakKind::ExtraGenesis,
                }],
            },
            checkpoint: None,
        };
        let json = payload.to_json_string().unwrap();
        assert!(json.contains("\"kind\": \"verifySignatures\""));
        assert!(json.contains("\"exitCode\": 1"));
        assert!(json.contains("\"extraGenesis\""));
        assert!(!json.contains("checkpoint"));
        assert!(!json.contains("invalidSamples"));
        assert!(json.contains("\"strict\": false"));
    }

    #[test]
    fn cap_breaks_sorts_by_tx_id() {
        let capped = cap_sorted_breaks(vec![
            VerifyChainBreakJson {
                tx_id: "z".into(),
                kind: ChainBreakKind::Orphan,
            },
            VerifyChainBreakJson {
                tx_id: "a".into(),
                kind: ChainBreakKind::Fork,
            },
        ]);
        assert_eq!(capped[0].tx_id, "a");
        assert_eq!(capped[1].tx_id, "z");
    }

    #[test]
    fn refuse_mixed_verify_diagnostics_matrix() {
        assert!(refuse_mixed_verify_diagnostics(true, true, false, false, false).is_err());
        assert!(refuse_mixed_verify_diagnostics(true, false, true, false, false).is_err());
        assert!(refuse_mixed_verify_diagnostics(false, true, false, true, false).is_err());
        assert!(refuse_mixed_verify_diagnostics(false, false, true, false, false).is_ok());
    }
}
