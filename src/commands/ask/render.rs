use crate::config::model::Config;
use crate::gemini::modes::GeminiMode;
use crate::gemini::wrapper::run_query;
use crate::local_model::pruner;
use miette::Result;
use owo_colors::OwoColorize;

/// TA14: Execute ask with a provider priority fallback chain.
/// Tries each provider in order with per-provider timeout. Degradable
/// errors trigger fallback to the next provider. If all providers fail,
/// degrades to context-only output (R4).
#[allow(clippy::too_many_arguments)]
pub(crate) fn execute_ask_with_providers(
    config: &Config,
    base_system_prompt: &str,
    user_prompt: &str,
    relevant_chunks: &[pruner::RankedChunk],
    default_timeout_secs: u64,
    mode: GeminiMode,
    latest_packet: &crate::impact::packet::ImpactPacket,
    adaptive_mode: crate::local_model::context::AdaptiveMode,
    truncated: bool,
    entries: &[crate::config::model::ProviderEntry],
) -> Result<()> {
    use crate::config::model::Provider;

    for entry in entries {
        let provider_name = entry.backend.display_name();
        let provider_timeout = entry.timeout_secs.unwrap_or(default_timeout_secs);

        match entry.backend {
            Provider::Local | Provider::OllamaCloud | Provider::OpenRouter => {
                // Apply per-provider overrides from ProviderEntry (TA14 R1)
                let mut provider_config = config.local_model.clone();
                if let Some(ref model) = entry.model {
                    provider_config.generation_model = model.clone();
                }
                if let Some(ref base_url) = entry.base_url {
                    // For OllamaCloud, override ollama_cloud_url; for Local, override base_url
                    match entry.backend {
                        Provider::OllamaCloud => {
                            provider_config.ollama_cloud_url = Some(base_url.clone());
                            if let Some(ref key_env) = entry.api_key_env
                                && let Ok(key) = std::env::var(key_env)
                            {
                                provider_config.ollama_cloud_api_key = Some(key);
                            }
                        }
                        Provider::Local => {
                            provider_config.base_url = base_url.clone();
                        }
                        Provider::OpenRouter => {
                            // OpenRouter uses its own base_url in the fallback chain
                        }
                        _ => {}
                    }
                }
                // For OllamaCloud, apply model override to ollama_cloud_model
                if entry.backend == Provider::OllamaCloud
                    && let Some(ref model) = entry.model
                {
                    provider_config.ollama_cloud_model = Some(model.clone());
                }

                let max_tokens = provider_config.context_window;
                let messages = crate::local_model::context::assemble_context(
                    base_system_prompt,
                    user_prompt,
                    relevant_chunks,
                    max_tokens,
                    adaptive_mode,
                );

                eprintln!("Using {provider_name}...");
                eprintln!("Contacting LLM...");

                match crate::local_model::client::complete_with_hard_deadline(
                    &provider_config,
                    &messages,
                    &crate::commands::ask::ask_completion_options(),
                    Some(provider_timeout),
                ) {
                    Ok(response) => {
                        println!("\n{}", "Response:".bold().green());
                        println!("{response}");
                        return Ok(());
                    }
                    Err(e) => {
                        let err_str =
                            crate::commands::ask::sanitize_error_for_logging(&e.to_string());
                        if is_degradable_error(&e.to_string()) {
                            eprintln!(
                                "{}",
                                format!(
                                    "{provider_name} failed ({err_str}); trying next provider..."
                                )
                                .yellow()
                            );
                            tracing::warn!("{provider_name} failed: {err_str}");
                            continue;
                        }
                        eprintln!("{}", err_str.red());
                        continue;
                    }
                }
            }
            Provider::Gemini => {
                eprintln!("Using {provider_name}...");
                match run_gemini_synthesis_with(
                    config,
                    base_system_prompt,
                    user_prompt,
                    relevant_chunks,
                    provider_timeout,
                    mode,
                    latest_packet,
                    adaptive_mode,
                    truncated,
                    entry.model.as_deref(),
                ) {
                    Ok(()) => return Ok(()),
                    Err(e) => {
                        let err_str =
                            crate::commands::ask::sanitize_error_for_logging(&format!("{e}"));
                        if is_degradable_error(&err_str) {
                            eprintln!(
                                "{}",
                                format!(
                                    "{provider_name} failed ({err_str}); trying next provider..."
                                )
                                .yellow()
                            );
                            tracing::warn!("{provider_name} failed: {err_str}");
                            continue;
                        }
                        eprintln!("{}", err_str.red());
                        continue;
                    }
                }
            }
        }
    }

    // All providers exhausted — degrade to context-only output (R4)
    eprintln!(
        "{}",
        "All providers exhausted. Degrading to context-only output.".yellow()
    );
    render_retrieved_context(relevant_chunks);
    Ok(())
}

