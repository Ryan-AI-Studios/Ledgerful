use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CoverageDelta {
    pub file_path: String,
    pub pattern_kind: String,
    pub previous_count: usize,
    pub current_count: usize,
    pub message: String,
}

impl Eq for CoverageDelta {}

impl PartialOrd for CoverageDelta {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for CoverageDelta {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.file_path
            .cmp(&other.file_path)
            .then_with(|| self.pattern_kind.cmp(&other.pattern_kind))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeUsageDelta {
    pub file_path: String,
    pub env_vars_previous_count: usize,
    pub env_vars_current_count: usize,
    pub config_keys_previous_count: usize,
    pub config_keys_current_count: usize,
    /// The actual env var names from the previous version (for identity-aware comparison).
    #[serde(default)]
    pub env_vars_previous: Vec<String>,
    /// The actual env var names from the current version (for identity-aware comparison).
    #[serde(default)]
    pub env_vars_current: Vec<String>,
}

impl Eq for RuntimeUsageDelta {}

impl PartialOrd for RuntimeUsageDelta {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for RuntimeUsageDelta {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.file_path.cmp(&other.file_path)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CoveringTest {
    pub test_file: String,
    pub test_symbol: String,
    pub confidence: f64,
    pub mapping_kind: String,
}

impl Eq for CoveringTest {}

impl PartialOrd for CoveringTest {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for CoveringTest {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.test_file
            .cmp(&other.test_file)
            .then_with(|| self.test_symbol.cmp(&other.test_symbol))
            .then_with(|| {
                self.confidence
                    .partial_cmp(&other.confidence)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| self.mapping_kind.cmp(&other.mapping_kind))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TestCoverage {
    pub changed_symbol: String,
    pub changed_file: String,
    pub covering_tests: Vec<CoveringTest>,
}

impl Eq for TestCoverage {}

impl PartialOrd for TestCoverage {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for TestCoverage {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.changed_symbol
            .cmp(&other.changed_symbol)
            .then_with(|| self.changed_file.cmp(&other.changed_file))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "camelCase")]
pub struct CallChainNode {
    pub symbol: String,
    pub file_path: PathBuf,
    pub is_data_model: bool,
    pub is_external: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "camelCase")]
pub struct CallChain {
    pub nodes: Vec<CallChainNode>,
    pub has_cycle: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub enum TraceConfigType {
    OpenTelemetryCollector,
    JaegerAgent,
    DataDogAgent,
    GrafanaAgent,
    GrafanaTempo,
    Unknown,
}

impl TraceConfigType {
    pub fn from_path(path: &std::path::Path) -> Self {
        let path_str = path.to_string_lossy().to_lowercase();
        if path_str.contains("otel") {
            Self::OpenTelemetryCollector
        } else if path_str.contains("jaeger") {
            Self::JaegerAgent
        } else if path_str.contains("datadog") {
            Self::DataDogAgent
        } else if path_str.contains("grafana-agent") {
            Self::GrafanaAgent
        } else if path_str.contains("tempo") {
            Self::GrafanaTempo
        } else {
            Self::Unknown
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "camelCase")]
pub struct TraceConfigChange {
    pub file: PathBuf,
    pub config_type: TraceConfigType,
    pub risk_weight: u8,
    pub is_deleted: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "camelCase")]
pub struct TraceEnvVarChange {
    pub var_name: String,
    pub pattern: String,
    pub risk_weight: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "camelCase")]
pub struct SdkDependencyDelta {
    pub added: Vec<SdkDependency>,
    pub removed: Vec<SdkDependency>,
    pub modified: Vec<SdkDependency>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "camelCase")]
pub struct SdkDependency {
    pub sdk_name: String,
    pub file_path: PathBuf,
    pub import_statement: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ManifestType {
    Dockerfile,
    DockerCompose,
    Kubernetes,
    Terraform,
    Helm,
    CiWorkflow,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeployManifestChange {
    pub file: PathBuf,
    pub manifest_type: ManifestType,
    pub risk_tier: u8,
    pub coupled_files: Vec<String>,
    pub high_blast_resources: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
}

impl PartialEq for DeployManifestChange {
    fn eq(&self, other: &Self) -> bool {
        self.file == other.file
            && self.manifest_type == other.manifest_type
            && self.risk_tier == other.risk_tier
            && self.coupled_files == other.coupled_files
            && self.high_blast_resources == other.high_blast_resources
            && self.service_name == other.service_name
            && self.owner == other.owner
    }
}

impl Eq for DeployManifestChange {}

impl PartialOrd for DeployManifestChange {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for DeployManifestChange {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        other
            .risk_tier
            .cmp(&self.risk_tier)
            .then_with(|| self.file.cmp(&other.file))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DataFlowMatch {
    pub chain_label: String,
    pub changed_nodes: Vec<String>,
    pub total_nodes: usize,
    pub change_pct: f64,
    pub risk: super::RiskLevel,
}

impl Eq for DataFlowMatch {}

impl PartialOrd for DataFlowMatch {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for DataFlowMatch {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        other
            .change_pct
            .partial_cmp(&self.change_pct)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| self.chain_label.cmp(&other.chain_label))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::impact::packet::RiskLevel;
    use rstest::rstest;
    use serde::Serialize;
    use serde::de::DeserializeOwned;

    fn roundtrip_json<T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug>(
        value: T,
    ) -> T {
        let json = serde_json::to_string(&value).unwrap();
        serde_json::from_str(&json).unwrap()
    }

    #[rstest]
    #[case::deploy_manifest_change(DeployManifestChange {
        file: PathBuf::from("Dockerfile"),
        manifest_type: ManifestType::Dockerfile,
        risk_tier: 2,
        coupled_files: vec!["src/".to_string()],
        high_blast_resources: vec![],
        service_name: None,
        owner: None,
    })]
    #[case::data_flow_match(DataFlowMatch {
        chain_label: "A -> B -> C".to_string(),
        changed_nodes: vec!["A".to_string(), "C".to_string()],
        total_nodes: 3,
        change_pct: 0.67,
        risk: RiskLevel::High,
    })]
    fn packet_coverage_roundtrips<
        T: Serialize + DeserializeOwned + Clone + PartialEq + std::fmt::Debug,
    >(
        #[case] value: T,
    ) {
        let expected = value.clone();
        let back = roundtrip_json(value);
        assert_eq!(back, expected, "roundtrip should preserve value");
    }
}
