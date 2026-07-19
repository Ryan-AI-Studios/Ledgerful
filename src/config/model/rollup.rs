use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Per-user configuration for the cross-repo ledger/posture rollup.
///
/// All fields are optional in `~/.ledgerful/config.toml` under the
/// `[global_rollup]` table. Defaults favor a safe, bounded home-directory walk
/// with a regenerable local cache.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct GlobalRollupConfig {
    /// Root directories to walk looking for `.ledgerful/state/ledger.db`.
    #[serde(default = "default_roots")]
    pub roots: Vec<PathBuf>,
    /// Hard backstop deadline for the discovery walk, in seconds.
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u64,
    /// Cache entries older than this are considered stale and trigger a re-walk.
    #[serde(default = "default_staleness_secs")]
    pub staleness_secs: u64,
    /// Optional maximum directory depth for discovery (None = unlimited).
    /// The walk timeout remains the backstop regardless of this value.
    #[serde(default)]
    pub max_depth: Option<usize>,
    /// Master switch. `--opt-out` sets false; `--opt-in` restores true.
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

impl Default for GlobalRollupConfig {
    fn default() -> Self {
        Self {
            roots: default_roots(),
            timeout_secs: default_timeout_secs(),
            staleness_secs: default_staleness_secs(),
            max_depth: None,
            enabled: default_enabled(),
        }
    }
}

fn default_roots() -> Vec<PathBuf> {
    vec![PathBuf::from("~")]
}

fn default_timeout_secs() -> u64 {
    30
}

fn default_staleness_secs() -> u64 {
    3600
}

fn default_enabled() -> bool {
    true
}
