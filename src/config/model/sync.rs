use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SyncConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_target")]
    pub target: String,
    #[serde(default = "default_schedule")]
    pub schedule: String,
    #[serde(default = "default_batch_size")]
    pub batch_size: usize,
    #[serde(default = "default_archive_retention_days")]
    pub archive_retention_days: u64,
    #[serde(default = "default_max_clock_drift_seconds")]
    pub max_clock_drift_seconds: u64,
    #[serde(default)]
    pub device_id: Option<String>,
}

impl Default for SyncConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            target: default_target(),
            schedule: default_schedule(),
            batch_size: default_batch_size(),
            archive_retention_days: default_archive_retention_days(),
            max_clock_drift_seconds: default_max_clock_drift_seconds(),
            device_id: None,
        }
    }
}

fn default_target() -> String {
    "".to_string()
}

fn default_schedule() -> String {
    "0 3 * * *".to_string()
}

fn default_batch_size() -> usize {
    500
}

fn default_archive_retention_days() -> u64 {
    90
}

fn default_max_clock_drift_seconds() -> u64 {
    300
}
