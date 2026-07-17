use serde::{Deserialize, Serialize};

/// `[bridge]` configuration — controls the Ledgerful data-interchange bridge.
///
/// The bridge is a versioned local NDJSON export/import/query interchange.
/// It is local-only and network-free: it uses stdout/files, the local SQLite
/// ledger, and a local named-pipe/Unix-socket IPC path.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct BridgeConfig {
    /// External context-provider binary invoked by `bridge query` when the
    /// IPC path is unavailable.
    ///
    /// Example: `provider_command = "ai-brains"` (the documented example
    /// provider). Must accept `sync query <query> --format ndjson` on stdin
    /// and emit `BridgeRecord` NDJSON lines on stdout.
    #[serde(default = "default_provider_command")]
    pub provider_command: String,
}

fn default_provider_command() -> String {
    "ai-brains".to_string()
}

impl Default for BridgeConfig {
    fn default() -> Self {
        Self {
            provider_command: default_provider_command(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::model::Config;

    #[test]
    fn default_provider_command_is_example_provider() {
        let config = BridgeConfig::default();
        assert_eq!(config.provider_command, "ai-brains");
    }

    #[test]
    fn bridge_provider_command_deserializes() {
        let toml_str = r#"
            [bridge]
            provider_command = "example-provider"
        "#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.bridge.provider_command, "example-provider");
    }

    #[test]
    fn omitted_bridge_section_defaults() {
        let config: Config = toml::from_str("").unwrap();
        assert_eq!(config.bridge.provider_command, "ai-brains");
    }
}
