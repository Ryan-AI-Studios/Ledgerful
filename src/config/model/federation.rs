use serde::{Deserialize, Serialize};

/// TA31 R2: opt-in auto-sync of stale/missing sibling `schema.json` files
/// discovered by `FederatedScanner::scan_siblings()`. Defaults to `false` —
/// auto-sync spawns a blocking `ledgerful federate export` subprocess per
/// sibling, which is too expensive to run unconditionally from every call
/// site (notably the `GET /api/projects` HTTP handler). Only
/// `execute_federate_scan` (the `ledgerful federate scan` CLI command) reads
/// this flag and opts the scanner in via `FederatedScanner::with_auto_sync`.
#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub struct FederationConfig {
    #[serde(default)]
    pub auto_sync_siblings: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::model::Config;

    #[test]
    fn default_is_off() {
        let config = FederationConfig::default();
        assert!(!config.auto_sync_siblings);
    }

    #[test]
    fn deserializes_from_toml() {
        let toml_str = r#"
            [federation]
            auto_sync_siblings = true
        "#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert!(config.federation.auto_sync_siblings);
    }

    #[test]
    fn omitted_section_defaults_to_false() {
        let config: Config = toml::from_str("").unwrap();
        assert!(!config.federation.auto_sync_siblings);
    }
}
