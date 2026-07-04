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