/// Run Gemini backend synthesis. Extracted from the `Backend::Gemini` arm so the
/// interactive cloud fallback in `degrade_to_context` can reuse it without
/// re-routing the whole command.
#[allow(clippy::too_many_arguments)]
pub(crate) fn run_gemini_synthesis(
    config: &Config,
    base_system_prompt: &str,
    user_prompt: &str,
    relevant_chunks: &[pruner::RankedChunk],
    timeout_secs: u64,
    mode: GeminiMode,
    latest_packet: &crate::impact::packet::ImpactPacket,
    adaptive_mode: crate::local_model::context::AdaptiveMode,
    truncated: bool,
) -> Result<()> {
    run_gemini_synthesis_with(
        config,
        base_system_prompt,
        user_prompt,
        relevant_chunks,
        timeout_secs,
        mode,
        latest_packet,
        adaptive_mode,
        truncated,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
fn run_gemini_synthesis_with(
    config: &Config,
    base_system_prompt: &str,
    user_prompt: &str,
    relevant_chunks: &[pruner::RankedChunk],
    timeout_secs: u64,
    mode: GeminiMode,
    latest_packet: &crate::impact::packet::ImpactPacket,
    adaptive_mode: crate::local_model::context::AdaptiveMode,
    truncated: bool,
    model_override: Option<&str>,
) -> Result<()> {
    // 0073: hard-block direct Gemini under CloudPolicy::Forbidden.
    if let Err(e) =
        crate::local_model::cloud_policy::deny_if_forbidden("run_gemini_synthesis blocked")
    {
        return Err(miette::miette!("{e}"));
    }

    eprintln!("Using Gemini...");

    let budget_tokens = config.gemini.context_window;

    let user_prompt = if truncated {
        format!("{user_prompt}\n\n[Packet truncated for Gemini submission]")
    } else {
        user_prompt.to_string()
    };

    let messages = crate::local_model::context::assemble_context(
        base_system_prompt,
        &user_prompt,
        relevant_chunks,
        budget_tokens,
        adaptive_mode,
    );

    let final_sys_prompt = &messages[0].content;

    let mut final_usr_prompt = String::new();
    if messages.len() > 2 {
        final_usr_prompt.push_str("## Codebase Context Chunks\n\n");
        for msg in &messages[1..messages.len() - 1] {
            final_usr_prompt.push_str(&msg.content);
            final_usr_prompt.push_str("\n\n");
        }
    }
    if messages.len() > 1 {
        final_usr_prompt.push_str("User Question: ");
        if let Some(last) = messages.last() {
            final_usr_prompt.push_str(&last.content);
        }
    } else {
        final_usr_prompt.push_str(&user_prompt);
    }

    // Single sanitize pass (sanitize_for_egress); no second pass in run_query.
    let sanitize_result = crate::gemini::sanitize::sanitize_for_egress(&final_usr_prompt);
    let sanitized_user_prompt = sanitize_result.sanitized;

    let model = model_override
        .filter(|m| !m.is_empty())
        .map(|m| m.to_string())
        .unwrap_or_else(|| {
            crate::gemini::wrapper::select_gemini_model(&config.gemini, mode, latest_packet)
        });

    run_query(
        final_sys_prompt,
        &sanitized_user_prompt,
        Some(timeout_secs),
        &model,
        config.gemini.api_key.as_deref(),
    )
}

/// Graceful degradation: when the local completion model is
/// unreachable, warn the user, optionally offer an interactive switch to the
/// configured Gemini backend, and otherwise render the gathered retrieval
/// context directly. Returns `Ok(())` so `ask` never hard-fails on a
/// transport-level local-model outage.
///
/// The interactive Gemini prompt only fires when a Gemini key is configured
/// directly in `config.toml` (`config.gemini.api_key`). The env-key path
/// (`GEMINI_API_KEY`) is already attempted inside `complete()`'s cloud fallback
/// chain, so re-prompting for it would just retry a known failure. The prompt
/// is also gated on `util::term::is_interactive()` so it never blocks in
/// non-interactive/CI sessions.
pub(crate) fn degrade_to_context(
    config: &Config,
    relevant_chunks: &[pruner::RankedChunk],
    err: &str,
    gemini_synthesis: impl FnOnce() -> Result<()>,
) -> Result<()> {
    let base_url = config
        .local_model
        .generation_url
        .as_deref()
        .filter(|u| !u.is_empty())
        .unwrap_or(&config.local_model.base_url);
    eprintln!("{}", degrade_warning(base_url).yellow());
    tracing::warn!("Local completion degraded to context render: {err}");

    if should_prompt_for_cloud(config, crate::util::term::is_interactive()) {
        use inquire::Confirm;
        if let Ok(true) = Confirm::new(
            "Local model unavailable. Try with Gemini instead? (Requires GEMINI_API_KEY)",
        )
        .with_default(true)
        .prompt()
        {
            return gemini_synthesis();
        }
    }

    render_retrieved_context(relevant_chunks);
    Ok(())
}

/// Gate predicate for the interactive Gemini-switch prompt inside
/// `degrade_to_context`. Extracted as a pure helper so the gate condition is
/// unit-testable without driving the full degrade path (which would require
/// capturing stdout/stderr and an `inquire` prompt).
///
/// Under `CloudPolicy::Forbidden` always returns false (0073 — non-interactive
/// MCP and Forbidden never degrade→Gemini). Otherwise:
/// `interactive && config.gemini.api_key.is_some()`.
///
/// `interactive` is passed in (rather than read from `util::term` inside) so
/// tests can inject a deterministic value without mutating process env or
/// depending on whether stdin is a tty.
pub(crate) fn should_prompt_for_cloud(config: &Config, interactive: bool) -> bool {
    if crate::local_model::CloudPolicy::from_env().is_forbidden() {
        return false;
    }
    interactive && config.gemini.api_key.is_some()
}

/// Render gathered retrieval context to stdout when LLM synthesis is skipped.
/// Emits a deterministic, ranked view of the chunks that would have been sent
/// to the LLM (code snippets, KG neighborhood, documentation).
pub(crate) fn render_retrieved_context(chunks: &[pruner::RankedChunk]) {
    println!("\n{}", degrade_context_header().bold().cyan());
    print!("{}", format_retrieved_context_body(chunks));
}

/// Spec-pinned header for the degraded-context render. Extracted as a pure
/// helper so the exact wording is unit-testable without capturing stdout.
pub(crate) fn degrade_context_header() -> &'static str {
    "Retrieved context (local model unavailable, skipping synthesis):"
}

/// Spec-pinned warning emitted when the local completion model is unreachable
/// and `ask` degrades to graph/semantic search. Extracted as a pure helper so
/// the exact wording (including the configured URL) is unit-testable without
/// capturing stderr.
pub(crate) fn degrade_warning(base_url: &str) -> String {
    format!(
        "Warning: Local completion model at {} is unreachable. Falling back to graph/semantic search.",
        base_url
    )
}

/// Build the body of the degraded-context output as a string. Separated from
/// `render_retrieved_context` so the deterministic ranking/format is unit-testable
/// without capturing stdout.
pub(crate) fn format_retrieved_context_body(chunks: &[pruner::RankedChunk]) -> String {
    if chunks.is_empty() {
        return "(no retrieval context available for this query)\n".to_string();
    }
    // Deterministic order: highest score first, then by source for stable ties.
    let mut sorted: Vec<&pruner::RankedChunk> = chunks.iter().collect();
    sorted.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.source.cmp(&b.source))
    });
    let mut out = String::new();
    for (idx, chunk) in sorted.iter().enumerate() {
        out.push_str(&format!(
            "\n--- [{}] {} (score: {:.3}) ---\n",
            idx + 1,
            chunk.source,
            chunk.score
        ));
        out.push_str(&chunk.content);
        out.push('\n');
    }
    out
}

