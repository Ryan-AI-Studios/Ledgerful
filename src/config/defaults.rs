use crate::config::ConfigError;
use camino::Utf8PathBuf;
use std::fs;

pub const DEFAULT_CONFIG: &str = r#"[core]
strict = false
auto_fix = false

[watch]
debounce_ms = 1000
ignore_patterns = [
    "target", "target/**", ".git", ".git/**", "node_modules", "node_modules/**",
    ".claude", ".claude/**", ".codex", ".codex/**", ".opencode", ".opencode/**",
    ".agents", ".agents/**", ".ledgerful", ".ledgerful/**"
]

[temporal]
max_commits = 1000
max_files_per_commit = 50
coupling_threshold = 0.75
min_shared_commits = 3
min_revisions = 5
decay_half_life = 100

[hotspots]
max_commits = 500
limit = 10

[verify]
mode = "auto"
# default_timeout_secs = 300
# Steps to run when `ledgerful verify` is invoked without -c.
# Each step has a description, command, and optional timeout_secs (defaults to 300).
# [[verify.steps]]
# description = "Run project tests"
# command = "cargo nextest run --workspace --all-features --profile ci"
# timeout_secs = 300
# [[verify.steps]]
# description = "Check formatting"
# command = "cargo fmt --check"

[gemini]
# Prefer GEMINI_API_KEY in the environment or local .env.
# api_key = "..."
# Optional override for every ask mode:
# model = "gemini-3.1-pro"
fast_model = "gemini-3.1-flash-lite"
deep_model = "gemini-3.1-pro"
timeout_secs = 120
context_window = 128000

[index]
stale_threshold_days = 3

[local_model]
# Use 127.0.0.1 — 'localhost' resolves to ::1 (IPv6) on Windows, which breaks IPv4-only servers
base_url = "http://127.0.0.1:8081"
embedding_url = "http://127.0.0.1:8083"
# Optional fallback for Ollama Cloud completions.
# Prefer OLLAMA_CLOUD_* environment variables or local .env for secrets.
# Native:   ollama_cloud_url = "https://ollama.com/api"  (POST /api/chat)
# OpenAI:   ollama_cloud_url = "https://ollama.com"      (POST /v1/chat/completions)
# NOTE: https://api.ollama.com does NOT support /v1/chat/completions
# ollama_cloud_model = "minimax-m3:cloud"
# Backward-compatible alias: ollama_key = "..."  (same as ollama_cloud_api_key)

[semantic]
hnsw_rebuild_threshold = 500
# parse_concurrency = 8
# embed_concurrency = 4
# embed_concurrency_cap = 4
# Legacy combined field:
# concurrency = 8

[intent]
required = "always"
tui_enabled = true
require_signing = false

[impact.risk_weights]
rs = 1.0
toml = 0.8
json = 0.7
yml = 0.3
yaml = 0.3
md = 0.1
txt = 0.1
codex = 0.01
claude = 0.01
"#;

pub const DEFAULT_CONFIG_TEMPLATE_ENV: &str = "LEDGERFUL_DEFAULT_CONFIG";
pub const USER_DEFAULT_CONFIG_FILE: &str = "default-config.toml";

pub fn default_config_contents() -> Result<String, ConfigError> {
    if let Some(path) = default_config_template_path()
        && path.exists()
    {
        return fs::read_to_string(path.as_std_path()).map_err(|source| ConfigError::ReadFailed {
            path: path.to_string(),
            source,
        });
    }

    Ok(DEFAULT_CONFIG.to_string())
}

pub(crate) fn default_config_template_path() -> Option<Utf8PathBuf> {
    if let Some(path) = std::env::var_os(DEFAULT_CONFIG_TEMPLATE_ENV) {
        return Utf8PathBuf::from_path_buf(path.into()).ok();
    }

    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"))?;
    Utf8PathBuf::from_path_buf(home.into())
        .ok()
        .map(|home| home.join(".ledgerful").join(USER_DEFAULT_CONFIG_FILE))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_template_uses_127() {
        let config: crate::config::model::Config = toml::from_str(DEFAULT_CONFIG).unwrap();
        assert_eq!(config.local_model.base_url, "http://127.0.0.1:8081");
        assert_eq!(
            config.local_model.embedding_url.as_deref(),
            Some("http://127.0.0.1:8083")
        );
    }

    #[test]
    fn config_template_sets_hnsw_rebuild_threshold() {
        let config: crate::config::model::Config = toml::from_str(DEFAULT_CONFIG).unwrap();
        assert_eq!(config.semantic.hnsw_rebuild_threshold(), 500);
    }

    #[test]
    fn config_template_excludes_agent_dotfiles() {
        let config: crate::config::model::Config = toml::from_str(DEFAULT_CONFIG).unwrap();
        let patterns = &config.watch.ignore_patterns;
        assert!(
            patterns.iter().any(|p| p == ".claude/**"),
            "missing .claude/**"
        );
        assert!(
            patterns.iter().any(|p| p == ".agents/**"),
            "missing .agents/**"
        );
        assert!(
            patterns.iter().any(|p| p == ".codex/**"),
            "missing .codex/**"
        );
        assert!(
            patterns.iter().any(|p| p == ".opencode/**"),
            "missing .opencode/**"
        );
        assert!(
            patterns.iter().any(|p| p == "target/**"),
            "missing target/**"
        );
    }
}
