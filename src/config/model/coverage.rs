use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct DeadCodeConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_dead_code_confidence_threshold")]
    pub confidence_threshold: f64,
    #[serde(default = "default_git_inactivity_days")]
    pub git_inactivity_days: u32,
    #[serde(default = "default_reachability_weight")]
    pub reachability_weight: f64,
    #[serde(default = "default_git_activity_weight")]
    pub git_activity_weight: f64,
    #[serde(default = "default_test_coverage_weight")]
    pub test_coverage_weight: f64,
}

fn default_dead_code_confidence_threshold() -> f64 {
    0.75
}
fn default_git_inactivity_days() -> u32 {
    90
}
fn default_reachability_weight() -> f64 {
    1.0
}
fn default_git_activity_weight() -> f64 {
    1.0
}
fn default_test_coverage_weight() -> f64 {
    1.0
}

impl Default for DeadCodeConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            confidence_threshold: default_dead_code_confidence_threshold(),
            git_inactivity_days: default_git_inactivity_days(),
            reachability_weight: default_reachability_weight(),
            git_activity_weight: default_git_activity_weight(),
            test_coverage_weight: default_test_coverage_weight(),
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct IndexConfig {
    /// Number of days after which the Tantivy/CozoDB index is considered stale.
    #[serde(default = "default_stale_threshold_days")]
    pub stale_threshold_days: u64,
}

fn default_stale_threshold_days() -> u64 {
    3
}