/// Classify a local-model error string as degradable (transport/unreachable/timeout
/// or transient server-unavailability) versus non-degradable (auth/rate-limit/other).
/// Degradable errors fall back to rendering retrieved context; non-degradable keep
/// the existing hard-fail behavior.
pub(crate) fn is_degradable_error(err: &str) -> bool {
    // cloud_policy_forbidden may embed "unreachable" in the message; treat it
    // as non-degradable for the interactive Gemini arm (context-only degrade
    // still happens when callers choose it, but we must not treat policy as a
    // soft transport failure that re-opens cloud paths).
    if err.contains(crate::local_model::CLOUD_POLICY_FORBIDDEN_CODE) {
        return false;
    }
    let lower = err.to_lowercase();
    lower.contains("unreachable")
        || lower.contains("timed out")
        || lower.contains("connection refused")
        || lower.contains("timeout")
        || lower.contains("os error")
        || lower.contains("not reachable")
        || lower.contains("503")
        || lower.contains("502")
        || lower.contains("504")
        || lower.contains("service unavailable")
        || lower.contains("bad gateway")
        || lower.contains("gateway timeout")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::model::Config;

    #[test]
    fn degradable_error_classification_transport() {
        assert!(is_degradable_error(
            "Local model server at http://127.0.0.1:1 is unreachable"
        ));
        assert!(is_degradable_error("Local model timed out after 5s"));
        assert!(is_degradable_error("connection refused (os error 10061)"));
        assert!(is_degradable_error("Local model not reachable at host"));
        assert!(is_degradable_error("Timed Out after 3s"));
    }

    #[test]
    fn degradable_error_classification_transient_server_unavailable() {
        assert!(is_degradable_error(
            "503 server error (Service Unavailable)"
        ));
        assert!(is_degradable_error("ollama returned 503: model warming"));
        assert!(is_degradable_error("502 server error (Bad Gateway)"));
        assert!(is_degradable_error("cloud returned 504: Gateway Timeout"));
        assert!(is_degradable_error("Service Unavailable"));
        assert!(is_degradable_error("Bad Gateway"));
        assert!(is_degradable_error("Gateway Timeout"));
        assert!(is_degradable_error("SERVICE UNAVAILABLE"));
        assert!(is_degradable_error("service unavailable"));
    }

    #[test]
    fn degradable_error_classification_non_degradable() {
        assert!(!is_degradable_error("401 server error (unauthorized)"));
        assert!(!is_degradable_error("rate limited. Wait a moment."));
        assert!(!is_degradable_error("Failed to parse completion response"));
        assert!(!is_degradable_error("returned empty message content"));
        assert!(!is_degradable_error("429 Too Many Requests"));
        assert!(!is_degradable_error("500 server error (internal)"));
        assert!(!is_degradable_error("cloud returned 500: boom"));
        assert!(!is_degradable_error(
            "cloud_policy_forbidden: local model at http://127.0.0.1:1 unreachable; cloud fallback denied"
        ));
    }

    #[test]
    fn format_retrieved_context_empty() {
        let body = format_retrieved_context_body(&[]);
        assert!(
            body.contains("no retrieval context available"),
            "expected empty marker, got: {body}"
        );
    }

    #[test]
    fn format_retrieved_context_sorted_and_ranked() {
        use crate::local_model::pruner::RankedChunk;
        let chunks = vec![
            RankedChunk {
                source: "src/b.rs:: low".to_string(),
                content: "low score body".to_string(),
                score: 0.2,
            },
            RankedChunk {
                source: "src/a.rs:: high".to_string(),
                content: "high score body".to_string(),
                score: 0.9,
            },
            RankedChunk {
                source: "src/c.rs:: mid".to_string(),
                content: "mid score body".to_string(),
                score: 0.5,
            },
        ];
        let body = format_retrieved_context_body(&chunks);
        let high_pos = body.find("high score body").unwrap();
        let mid_pos = body.find("mid score body").unwrap();
        let low_pos = body.find("low score body").unwrap();
        assert!(high_pos < mid_pos, "high must precede mid: {body}");
        assert!(mid_pos < low_pos, "mid must precede low: {body}");
        assert!(body.contains("src/a.rs:: high"));
        assert!(body.contains("score: 0.900"));
    }

    #[test]
    fn degrade_warning_pins_spec_text() {
        let warning = degrade_warning("http://127.0.0.1:1");
        assert_eq!(
            warning,
            "Warning: Local completion model at http://127.0.0.1:1 is unreachable. Falling back to graph/semantic search."
        );
        assert_eq!(
            degrade_context_header(),
            "Retrieved context (local model unavailable, skipping synthesis):"
        );
    }

    #[test]
    fn degrade_render_emits_spec_header_and_ranked_body() {
        use crate::local_model::pruner::RankedChunk;
        let chunks = vec![
            RankedChunk {
                source: "src/low.rs".to_string(),
                content: "low body".to_string(),
                score: 0.2,
            },
            RankedChunk {
                source: "src/high.rs".to_string(),
                content: "high body".to_string(),
                score: 0.9,
            },
        ];
        let header = degrade_context_header();
        let body = format_retrieved_context_body(&chunks);
        assert_eq!(
            header,
            "Retrieved context (local model unavailable, skipping synthesis):"
        );
        let high_pos = body.find("high body").unwrap();
        let low_pos = body.find("low body").unwrap();
        assert!(high_pos < low_pos, "high must precede low: {body}");
        assert!(body.contains("src/high.rs"));
        assert!(body.contains("score: 0.900"));
    }

    #[test]
    fn should_prompt_for_cloud_gate_skips_non_interactive_even_with_key() {
        let mut config = Config::default();
        config.gemini.api_key = Some("test-key".to_string());
        assert!(
            !should_prompt_for_cloud(&config, false),
            "non-interactive session must skip the cloud prompt even with a key configured"
        );
        assert!(should_prompt_for_cloud(&config, true));
    }

    #[test]
    fn should_prompt_for_cloud_gate_skips_when_no_key_configured() {
        let config = Config::default();
        assert!(!should_prompt_for_cloud(&config, true));
        assert!(!should_prompt_for_cloud(&config, false));
    }

    #[test]
    #[serial_test::serial(env)]
    fn should_prompt_for_cloud_gate_skips_under_forbidden() {
        mod env_guard {
            include!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/integration/common/env_guard.rs"
            ));
        }
        use crate::local_model::cloud_policy::{CLOUD_POLICY_ENV, CLOUD_POLICY_FORBIDDEN_VALUE};
        use env_guard::TempEnv;

        let _pol = TempEnv::set(CLOUD_POLICY_ENV, CLOUD_POLICY_FORBIDDEN_VALUE);
        let mut config = Config::default();
        config.gemini.api_key = Some("test-key".to_string());
        assert!(
            !should_prompt_for_cloud(&config, true),
            "Forbidden must never prompt for Gemini degrade"
        );
    }
}
