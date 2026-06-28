use serde::{Deserialize, Serialize};

/// `[ask]` configuration — controls `ledgerful ask` behavior.
///
/// Currently governs whether `ask` auto-computes a fresh `ImpactPacket`
/// in-memory (Track DX6) instead of reading the cached/stored packet and
/// emitting a staleness warning. See `commands::ask::execute_ask` for the
/// resolution rule (`--auto-scan` flag OR `auto_scan_default = true`).
#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub struct AskConfig {
    /// When true, `ledgerful ask` computes a fresh impact packet in-memory
    /// on every invocation, suppressing the "stale impact report" warning.
    /// Equivalent to always passing `--auto-scan`. Defaults to `false` (the
    /// derived `Default` for `bool`) to preserve the existing cached-packet
    /// behavior unless opted in.
    #[serde(default)]
    pub auto_scan_default: bool,

    /// Configurable provider priority list for `ask` (Track TA14).
    /// When non-empty, the `ask` command tries each provider in order,
    /// falling back to the next on failure. When empty, legacy
    /// `resolve_backend` logic is used (backward compatibility).
    #[serde(default)]
    pub providers: ProvidersConfig,
}

/// Provider priority configuration for `[ask.providers]`.
#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub struct ProvidersConfig {
    /// Ordered list of providers to try for `ask` commands.
    /// Example:
    /// ```toml
    /// [[ask.providers.priority]]
    /// backend = "ollama_cloud"
    /// model = "glm-5.2"
    /// timeout_secs = 30
    /// ```
    #[serde(default)]
    pub priority: Vec<ProviderEntry>,
}

/// A single provider entry in the priority list.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderEntry {
    /// Which backend to use for this provider.
    pub backend: Provider,
    /// Model name (optional — falls back to the backend's default).
    #[serde(default)]
    pub model: Option<String>,
    /// Per-provider timeout in seconds (optional — falls back to backend default).
    #[serde(default)]
    pub timeout_secs: Option<u64>,
    /// Env var name to read the API key from (optional — falls back to backend default).
    #[serde(default)]
    pub api_key_env: Option<String>,
    /// Override base URL (optional — falls back to backend default).
    #[serde(default)]
    pub base_url: Option<String>,
}

/// All supported LLM provider backends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    OllamaCloud,
    Gemini,
    Local,
    OpenRouter,
}

impl Provider {
    /// Parse a provider name from a string, returning a clear error on
    /// unknown values (R7: fail fast on invalid env var overrides).
    pub fn from_str_fail_fast(s: &str, source: &str) -> Result<Self, String> {
        match s.trim().to_lowercase().as_str() {
            "ollama_cloud" => Ok(Self::OllamaCloud),
            "gemini" => Ok(Self::Gemini),
            "local" => Ok(Self::Local),
            "openrouter" => Ok(Self::OpenRouter),
            _ => Err(format!(
                "Invalid provider '{}' in {}. Valid values: ollama_cloud, gemini, local, openrouter",
                s, source
            )),
        }
    }

    pub fn display_name(&self) -> &'static str {
        match self {
            Self::OllamaCloud => "OllamaCloud",
            Self::Gemini => "Gemini",
            Self::Local => "Local",
            Self::OpenRouter => "OpenRouter",
        }
    }
}
