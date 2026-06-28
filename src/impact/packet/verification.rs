use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "camelCase")]
pub struct VerificationResult {
    pub name: String,
    pub command: String,
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    pub duration_ms: u64,
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CIGate {
    pub platform: String,
    pub job_name: String,
    pub trigger: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub release_gates: Vec<String>,
}

impl Eq for CIGate {}

impl PartialOrd for CIGate {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for CIGate {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.platform
            .cmp(&other.platform)
            .then_with(|| self.job_name.cmp(&other.job_name))
            .then_with(|| self.workflow_name.cmp(&other.workflow_name))
            .then_with(|| self.trigger.cmp(&other.trigger))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CIPrediction {
    pub job_name: String,
    pub platform: String,
    pub failure_probability: f32,
    pub explanation: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "camelCase")]
pub struct CiConfigChange {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub known_ci_files: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unknown_ci_files: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pre_commit_files: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub generated_ci_files: Vec<String>,
    #[serde(default)]
    pub source_changed: bool,
    #[serde(default)]
    pub deploy_changed: bool,
}
