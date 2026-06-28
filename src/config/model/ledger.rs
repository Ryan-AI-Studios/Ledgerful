use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct LedgerConfig {
    /// Enable tech stack enforcement at transaction start
    #[serde(default)]
    pub enforcement_enabled: bool,

    /// Require verification pass before commit for high-risk categories
    #[serde(default)]
    pub verify_to_commit: bool,

    /// Auto-reconcile watcher drift for the same entity at commit time
    #[serde(default = "default_auto_reconcile")]
    pub auto_reconcile: bool,

    /// Roll back PENDING transactions older than this many hours
    #[serde(default = "default_stale_threshold_hours")]
    pub stale_threshold_hours: u64,

    /// Category-to-stack mappings (defined in config, not just DB)
    #[serde(default)]
    pub category_mappings: Vec<CategoryMapping>,

    /// Watcher patterns for drift detection (supplements hardcoded list)
    #[serde(default)]
    pub watcher_patterns: Vec<WatcherPattern>,

    /// Template for git commit messages when using --with-git.
    /// Supports placeholders: {category}, {summary}, {tx_id}.
    /// Default: "[{category}] {summary}\n\nLedger: {tx_id}".
    #[serde(default)]
    pub git_commit_template: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct CategoryMapping {
    pub ledger_category: String,
    pub stack_category: String,
    pub glob: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct WatcherPattern {
    pub glob: String,
    pub category: String,
}

impl Default for LedgerConfig {
    fn default() -> Self {
        Self {
            enforcement_enabled: false,
            verify_to_commit: false,
            auto_reconcile: default_auto_reconcile(),
            stale_threshold_hours: default_stale_threshold_hours(),
            category_mappings: Vec::new(),
            watcher_patterns: Vec::new(),
            git_commit_template: None,
        }
    }
}

fn default_auto_reconcile() -> bool {
    true
}

fn default_stale_threshold_hours() -> u64 {
    24
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ledger_config_defaults() {
        let config = LedgerConfig::default();
        assert!(!config.enforcement_enabled);
        assert!(!config.verify_to_commit);
        assert!(config.auto_reconcile);
        assert_eq!(config.stale_threshold_hours, 24);
    }
}
