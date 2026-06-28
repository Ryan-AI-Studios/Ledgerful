use serde::{Deserialize, Serialize};

pub const DEFAULT_GEMINI_FAST_MODEL: &str = "gemini-3.1-flash-lite";
pub const DEFAULT_GEMINI_DEEP_MODEL: &str = "gemini-3.1-pro";

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct GeminiConfig {
    pub api_key: Option<String>,
    /// Optional override used for every Gemini request.
    pub model: Option<String>,
    /// Default for routine, low-latency Ledgerful ask modes.
    pub fast_model: Option<String>,
    /// Default for high-risk or review-heavy Ledgerful ask modes.
    pub deep_model: Option<String>,
    pub timeout_secs: Option<u64>,
    /// Context window size in tokens for the Gemini model.
    /// Used to derive the truncation budget for prompt submission:
    /// char_limit = context_window * 4 * 80 / 100 (4 chars/token, 80% reserved for context).
    #[serde(default = "default_context_window")]
    pub context_window: usize,
}

impl Default for GeminiConfig {
    fn default() -> Self {
        Self {
            api_key: None,
            model: None,
            fast_model: None,
            deep_model: None,
            timeout_secs: None,
            context_window: default_context_window(),
        }
    }
}

fn default_context_window() -> usize {
    128_000
}
