pub(crate) mod backend;
pub(crate) mod context;
pub(crate) mod execute;
pub(crate) mod render;

// Re-export the public API surface so existing `crate::commands::ask::*`
// import paths keep working after the split.
pub use backend::{
    resolve_backend, resolve_backend_with, resolve_provider_entries, resolve_provider_priority,
    sanitize_error_for_logging,
};
pub use context::{build_ask_user_prompt, escape_cozo_string, should_prune_impact};
pub use execute::execute_ask;

// `Backend` is referenced by `cli::args` and `config_verify`, so it must stay
// at the crate-visible `pub` level.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, clap::ValueEnum,
)]
#[serde(rename_all = "snake_case")]
pub enum Backend {
    Local,
    Gemini,
    OllamaCloud,
    OpenRouter,
}

// Internal helpers and constants used across the submodules.
pub(crate) fn ask_completion_options() -> crate::local_model::client::CompletionOptions {
    crate::local_model::client::CompletionOptions {
        max_tokens: 512,
        ..crate::local_model::client::CompletionOptions::default()
    }
}

// Re-expose private implementation helpers to the submodules that need them.
pub(crate) use context::{fetch_kg_bm25, fetch_kg_neighborhood, gather_semantic_chunks};
pub(crate) use render::{degrade_to_context, execute_ask_with_providers, run_gemini_synthesis};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::model::{Config, GeminiConfig};
    use crate::gemini::modes::GeminiMode;

    #[test]
    fn test_select_gemini_model_logic() {
        let packet = crate::impact::packet::ImpactPacket::default();

        // 1. Defaults
        unsafe {
            std::env::remove_var("GEMINI_FAST_MODEL");
            std::env::remove_var("GEMINI_DEEP_MODEL");
        }
        let config = GeminiConfig {
            fast_model: Some("fast".to_string()),
            deep_model: Some("deep".to_string()),
            ..GeminiConfig::default()
        };
        let fast_model =
            crate::gemini::wrapper::select_gemini_model(&config, GeminiMode::Suggest, &packet);
        assert_eq!(fast_model, "fast");

        let deep_model =
            crate::gemini::wrapper::select_gemini_model(&config, GeminiMode::ReviewPatch, &packet);
        assert_eq!(deep_model, "deep");

        // 2. Config Overrides
        let config_custom = GeminiConfig {
            model: Some("custom".to_string()),
            ..GeminiConfig::default()
        };
        let model = crate::gemini::wrapper::select_gemini_model(
            &config_custom,
            GeminiMode::Suggest,
            &packet,
        );
        assert_eq!(model, "custom");

        // 3. Env Overrides
        unsafe {
            std::env::set_var("GEMINI_FAST_MODEL", "env-fast");
            std::env::set_var("GEMINI_DEEP_MODEL", "env-deep");
        }
        let config_empty = GeminiConfig::default();
        let fast_model_env = crate::gemini::wrapper::select_gemini_model(
            &config_empty,
            GeminiMode::Suggest,
            &packet,
        );
        assert_eq!(fast_model_env, "env-fast");

        let deep_model_env = crate::gemini::wrapper::select_gemini_model(
            &config_empty,
            GeminiMode::ReviewPatch,
            &packet,
        );
        assert_eq!(deep_model_env, "env-deep");

        unsafe {
            std::env::remove_var("GEMINI_FAST_MODEL");
            std::env::remove_var("GEMINI_DEEP_MODEL");
        }
    }

    #[test]
    fn ask_completion_options_are_bounded() {
        let options = ask_completion_options();
        assert_eq!(options.max_tokens, 512);
        assert!(options.max_tokens < Config::default().local_model.context_window);
    }

    #[test]
    fn test_default_context_window_yields_hardcoded_budget() {
        let config = GeminiConfig::default(); // defaults to 128,000
        let min_context_chars: usize = 32_768;
        let char_limit =
            (config.context_window as u64 * 4 * 80 / 100).max(min_context_chars as u64) as usize;
        assert_eq!(char_limit, 409_600);
    }

    #[test]
    fn test_custom_context_window_adjusts_budget() {
        let config = GeminiConfig {
            context_window: 200_000,
            ..Default::default()
        };
        let min_context_chars: usize = 32_768;
        let char_limit =
            (config.context_window as u64 * 4 * 80 / 100).max(min_context_chars as u64) as usize;
        assert_eq!(char_limit, 640_000);
    }

    #[test]
    fn test_small_context_window_budget() {
        let config = GeminiConfig {
            context_window: 32_000,
            ..Default::default()
        };
        let min_context_chars: usize = 32_768;
        let char_limit =
            (config.context_window as u64 * 4 * 80 / 100).max(min_context_chars as u64) as usize;
        assert_eq!(char_limit, 102_400);
    }

    #[test]
    fn test_zero_context_window_fallback() {
        let config = GeminiConfig {
            context_window: 0,
            ..Default::default()
        };
        let min_context_chars: usize = 32_768;
        let char_limit =
            (config.context_window as u64 * 4 * 80 / 100).max(min_context_chars as u64) as usize;
        assert_eq!(char_limit, 32_768);
    }
}
