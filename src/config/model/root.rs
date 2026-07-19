use serde::{Deserialize, Serialize};

pub type TomlError = toml::de::Error;

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub struct Config {
    #[serde(default)]
    pub core: super::coverage::CoreConfig,
    #[serde(default)]
    pub watch: super::coverage::WatchConfig,
    #[serde(default)]
    pub gemini: super::gemini::GeminiConfig,
    #[serde(default)]
    pub temporal: super::coverage::TemporalConfig,
    #[serde(default)]
    pub hotspots: super::coverage::HotspotsConfig,
    #[serde(default)]
    pub verify: super::verify::VerifyConfig,
    #[serde(default)]
    pub ledger: super::ledger::LedgerConfig,
    #[serde(default)]
    pub local_model: super::local_model::LocalModelConfig,
    #[serde(default)]
    pub semantic: super::semantic::SemanticConfig,
    #[serde(default)]
    pub docs: super::coverage::DocsConfig,
    #[serde(default)]
    pub observability: super::coverage::ObservabilityConfig,
    #[serde(default)]
    pub contracts: super::coverage::ContractsConfig,
    #[serde(default)]
    pub coverage: super::coverage::CoverageConfig,
    #[serde(default)]
    pub dead_code: super::coverage::DeadCodeConfig,
    #[serde(default)]
    pub index: super::coverage::IndexConfig,
    #[serde(default)]
    pub intent: super::coverage::IntentConfig,
    #[serde(default)]
    pub impact: super::coverage::ImpactConfig,
    #[serde(default)]
    pub services: super::coverage::ServiceConfig,
    #[serde(default)]
    pub sync: super::sync::SyncConfig,
    #[serde(default)]
    pub ask: super::ask::AskConfig,
    #[serde(default)]
    pub federation: super::federation::FederationConfig,
    #[serde(default)]
    pub gate: super::gate::GateConfig,
    #[serde(default)]
    pub bridge: super::bridge::BridgeConfig,
    #[serde(default)]
    pub global_rollup: super::rollup::GlobalRollupConfig,
}
