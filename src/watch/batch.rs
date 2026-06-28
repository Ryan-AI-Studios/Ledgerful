use crate::watch::WatchError;
use camino::Utf8PathBuf;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum WatchEventKind {
    Create,
    Modify,
    Delete,
    Rename,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WatchEvent {
    pub path: Utf8PathBuf,
    pub kind: WatchEventKind,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WatchBatch {
    pub timestamp: DateTime<Utc>,
    pub events: Vec<WatchEvent>,
}

impl WatchBatch {
    pub fn new(events: Vec<WatchEvent>) -> Self {
        Self {
            timestamp: Utc::now(),
            events,
        }
    }

    pub fn save(&self, path: &camino::Utf8Path) -> Result<(), WatchError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let content = serde_json::to_string_pretty(self)
            .map_err(|e| WatchError::NotifyError(format!("Failed to serialize batch: {}", e)))?;

        // Use a temporary file for atomic write if possible
        let tmp_path = path.with_extension("tmp");
        fs::write(&tmp_path, content)?;
        fs::rename(&tmp_path, path)?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use camino::Utf8Path;
    use tempfile::tempdir;

    #[test]
    fn test_batch_serialization() {
        let events = vec![
            WatchEvent {
                path: Utf8PathBuf::from("src/main.rs"),
                kind: WatchEventKind::Modify,
            },
            WatchEvent {
                path: Utf8PathBuf::from("src/watch/mod.rs"),
                kind: WatchEventKind::Create,
            },
        ];
        let batch = WatchBatch::new(events);

        let tmp = tempdir().unwrap();
        let root = Utf8Path::from_path(tmp.path()).unwrap();
        let batch_path = root.join("current-batch.json");

        batch.save(&batch_path).unwrap();

        assert!(batch_path.exists());

        let content = fs::read_to_string(&batch_path).unwrap();
        let deserialized: WatchBatch = serde_json::from_str(&content).unwrap();

        assert_eq!(batch.events, deserialized.events);
    }
}
