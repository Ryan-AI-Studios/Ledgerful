use serde::{Deserialize, Serialize};
use std::time::Duration;

/// TA31 R2: opt-in auto-sync of stale/missing sibling `schema.json` files
/// discovered by `FederatedScanner::scan_siblings()`. Defaults to `false` —
/// auto-sync spawns a blocking `ledgerful federate export` subprocess per
/// sibling, which is too expensive to run unconditionally from every call
/// site (notably the `GET /api/projects` HTTP handler). Only
/// `execute_federate_scan` (the `ledgerful federate scan` CLI command) reads
/// this flag and opts the scanner in via `FederatedScanner::with_auto_sync`.
///
/// 0034 adds scan reliability controls:
/// - `scan_exclusions`: directory names skipped during federated dependency
///   scanning (default = the original hard-coded tooling-cache list).
/// - `sync_timeout_secs`: per-sibling timeout for the auto-sync export
///   subprocess.
/// - `scan_file_budget`: maximum files walked in a single federated dependency
///   scan; the scan returns partial results when the budget is exceeded.
/// - `scan_timeout_secs`: overall backstop deadline for impact enrichment so
///   a hung provider cannot stall the whole command.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct FederationConfig {
    #[serde(default)]
    pub auto_sync_siblings: bool,
    #[serde(default = "default_scan_exclusions")]
    pub scan_exclusions: Vec<String>,
    #[serde(default = "default_sync_timeout_secs")]
    pub sync_timeout_secs: u64,
    #[serde(default = "default_scan_file_budget")]
    pub scan_file_budget: usize,
    #[serde(default = "default_scan_timeout_secs")]
    pub scan_timeout_secs: u64,
}

pub fn default_scan_exclusions() -> Vec<String> {
    vec![
        ".git".to_string(),
        ".ledgerful".to_string(),
        "target".to_string(),
        "node_modules".to_string(),
        ".opencode".to_string(),
        ".cargo".to_string(),
        ".claude".to_string(),
        ".config".to_string(),
        ".agents".to_string(),
        "vendor".to_string(),
    ]
}

pub fn default_sync_timeout_secs() -> u64 {
    30
}

pub fn default_scan_file_budget() -> usize {
    5000
}

pub fn default_scan_timeout_secs() -> u64 {
    120
}

impl FederationConfig {
    pub fn sync_timeout(&self) -> Duration {
        Duration::from_secs(self.sync_timeout_secs)
    }

    pub fn scan_timeout(&self) -> Duration {
        Duration::from_secs(self.scan_timeout_secs)
    }
}

impl Default for FederationConfig {
    fn default() -> Self {
        Self {
            auto_sync_siblings: false,
            scan_exclusions: default_scan_exclusions(),
            sync_timeout_secs: default_sync_timeout_secs(),
            scan_file_budget: default_scan_file_budget(),
            scan_timeout_secs: default_scan_timeout_secs(),
        }
    }
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
    fn default_scan_exclusions_matches_hard_coded_list() {
        let config = FederationConfig::default();
        assert_eq!(
            config.scan_exclusions,
            vec![
                ".git",
                ".ledgerful",
                "target",
                "node_modules",
                ".opencode",
                ".cargo",
                ".claude",
                ".config",
                ".agents",
                "vendor",
            ]
        );
        assert_eq!(config.sync_timeout_secs, 30);
        assert_eq!(config.scan_file_budget, 5000);
        assert_eq!(config.scan_timeout_secs, 120);
    }

    #[test]
    fn deserializes_from_toml() {
        let toml_str = r#"
            [federation]
            auto_sync_siblings = true
        "#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert!(config.federation.auto_sync_siblings);
        assert_eq!(config.federation.scan_exclusions, default_scan_exclusions());
    }

    #[test]
    fn omitted_section_defaults_to_false() {
        let config: Config = toml::from_str("").unwrap();
        assert!(!config.federation.auto_sync_siblings);
        assert_eq!(config.federation.scan_exclusions, default_scan_exclusions());
    }

    #[test]
    fn toml_overrides_scan_exclusions() {
        let toml_str = r#"
            [federation]
            scan_exclusions = ["node_modules", "fixtures"]
        "#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(
            config.federation.scan_exclusions,
            vec!["node_modules".to_string(), "fixtures".to_string()]
        );
    }

    #[test]
    fn toml_overrides_scan_timeouts_and_budget() {
        let toml_str = r#"
            [federation]
            sync_timeout_secs = 15
            scan_file_budget = 2500
            scan_timeout_secs = 90
        "#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.federation.sync_timeout_secs, 15);
        assert_eq!(config.federation.scan_file_budget, 2500);
        assert_eq!(config.federation.scan_timeout_secs, 90);
    }
}
