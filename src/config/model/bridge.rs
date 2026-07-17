use serde::{Deserialize, Serialize};

/// `[bridge]` configuration — controls the Ledgerful data-interchange bridge.
///
/// The bridge is a versioned local NDJSON export/import/query interchange.
/// It is local-only and network-free: it uses stdout/files, the local SQLite
/// ledger, and a local named-pipe/Unix-socket IPC path.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct BridgeConfig {
    /// Master switch for the Ledgerful data-interchange bridge.
    ///
    /// When `false` (the default), Ledgerful performs zero implicit bridge
    /// activity: no IPC connects, no provider spawns, no automatic verify/watch
    /// pushes, and no CozoDB `Turn`/`Session`/`Memory`/`Decision` lifecycle.
    /// Explicit `bridge export`/`bridge import` remain usable because they are
    /// pure-local I/O.
    #[serde(default)]
    pub enabled: bool,

    /// External context-provider binary invoked by `bridge query` when the
    /// IPC path is unavailable.
    ///
    /// Example: `provider_command = "ai-brains"` (the documented example
    /// provider). Must accept `sync query <query> --format ndjson` on stdin
    /// and emit `BridgeRecord` NDJSON lines on stdout.
    #[serde(default = "default_provider_command")]
    pub provider_command: String,
}

const fn default_enabled() -> bool {
    false
}

fn default_provider_command() -> String {
    "ai-brains".to_string()
}

impl Default for BridgeConfig {
    fn default() -> Self {
        Self {
            enabled: default_enabled(),
            provider_command: default_provider_command(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::model::Config;

    #[test]
    fn bridge_enabled_defaults_to_false() {
        let config = BridgeConfig::default();
        assert!(!config.enabled);
    }

    #[test]
    fn bridge_enabled_deserializes_true() {
        let toml_str = r#"
            [bridge]
            enabled = true
        "#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert!(config.bridge.enabled);
    }

    #[test]
    fn omitted_bridge_enabled_defaults_to_false() {
        let config: Config = toml::from_str("").unwrap();
        assert!(!config.bridge.enabled);
    }

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