impl Default for IndexConfig {
    fn default() -> Self {
        Self {
            stale_threshold_days: default_stale_threshold_days(),
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ImpactConfig {
    #[serde(default = "default_risk_weights")]
    pub risk_weights: HashMap<String, f64>,
}

fn default_risk_weights() -> HashMap<String, f64> {
    let mut weights = HashMap::new();
    weights.insert("rs".to_string(), 1.0);
    weights.insert("toml".to_string(), 0.8);
    weights.insert("json".to_string(), 0.7);
    weights.insert("yml".to_string(), 0.3);
    weights.insert("yaml".to_string(), 0.3);
    weights.insert("md".to_string(), 0.1);
    weights.insert("txt".to_string(), 0.1);
    weights.insert("codex".to_string(), 0.01);
    weights.insert("claude".to_string(), 0.01);
    weights
}

impl Default for ImpactConfig {
    fn default() -> Self {
        Self {
            risk_weights: default_risk_weights(),
        }
    }
}

impl ImpactConfig {
    pub fn get_path_weight(&self, path: &std::path::Path) -> f64 {
        if let Some(ext) = path.extension().and_then(|e| e.to_str())
            && let Some(weight) = self.risk_weights.get(ext)
        {
            return *weight;
        }

        let filename = path.file_name().and_then(|f| f.to_str()).unwrap_or("");
        if let Some(name_without_dot) = filename.strip_prefix('.')
            && let Some(weight) = self.risk_weights.get(name_without_dot)
        {
            return *weight;
        }
        for component in path.components() {
            if let Some(comp_str) = component.as_os_str().to_str() {
                let comp_clean = comp_str.trim_start_matches('.');
                if let Some(weight) = self.risk_weights.get(comp_clean) {
                    return *weight;
                }
            }
        }
        1.0
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct IntentConfig {
    #[serde(default = "default_intent_required")]
    pub required: String, // "always" | "never"
    #[serde(default = "default_tui_enabled")]
    pub tui_enabled: bool,
    #[serde(default = "default_require_signing")]
    pub require_signing: bool,
    /// Hex Ed25519 public keys (64 hex chars, normalized lowercase on load).
    /// Empty list = soft pin: crypto-valid keys report `VALID (unknown key)`.
    #[serde(default)]
    pub trusted_public_keys: Vec<String>,
    /// Reject rows with `sig_version < min_sig_version` during verify.
    /// Default 1 accepts historical v1 rows; set 2 after re-sign --all.
    #[serde(default = "default_min_sig_version")]
    pub min_sig_version: u32,
}

fn default_intent_required() -> String {
    "always".to_string()
}

fn default_tui_enabled() -> bool {
    true
}

fn default_require_signing() -> bool {
    false
}

fn default_min_sig_version() -> u32 {
    1
}

impl Default for IntentConfig {
    fn default() -> Self {
        Self {
            required: default_intent_required(),
            tui_enabled: default_tui_enabled(),
            require_signing: default_require_signing(),
            trusted_public_keys: Vec::new(),
            min_sig_version: default_min_sig_version(),
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct HotspotsConfig {
    #[serde(default = "default_hotspots_max_commits")]
    pub max_commits: usize,
    #[serde(default = "default_hotspots_limit")]
    pub limit: usize,
    /// Half-life for exponential decay (in commits). Recent commits weighted higher.
    #[serde(default = "default_decay_half_life")]
    pub decay_half_life: usize,
}

impl Default for HotspotsConfig {
    fn default() -> Self {
        Self {
            max_commits: default_hotspots_max_commits(),
            limit: default_hotspots_limit(),
            decay_half_life: default_decay_half_life(),
        }
    }
}

fn default_hotspots_max_commits() -> usize {
    500
}
fn default_hotspots_limit() -> usize {
    10
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct TemporalConfig {
    #[serde(default = "default_max_commits")]
    pub max_commits: usize,
    #[serde(default = "default_max_files_per_commit")]
    pub max_files_per_commit: usize,
    #[serde(default = "default_coupling_threshold")]
    pub coupling_threshold: f32,
    #[serde(default = "default_all_parents")]
    pub all_parents: bool,
    /// Minimum number of commits two files must share to be considered coupled
    #[serde(default = "default_min_shared_commits")]
    pub min_shared_commits: usize,
    /// Minimum number of commits a file must appear in to be eligible for coupling
    #[serde(default = "default_min_revisions")]
    pub min_revisions: usize,
    /// Half-life for exponential decay (in commits). Recent commits weighted higher.
    #[serde(default = "default_decay_half_life")]
    pub decay_half_life: usize,
}

impl Default for TemporalConfig {
    fn default() -> Self {
        Self {
            max_commits: default_max_commits(),
            max_files_per_commit: default_max_files_per_commit(),
            coupling_threshold: default_coupling_threshold(),
            all_parents: default_all_parents(),
            min_shared_commits: default_min_shared_commits(),
            min_revisions: default_min_revisions(),
            decay_half_life: default_decay_half_life(),
        }
    }
}

fn default_max_commits() -> usize {
    1000
}
fn default_max_files_per_commit() -> usize {
    50
}
fn default_coupling_threshold() -> f32 {
    0.75
}
fn default_all_parents() -> bool {
    false
}

fn default_min_shared_commits() -> usize {
    3
}
fn default_min_revisions() -> usize {
    5
}
fn default_decay_half_life() -> usize {
    100
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct CoreConfig {
    #[serde(default = "default_strict")]
    pub strict: bool,
    #[serde(default = "default_auto_fix")]
    pub auto_fix: bool,
}

impl Default for CoreConfig {
    fn default() -> Self {
        Self {
            strict: default_strict(),
            auto_fix: default_auto_fix(),
        }
    }
}

fn default_strict() -> bool {
    false
}
fn default_auto_fix() -> bool {
    false
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct WatchConfig {
    #[serde(default = "default_debounce_ms")]
    pub debounce_ms: u64,
    #[serde(default = "default_ignore_patterns")]
    pub ignore_patterns: Vec<String>,
}

impl Default for WatchConfig {
    fn default() -> Self {
        Self {
            debounce_ms: default_debounce_ms(),
            ignore_patterns: default_ignore_patterns(),
        }
    }
}

fn default_debounce_ms() -> u64 {
    1000
}
fn default_ignore_patterns() -> Vec<String> {
    vec![
        "target".to_string(),
        "target/**".to_string(),
        ".git".to_string(),
        ".git/**".to_string(),
        "node_modules".to_string(),
        "node_modules/**".to_string(),
        ".claude".to_string(),
        ".claude/**".to_string(),
        ".codex".to_string(),
        ".codex/**".to_string(),
        ".opencode".to_string(),
        ".opencode/**".to_string(),
        ".agents".to_string(),
        ".agents/**".to_string(),
        ".ledgerful".to_string(),
        ".ledgerful/**".to_string(),
    ]
}

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub struct ServiceConfig {
    #[serde(default)]
    pub definitions: Vec<ServiceDefinition>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ServiceDefinition {
    pub name: String,
    pub root: String, // Directory path
    #[serde(default)]
    pub owners: Vec<String>,
    #[serde(default)]
    pub runtime_name: Option<String>,
    #[serde(default)]
    pub queues: Vec<String>,
    #[serde(default)]
    pub topics: Vec<String>,
    #[serde(default)]
    pub rpc_endpoints: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct DocsConfig {
    #[serde(default)]
    pub include: Vec<String>,
    #[serde(default = "default_chunk_tokens")]
    pub chunk_tokens: usize,
    #[serde(default = "default_chunk_overlap")]
    pub chunk_overlap: usize,
    #[serde(default = "default_retrieval_top_k")]
    pub retrieval_top_k: usize,
}

fn default_chunk_tokens() -> usize {
    512
}
fn default_chunk_overlap() -> usize {
    64
}
fn default_retrieval_top_k() -> usize {
    5
}

impl Default for DocsConfig {
    fn default() -> Self {
        Self {
            include: Vec::new(),
            chunk_tokens: default_chunk_tokens(),
            chunk_overlap: default_chunk_overlap(),
            retrieval_top_k: default_retrieval_top_k(),
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ObservabilityConfig {
    #[serde(default)]
    pub prometheus_url: String,
    #[serde(default)]
    pub service_map: HashMap<String, String>,
    #[serde(default)]
    pub log_paths: Vec<String>,
    #[serde(default = "default_error_rate_threshold")]
    pub error_rate_threshold: f32,
    #[serde(default = "default_log_lookback_secs")]
    pub log_lookback_secs: u64,
}

fn default_error_rate_threshold() -> f32 {
    0.05
}
fn default_log_lookback_secs() -> u64 {
    3600
}

impl Default for ObservabilityConfig {
    fn default() -> Self {
        Self {
            prometheus_url: String::new(),
            service_map: HashMap::new(),
            log_paths: Vec::new(),
            error_rate_threshold: default_error_rate_threshold(),
            log_lookback_secs: default_log_lookback_secs(),
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ContractsConfig {
    #[serde(default)]
    pub spec_paths: Vec<String>,
    #[serde(default = "default_match_threshold")]
    pub match_threshold: f32,
}

fn default_match_threshold() -> f32 {
    0.5
}

impl Default for ContractsConfig {
    fn default() -> Self {
        Self {
            spec_paths: Vec::new(),
            match_threshold: default_match_threshold(),
        }
    }
}

fn default_max_reachability_depth() -> usize {
    5
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct CoverageConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_max_coupling_pairs")]
    pub max_coupling_pairs: usize,
    #[serde(default = "default_kg_timeout")]
    pub kg_timeout_secs: usize,
    #[serde(default = "default_max_reachability_depth")]
    pub max_reachability_depth: usize,
    #[serde(default)]
    pub traces: TracesConfig,
    #[serde(default)]
    pub sdk: SdkConfig,
    #[serde(default)]
    pub services: ServicesConfig,
    #[serde(default)]
    pub data_flow: DataFlowConfig,
    #[serde(default)]
    pub deploy: DeployConfig,
    #[serde(default)]
    pub ci_self_awareness: CiSelfAwarenessConfig,
    #[serde(default)]
    pub adr_staleness: AdrStalenessConfig,
}

fn default_max_coupling_pairs() -> usize {
    50
}

fn default_kg_timeout() -> usize {
    60
}

impl Default for CoverageConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            max_coupling_pairs: default_max_coupling_pairs(),
            kg_timeout_secs: default_kg_timeout(),
            max_reachability_depth: default_max_reachability_depth(),
            traces: TracesConfig::default(),
            sdk: SdkConfig::default(),
            services: ServicesConfig::default(),
            data_flow: DataFlowConfig::default(),
            deploy: DeployConfig::default(),
            ci_self_awareness: CiSelfAwarenessConfig::default(),
            adr_staleness: AdrStalenessConfig::default(),
        }
    }
}

/// Why service inference will or won't run, derived from config. This is the
/// single source of truth both the indexer and `services diff` consult, so
/// user-facing messaging can never drift from the actual gating policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceInferenceState {
    Enabled,
    DisabledGlobally,
    DisabledForServices,
}

impl CoverageConfig {
    pub fn service_inference_state(&self) -> ServiceInferenceState {
        if !self.enabled {
            ServiceInferenceState::DisabledGlobally
        } else if !self.services.enabled {
            ServiceInferenceState::DisabledForServices
        } else {
            ServiceInferenceState::Enabled
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct TracesConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_trace_config_patterns")]
    pub config_patterns: Vec<String>,
    #[serde(default = "default_trace_env_var_patterns")]
    pub env_var_patterns: Vec<String>,
    #[serde(default = "default_exclude_env_patterns")]
    pub exclude_env_patterns: Vec<String>,
    #[serde(default = "default_trace_risk_weight_per_config")]
    pub risk_weight_per_config_file: u32,
    #[serde(default = "default_trace_risk_weight_per_env")]
    pub risk_weight_per_env_var: u32,
    #[serde(default = "default_trace_risk_cap")]
    pub risk_cap: u32,
}

fn default_trace_risk_weight_per_config() -> u32 {
    3
}
fn default_trace_risk_weight_per_env() -> u32 {
    2
}
fn default_trace_risk_cap() -> u32 {
    10
}

fn default_trace_config_patterns() -> Vec<String> {
    vec![
        "**/otel*.yaml".to_string(),
        "**/jaeger*.yaml".to_string(),
        "**/datadog*.yaml".to_string(),
    ]
}

fn default_trace_env_var_patterns() -> Vec<String> {
    vec![
        "OTEL_*".to_string(),
        "JAEGER_*".to_string(),
        "DD_*".to_string(),
        "OTLP_*".to_string(),
    ]
}

fn default_exclude_env_patterns() -> Vec<String> {
    vec!["OTEL_SDK_DISABLED".to_string()]
}

impl Default for TracesConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            config_patterns: default_trace_config_patterns(),
            env_var_patterns: default_trace_env_var_patterns(),
            exclude_env_patterns: default_exclude_env_patterns(),
            risk_weight_per_config_file: default_trace_risk_weight_per_config(),
            risk_weight_per_env_var: default_trace_risk_weight_per_env(),
            risk_cap: default_trace_risk_cap(),
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SdkConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_sdk_patterns")]
    pub patterns: Vec<String>,
    #[serde(default = "default_sdk_risk_weight_new")]
    pub risk_weight_new: u32,
    #[serde(default = "default_sdk_risk_weight_modified")]
    pub risk_weight_modified: u32,
    #[serde(default = "default_sdk_risk_cap")]
    pub risk_cap: u32,
}

fn default_sdk_risk_cap() -> u32 {
    10
}

fn default_sdk_patterns() -> Vec<String> {
    vec![
        "stripe".to_string(),
        "auth0".to_string(),
        "twilio".to_string(),
        "sendgrid".to_string(),
        "openai".to_string(),
        "anthropic".to_string(),
    ]
}

fn default_sdk_risk_weight_new() -> u32 {
    5
}

fn default_sdk_risk_weight_modified() -> u32 {
    2
}

impl Default for SdkConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            patterns: default_sdk_patterns(),
            risk_weight_new: default_sdk_risk_weight_new(),
            risk_weight_modified: default_sdk_risk_weight_modified(),
            risk_cap: default_sdk_risk_cap(),
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ServicesConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_cross_service_elevation_threshold")]
    pub cross_service_elevation_threshold: u32,
    #[serde(default = "default_svc_risk_5plus")]
    pub risk_weight_5plus: u32,
    #[serde(default = "default_svc_risk_3to4")]
    pub risk_weight_3to4: u32,
    #[serde(default = "default_svc_risk_2svcs")]
    pub risk_weight_2svcs: u32,
}

fn default_svc_risk_5plus() -> u32 {
    15
}
fn default_svc_risk_3to4() -> u32 {
    8
}
fn default_svc_risk_2svcs() -> u32 {
    3
}

fn default_cross_service_elevation_threshold() -> u32 {
    2
}

impl Default for ServicesConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            cross_service_elevation_threshold: default_cross_service_elevation_threshold(),
            risk_weight_5plus: default_svc_risk_5plus(),
            risk_weight_3to4: default_svc_risk_3to4(),
            risk_weight_2svcs: default_svc_risk_2svcs(),
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct DataFlowConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_chain_depth_max")]
    pub chain_depth_max: u32,
    #[serde(default = "default_dataflow_risk_per_match")]
    pub risk_weight_per_match: u32,
    #[serde(default = "default_dataflow_risk_cap")]
    pub risk_cap: u32,
}

fn default_dataflow_risk_per_match() -> u32 {
    4
}
fn default_dataflow_risk_cap() -> u32 {
    12
}

fn default_chain_depth_max() -> u32 {
    5
}

impl Default for DataFlowConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            chain_depth_max: default_chain_depth_max(),
            risk_weight_per_match: default_dataflow_risk_per_match(),
            risk_cap: default_dataflow_risk_cap(),
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct DeployConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_deploy_patterns")]
    pub patterns: Vec<String>,
    #[serde(default = "default_deploy_risk_weight_per_manifest")]
    pub risk_weight_per_manifest: u32,
    #[serde(default = "default_deploy_risk_cap")]
    pub risk_cap: u32,
}

fn default_deploy_patterns() -> Vec<String> {
    vec![
        "**/Dockerfile*".to_string(),
        "**/docker-compose*.yml".to_string(),
        "**/*.tf".to_string(),
        "**/k8s/**/*.yaml".to_string(),
    ]
}

fn default_deploy_risk_weight_per_manifest() -> u32 {
    3
}

fn default_deploy_risk_cap() -> u32 {
    15
}

impl Default for DeployConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            patterns: default_deploy_patterns(),
            risk_weight_per_manifest: default_deploy_risk_weight_per_manifest(),
            risk_cap: default_deploy_risk_cap(),
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct CiSelfAwarenessConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_ci_changed_weight")]
    pub ci_changed_weight: u32,
    #[serde(default = "default_ci_plus_source_weight")]
    pub ci_plus_source_weight: u32,
}

fn default_ci_changed_weight() -> u32 {
    3
}

fn default_ci_plus_source_weight() -> u32 {
    5
}

impl Default for CiSelfAwarenessConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            ci_changed_weight: default_ci_changed_weight(),
            ci_plus_source_weight: default_ci_plus_source_weight(),
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct AdrStalenessConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_staleness_threshold_days")]
    pub threshold_days: u32,
}

fn default_staleness_threshold_days() -> u32 {
    365
}

impl Default for AdrStalenessConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            threshold_days: default_staleness_threshold_days(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_ignore_patterns_include_agent_dotfiles() {
        let config = WatchConfig::default();
        assert!(config.ignore_patterns.iter().any(|p| p == ".claude/**"));
        assert!(config.ignore_patterns.iter().any(|p| p == ".agents/**"));
        assert!(config.ignore_patterns.iter().any(|p| p == ".codex/**"));
        assert!(config.ignore_patterns.iter().any(|p| p == ".opencode/**"));
        // Regression: existing patterns still present
        assert!(config.ignore_patterns.iter().any(|p| p == "target/**"));
        assert!(config.ignore_patterns.iter().any(|p| p == ".git/**"));
    }

    #[test]
    fn test_docs_config_defaults() {
        let config = DocsConfig::default();
        assert!(config.include.is_empty());
        assert_eq!(config.chunk_tokens, 512);
        assert_eq!(config.chunk_overlap, 64);
        assert_eq!(config.retrieval_top_k, 5);
    }

    #[test]
    fn test_observability_config_defaults() {
        let config = ObservabilityConfig::default();
        assert_eq!(config.prometheus_url, "");
        assert!(config.service_map.is_empty());
        assert!(config.log_paths.is_empty());
        assert!((config.error_rate_threshold - 0.05).abs() < f32::EPSILON);
        assert_eq!(config.log_lookback_secs, 3600);
    }

    #[test]
    fn test_contracts_config_defaults() {
        let config = ContractsConfig::default();
        assert!(config.spec_paths.is_empty());
        assert!((config.match_threshold - 0.5).abs() < f32::EPSILON);
    }

    #[test]
    fn test_dead_code_config_defaults() {
        let config = DeadCodeConfig::default();
        assert!(!config.enabled);
        assert!((config.confidence_threshold - 0.75).abs() < f64::EPSILON);
        assert_eq!(config.git_inactivity_days, 90);
        assert!((config.reachability_weight - 1.0).abs() < f64::EPSILON);
        assert!((config.git_activity_weight - 1.0).abs() < f64::EPSILON);
        assert!((config.test_coverage_weight - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_coverage_config_defaults() {
        let config = CoverageConfig::default();
        assert!(!config.enabled);
        assert_eq!(config.max_coupling_pairs, 50);
    }

    #[test]
    fn test_temporal_config_defaults() {
        let config = TemporalConfig::default();
        assert_eq!(config.min_shared_commits, 3);
        assert_eq!(config.min_revisions, 5);
        assert_eq!(config.decay_half_life, 100);
    }
}
