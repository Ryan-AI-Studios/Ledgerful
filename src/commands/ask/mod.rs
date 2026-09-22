pub(crate) mod backend;
pub(crate) mod context;
pub(crate) mod execute;
pub(crate) mod gather;
pub(crate) mod legacy_complete;
pub(crate) mod mix;
pub(crate) mod render;

// Re-export the public API surface so existing `crate::commands::ask::*`
// import paths keep working after the split.
pub use backend::{
    AskTimeoutKind, DEFAULT_CLOUD_FALLBACK_TIMEOUT_SECS, DEFAULT_CLOUD_PROVIDER_TIMEOUT_SECS,
    complete_timeout_override, resolve_ask_timeout, resolve_backend, resolve_backend_with,
    resolve_provider_entries, resolve_provider_priority, sanitize_error_for_logging,
};
pub use context::{build_ask_user_prompt, escape_cozo_string, should_prune_impact};
pub use execute::{ExecuteAskOpts, execute_ask};

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
pub(crate) const ASK_LENGTH_STOP_FOOTER: &str = "Answer truncated: model stop reason length.";

pub(crate) fn ask_completion_options() -> crate::local_model::client::CompletionOptions {
    crate::local_model::client::CompletionOptions {
        max_tokens: 1024,
        ..crate::local_model::client::CompletionOptions::default()
    }
}

/// After the answer is on stdout: length stops are a miette error so `main`
/// prints this sentence once and exits 1. Any other reason is success.
pub(crate) fn finish_printed_ask(stop_reason: Option<&str>) -> miette::Result<()> {
    if crate::local_model::client::is_length_stop(stop_reason) {
        Err(miette::miette!(ASK_LENGTH_STOP_FOOTER))
    } else {
        Ok(())
    }
}

// Re-expose private implementation helpers to the submodules that need them.
pub(crate) use context::{fetch_kg_bm25, fetch_kg_neighborhood, gather_semantic_chunks};
pub(crate) use render::{degrade_to_context, execute_ask_with_providers, run_gemini_synthesis};

#[cfg(test)]
mod length_stop_tests {
    use super::{ASK_LENGTH_STOP_FOOTER, finish_printed_ask};
    use crate::local_model::client::is_length_stop;

    #[test]
    fn is_length_stop_matches_length_only() {
        assert!(is_length_stop(Some("length")));
        assert!(is_length_stop(Some("LENGTH")));
        assert!(is_length_stop(Some("Length")));
        for reason in [
            None,
            Some(""),
            Some("stop"),
            Some("load"),
            Some("unload"),
            Some("content_filter"),
            Some("tool_calls"),
            Some("function_call"),
            Some("max_tokens"),
        ] {
            assert!(!is_length_stop(reason), "{reason:?}");
        }
    }

    #[test]
    fn ask_length_stop_footer_is_the_miette_message() {
        let err = finish_printed_ask(Some("length")).unwrap_err();
        assert_eq!(err.to_string(), ASK_LENGTH_STOP_FOOTER);
        assert!(finish_printed_ask(Some("STOP")).is_ok());
        assert!(finish_printed_ask(None).is_ok());
        assert!(finish_printed_ask(Some("max_tokens")).is_ok());
    }

    #[test]
    fn ask_length_stop_print_arms_share_finish_printed_ask() {
        let legacy = include_str!("legacy_complete.rs");
        let render = include_str!("render.rs");
        assert!(
            legacy.contains("finish_printed_ask("),
            "legacy complete must use the shared footer helper"
        );
        assert!(
            render.contains("finish_printed_ask("),
            "provider-priority complete must use the shared footer helper"
        );
    }
}
