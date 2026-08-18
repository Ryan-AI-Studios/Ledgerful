//! Legacy single-backend complete path for `ask` (no provider-priority chain).

use crate::commands::ask::{
    AskTimeoutKind, Backend, ask_completion_options, degrade_to_context, run_gemini_synthesis,
    sanitize_error_for_logging,
};
use crate::config::model::Config;
use crate::gemini::modes::GeminiMode;
use crate::impact::packet::ImpactPacket;
use crate::local_model::context::AdaptiveMode;
use crate::local_model::pruner::RankedChunk;
use miette::Result;
use owo_colors::{OwoColorize, Stream, Style};

/// Named inputs for the legacy `match resolved_backend` complete path.
pub(crate) struct LegacyCompleteInputs<'a> {
    pub config: &'a Config,
    pub resolved_backend: Backend,
    pub timeout_kind: AskTimeoutKind,
    pub effective_timeout: u64,
    pub complete_override: Option<u64>,
    pub gemini_timeout: u64,
    pub base_system_prompt: &'a str,
    pub user_prompt: &'a str,
    pub relevant_chunks: &'a [RankedChunk],
    pub latest_packet: &'a ImpactPacket,
    pub adaptive_mode: AdaptiveMode,
    pub truncated: bool,
    pub mode: GeminiMode,
}

pub(crate) fn execute_legacy_complete(inputs: LegacyCompleteInputs<'_>) -> Result<()> {
    let LegacyCompleteInputs {
        config,
        resolved_backend,
        timeout_kind,
        effective_timeout,
        complete_override,
        gemini_timeout,
        base_system_prompt,
        user_prompt,
        relevant_chunks,
        latest_packet,
        adaptive_mode,
        truncated,
        mode,
    } = inputs;

    match resolved_backend {
        Backend::Local | Backend::OllamaCloud | Backend::OpenRouter => {
            let max_tokens = config.local_model.context_window;

            // B2: skip fixed 5s Local HTTP probe — status classification
            // (401/429/503) happens on the complete path (L1). Reachability
            // and connect budgets live in complete (B2b/B3).

            let messages = crate::local_model::context::assemble_context(
                base_system_prompt,
                user_prompt,
                relevant_chunks,
                max_tokens,
                adaptive_mode,
            );

            // Show progress indicator before LLM call with backend selection
            eprintln!("Using local/cloud model...");
            // B4: wait honesty when Local effective budget is large enough
            // that cold load may consume most of it.
            if matches!(timeout_kind, AskTimeoutKind::Local) && effective_timeout >= 60 {
                eprintln!(
                    "Waiting for local model (up to {effective_timeout}s; cold load may use most of this)…"
                );
            }
            eprintln!("Contacting LLM...");

            match crate::local_model::client::complete_with_hard_deadline(
                &config.local_model,
                &messages,
                &ask_completion_options(),
                complete_override,
            ) {
                Ok(response) => {
                    println!(
                        "\n{}",
                        "Local Model Response:".if_supports_color(Stream::Stdout, |s| s
                            .style(Style::new().bold().green()))
                    );
                    println!("{response}");
                    Ok(())
                }
                Err(e) => {
                    let raw = e.to_string();
                    let err_str = sanitize_error_for_logging(&raw);
                    // M6/M7: compact for degrade path and miette; full multi-line once on stderr.
                    let compact = crate::local_model::client::compact_completion_error(&err_str);
                    if crate::commands::ask::render::is_degradable_error(&raw) {
                        // Transport-level failure during synthesis — degrade
                        // to context render instead of hard-failing.
                        return degrade_to_context(config, relevant_chunks, &compact, || {
                            run_gemini_synthesis(
                                config,
                                base_system_prompt,
                                user_prompt,
                                relevant_chunks,
                                gemini_timeout,
                                mode,
                                latest_packet,
                                adaptive_mode,
                                truncated,
                            )
                        });
                    }
                    // Full multi-cause report once on terminal (M6/M7).
                    eprintln!("{}", err_str.if_supports_color(Stream::Stderr, |s| s.red()));
                    // Timeout remediations when **local** cause is timeout even if
                    // primary is cloud content-quality (0160). For multi-cause reports,
                    // only inspect the Local: section (Next: may mention --timeout generically).
                    let local_timeout =
                        if crate::local_model::client::is_multi_cause_fallback_error(&raw) {
                            crate::local_model::client::local_cause_is_timeout(&raw)
                        } else {
                            crate::commands::ask::render::is_timeout_error(&raw)
                        };
                    if local_timeout {
                        eprintln!(
                            "{}",
                            format!(
                                "Hint: Local model timed out after ~{effective_timeout}s. Raise --timeout or local_model.timeout_secs; warm/preload the model; or try --backend gemini."
                            )
                            .if_supports_color(Stream::Stderr, |s| s.yellow())
                        );
                    }
                    if raw.contains("401") {
                        eprintln!(
                            "{}",
                            "Hint: Check your OLLAMA_CLOUD_API_KEY or ollama_key in config.toml"
                                .if_supports_color(Stream::Stderr, |s| s.yellow())
                        );
                    }
                    if raw.contains("api.ollama.com") {
                        eprintln!(
                            "{}",
                            "Hint: Use ollama_cloud_url = \"https://ollama.com/api\" (native) or \"https://ollama.com\" (OpenAI-compatible)"
                                .if_supports_color(Stream::Stderr, |s| s.yellow())
                        );
                    }
                    // M7: miette compact single-line — never dump full multi-line body twice.
                    Err(miette::miette!("Local model failed: {compact}"))
                }
            }
        }
        Backend::Gemini => run_gemini_synthesis(
            config,
            base_system_prompt,
            user_prompt,
            relevant_chunks,
            gemini_timeout,
            mode,
            latest_packet,
            adaptive_mode,
            truncated,
        ),
    }
}
