use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DataModel {
    pub model_name: String,
    pub model_kind: String,
    pub confidence: f64,
    pub evidence: Option<String>,
}

impl Eq for DataModel {}

impl PartialOrd for DataModel {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for DataModel {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.model_name
            .cmp(&other.model_name)
            .then_with(|| self.model_kind.cmp(&other.model_kind))
            .then_with(|| {
                self.confidence
                    .partial_cmp(&other.confidence)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ApiRoute {
    pub method: String,
    pub path_pattern: String,
    pub handler_symbol_name: Option<String>,
    pub framework: String,
    pub route_source: String,
    pub mount_prefix: Option<String>,
    pub is_dynamic: bool,
    pub route_confidence: f64,
    pub evidence: String,
    #[serde(default)]
    pub auth_requirements: Option<Vec<String>>,
    #[serde(default)]
    pub schema_refs: Option<Vec<String>>,
    #[serde(default)]
    pub owning_service: Option<String>,
    #[serde(default)]
    pub consumers: Option<Vec<String>>,
}

impl Eq for ApiRoute {}

impl PartialOrd for ApiRoute {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for ApiRoute {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.method
            .cmp(&other.method)
            .then_with(|| self.path_pattern.cmp(&other.path_pattern))
            .then_with(|| self.framework.cmp(&other.framework))
            .then_with(|| {
                self.route_confidence
                    .partial_cmp(&other.route_confidence)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "camelCase")]
pub struct ServiceImpact {
    pub service_name: String,
    pub impact_kind: String, // "Downstream Breakage", "Public Contract Change"
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct ServiceMapDelta {
    pub services: Vec<Service>,
    pub affected_services: Vec<String>,
    pub cross_service_edges: Vec<(String, String, usize)>, // (caller_service, callee_service, count)
    pub total_services: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct Service {
    pub name: String,
    pub directory: PathBuf,
    pub routes: Vec<String>,      // paths
    pub data_models: Vec<String>, // names
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
