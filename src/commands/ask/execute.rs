use crate::commands::ask::gather::{gather_impact_and_bridge, gather_semantic_and_kg};
use crate::commands::ask::legacy_complete::{LegacyCompleteInputs, execute_legacy_complete};
use crate::commands::ask::{
    Backend, build_ask_user_prompt, resolve_backend, resolve_provider_entries,
};
use crate::commands::helpers::{get_layout, load_ledger_config};
use crate::config::model::Config;
use crate::gemini::modes::GeminiMode;
use crate::index::warn_if_stale;
use crate::state::layout::Layout;
use crate::state::storage::StorageManager;
use miette::Result;
use owo_colors::{OwoColorize, Stream};
use std::env;

const MIN_CONTEXT_CHARS: usize = 32_768;

/// Named inputs for [`execute_ask`] (not clap `AskArgs`).
#[derive(Debug, Clone)]
pub struct ExecuteAskOpts {
    pub query: Option<String>,
    pub semantic: bool,
    pub limit: usize,
    pub mode: GeminiMode,
    pub narrative: bool,
    pub backend: Option<Backend>,
    pub auto_index: bool,
    pub timeout_secs: Option<u64>,
    pub no_kg_fallback: bool,
    pub auto_scan: bool,
}

/// Entry point for the `ledgerful ask` CLI subcommand.
///
/// `timeout_secs` is the optional CLI `--timeout` override (U22 / 0158).
/// When `None`, backends resolve their own defaults (Local →
/// `local_model.timeout_secs` 300-class, Gemini → 120-class, other cloud 15).
/// Explicit `Some(n)` always wins for all backends.
pub fn execute_ask(opts: ExecuteAskOpts) -> Result<()> {
    let ExecuteAskOpts {
        query,
        semantic,
        limit,
        mode,
        narrative,
        backend,
        auto_index,
        timeout_secs,
        no_kg_fallback,
        auto_scan,
    } = opts;

    let layout = get_layout()?;
    let config = load_ledger_config(&layout)?;

    layout.ensure_state_dir()?;
    let storage = StorageManager::init_with_layout(&layout)?;
    let storage = prepare_ask_storage(storage, &layout, &config, auto_index)?;

    if try_early_ask_routes(&query, &storage, &layout)? {
        return Ok(());
    }

    let mut gathered = gather_impact_and_bridge(&storage, &layout, &config, &query, auto_scan)?;

    let resolved_backend = resolve_backend(&config, backend);
    validate_backend_configured(&config, backend, resolved_backend)?;

    let semantic = semantic || gathered.is_global;
    gather_semantic_and_kg(
        &mut gathered,
        &storage,
        &layout,
        &config,
        semantic,
        auto_index,
        limit,
        no_kg_fallback,
    );

    let adaptive_mode = if semantic {
        crate::local_model::context::AdaptiveMode::CodebaseFocus
    } else {
        crate::local_model::context::AdaptiveMode::ChangesFocus
    };

    // Token budget consistency
    let budget_tokens = match resolved_backend {
        Backend::Gemini => config.gemini.context_window,
        Backend::Local | Backend::OllamaCloud | Backend::OpenRouter => {
            config.local_model.context_window
        }
    };
    let char_limit = (budget_tokens as u64 * 4 * 80 / 100).max(MIN_CONTEXT_CHARS as u64) as usize;
    let truncated = gathered.latest_packet.truncate_for_context(char_limit);

    let user_prompt = build_ask_user_prompt(
        &gathered.query_string,
        gathered.is_global,
        narrative,
        &gathered.latest_packet,
        gathered.live_tree_clean,
    );

    let base_system_prompt = if gathered.is_global {
        let mut base = "You are Ledgerful, an expert software engineering assistant. You act as a codebase oracle answering architectural and implementation questions based on retrieved knowledge graph and semantic context snippets. Provide direct, technical, and accurate answers citing the retrieved snippets where relevant.".to_string();
        if gathered.relevant_chunks.is_empty() {
            base.push_str("\n\nNote: no retrieved snippets for this query.");
        }
        base
    } else {
        crate::local_model::context::get_system_prompt(&mode.to_string())
    };

    // 0158: backend-aware timeout. Prefer explicit CLI --backend for kind
    // (M6: collapsed OllamaCloud→Local must not get 300s local load budget).
    let timeout_kind =
        crate::commands::ask::AskTimeoutKind::from_backends(backend, resolved_backend);
    let effective_timeout =
        crate::commands::ask::resolve_ask_timeout(timeout_secs, timeout_kind, &config);
    let complete_override =
        crate::commands::ask::complete_timeout_override(timeout_secs, timeout_kind, &config);
    // Gemini primary always resolves via Gemini kind (120-class when omitted).
    let gemini_timeout = crate::commands::ask::resolve_ask_timeout(
        timeout_secs,
        crate::commands::ask::AskTimeoutKind::Gemini,
        &config,
    );

    // TA14: If a provider priority list is configured, try each provider
    // in order, falling back to the next on degradable errors. If all
    // providers fail, degrade to context-only output (R4).
    if !config.ask.providers.priority.is_empty() {
        let entries =
            resolve_provider_entries(&config, backend).map_err(|e| miette::miette!("{e}"))?;
        return crate::commands::ask::execute_ask_with_providers(
            &config,
            &base_system_prompt,
            &user_prompt,
            &gathered.relevant_chunks,
            timeout_secs,
            mode,
            &gathered.latest_packet,
            adaptive_mode,
            truncated,
            &entries,
        );
    }

    execute_legacy_complete(LegacyCompleteInputs {
        config: &config,
        resolved_backend,
        timeout_kind,
        effective_timeout,
        complete_override,
        gemini_timeout,
        base_system_prompt: &base_system_prompt,
        user_prompt: &user_prompt,
        relevant_chunks: &gathered.relevant_chunks,
        latest_packet: &gathered.latest_packet,
        adaptive_mode,
        truncated,
        mode,
    })
}

fn prepare_ask_storage(
    storage: StorageManager,
    layout: &Layout,
    config: &Config,
    auto_index: bool,
) -> Result<StorageManager> {
    let threshold = config.index.stale_threshold_days;
    let non_interactive = crate::index::staleness::is_non_interactive();
    if auto_index {
        let (storage, _) = crate::index::staleness::try_auto_index(storage, threshold, layout)?;
        return Ok(storage);
    }
    if non_interactive {
        // Non-interactive mode: skip auto-index prompt, just warn
        warn_if_stale(&storage, threshold);
        return Ok(storage);
    }
    let is_stale = warn_if_stale(&storage, threshold);
    if is_stale && crate::util::term::is_interactive() {
        use inquire::Confirm;
        if let Ok(true) = Confirm::new("Index is stale. Would you like to run auto-index now?")
            .with_default(true)
            .prompt()
        {
            eprintln!("Running auto-indexing...");
            let (storage, _) = crate::index::staleness::try_auto_index(storage, threshold, layout)?;
            return Ok(storage);
        }
    }
    Ok(storage)
}

/// CG-F20 → ProductDocs (0139) → CG-F31. Returns `true` when the query was
/// answered without an LLM backend.
fn try_early_ask_routes(
    query: &Option<String>,
    storage: &StorageManager,
    layout: &Layout,
) -> Result<bool> {
    // Graph-first routing (CG-F20): exact structural questions (callers,
    // callees, route ownership, symbol definitions) are answered directly
    // from the index/graph, with file+line citations, instead of being
    // handed to the LLM as just another context chunk. This runs before any
    // LLM-backend validation or bridge/context work so these queries never
    // require a configured LLM backend. The LLM is only consulted as a
    // fallback when the intent isn't recognized or the structured resolver
    // finds nothing.
    if let Some(q) = query
        && let Some(intent) = crate::commands::ask_routing::parse_intent(q)
    {
        match crate::commands::ask_routing::resolve_intent(&intent, storage.get_connection()) {
            Ok(Some(resolved)) => {
                println!(
                    "{}",
                    "Exact structural query resolved via index routing."
                        .if_supports_color(Stream::Stdout, |s| s.cyan())
                );
                println!("\n{resolved}");
                return Ok(true);
            }
            Ok(None) => {
                // 0142: SymbolDefinition → secondary FTS + honest local miss
                // (no LLM invent of "no codebase" while search can answer).
                // CallersOf / CalleesOf / ListRoutes / RouteOwner keep fall-through.
                let explanation = match intent {
                    crate::commands::ask_routing::ExactIntent::SymbolDefinition(ref t) => {
                        let hits = crate::commands::ask_routing::search_symbol_secondary(layout, t);
                        if !hits.is_empty() {
                            println!(
                                "{}",
                                crate::commands::ask_routing::LOCAL_GROUNDING_SEARCH_BANNER
                                    .if_supports_color(Stream::Stdout, |s| s.cyan())
                            );
                            println!(
                                "\n{}",
                                crate::commands::ask_routing::format_search_evidence(t, &hits)
                            );
                            return Ok(true);
                        }
                        println!(
                            "{}",
                            crate::commands::ask_routing::LOCAL_GROUNDING_MISS_BANNER
                                .if_supports_color(Stream::Stdout, |s| s.cyan())
                        );
                        println!(
                            "\n{}",
                            crate::commands::ask_routing::format_local_grounding_miss(t)
                        );
                        return Ok(true);
                    }
                    crate::commands::ask_routing::ExactIntent::CallersOf(ref t) => {
                        format!("searched for callers of `{}`", t)
                    }
                    crate::commands::ask_routing::ExactIntent::CalleesOf(ref t) => {
                        format!("searched for callees of `{}`", t)
                    }
                    crate::commands::ask_routing::ExactIntent::ListRoutes => {
                        "searched for API routes".to_string()
                    }
                    crate::commands::ask_routing::ExactIntent::RouteOwner(ref t) => {
                        format!("searched for handlers of route `{}`", t)
                    }
                };
                eprintln!(
                    "{}",
                    format!(
                        "No structural results found ({}). Falling back to semantic search...",
                        explanation
                    )
                    .if_supports_color(Stream::Stderr, |s| s.yellow())
                );
                // No indexed results for this structural query; fall through to semantic/LLM
            }
            Err(e) => {
                tracing::warn!("Exact intent routing failed: {e}; falling through to semantic");
            }
        }
    }

    // Product-docs / Daily 5 routing (0139): product-usage questions about
    // the agent default path are answered from skill Daily 5 + live clap
    // about text before any LLM backend. Wire order is load-bearing:
    // CG-F20 → ProductDocs → CG-F31 → LLM, so "session start commands"
    // is not swallowed by GenericDiscovery (operator-surface-policy.md §2).
    if let Some(q) = query
        && crate::commands::ask_routing::parse_product_docs_intent(q).is_some()
    {
        let corpus = crate::commands::ask_routing::build_command_corpus();
        let answer = crate::commands::ask_routing::build_daily5_answer(&corpus);
        println!(
            "{}",
            crate::commands::ask_routing::PRODUCT_DOCS_DAILY5_BANNER
                .if_supports_color(Stream::Stdout, |s| s.cyan())
        );
        println!("\n{answer}");
        return Ok(true);
    }

    // Command-discovery / repo-health routing (CG-F31): operator-intent
    // questions about *which CLI command* to run (e.g. "what commands show
    // repo health?") are answered directly from the live clap command
    // corpus, with the same early-exit shape as the CG-F20 block above. This
    // also runs before any LLM-backend validation, so on a successful
    // resolution the backend-selection chatter below (`Contacting LLM...`,
    // `Using Gemini...`) is never reached for this query class. Conservative
    // by construction: `parse_command_discovery_intent` returns `None` for
    // anything that isn't clearly a command-discovery question (including
    // CG-F20 structural questions and narrative/implementation questions),
    // so this falls through to existing behavior unchanged in that case.
    if let Some(q) = query
        && let Some(discovery_intent) =
            crate::commands::ask_routing::parse_command_discovery_intent(q)
    {
        let corpus = crate::commands::ask_routing::build_command_corpus();
        if let Some(answer) = crate::commands::ask_routing::build_command_discovery_answer(
            &discovery_intent,
            q,
            &corpus,
        ) {
            println!(
                "{}",
                "Command-discovery query resolved via live CLI metadata."
                    .if_supports_color(Stream::Stdout, |s| s.cyan())
            );
            println!("\n{answer}");
            return Ok(true);
        }
        // Low confidence: no corpus entry scored above zero. Fall through to
        // semantic/LLM routing rather than answering with nothing useful.
    }

    Ok(false)
}

fn validate_backend_configured(
    config: &Config,
    backend: Option<Backend>,
    resolved_backend: Backend,
) -> Result<()> {
    match resolved_backend {
        Backend::Gemini => {
            let has_gemini_key = config.gemini.api_key.is_some()
                || env::var("GEMINI_API_KEY").is_ok()
                || crate::config::model::read_env_key("GEMINI_API_KEY").is_some();

            if !has_gemini_key {
                return Err(miette::miette!(
                    "Gemini backend selected but GEMINI_API_KEY is not configured. Use --backend local or set the API key."
                ));
            }
        }
        Backend::Local | Backend::OllamaCloud | Backend::OpenRouter => {
            if !crate::local_model::client::is_configured(&config.local_model) {
                // 0073: under Forbidden, cloud-only credentials do not count as
                // configured — return the structured opt-in-bearing error so
                // MCP clients see cloud_policy_forbidden + ALLOW_CLOUD guidance.
                if crate::local_model::CloudPolicy::from_env().is_forbidden() {
                    return Err(miette::miette!(
                        "{}",
                        crate::local_model::cloud_policy_forbidden_error(
                            "local model required; cloud keys are ignored under Forbidden"
                        )
                    ));
                }
                if let Some(Backend::Gemini) = backend {
                    return Err(miette::miette!(
                        "Gemini API key missing and no local model is configured. Please configure either Gemini or a local model (Ollama/llama.cpp)."
                    ));
                } else {
                    return Err(miette::miette!(
                        "Local model backend selected but not configured. Use --backend gemini or configure a local model."
                    ));
                }
            }

            if let Some(Backend::Gemini) = backend {
                eprintln!(
                    "{}",
                    "Gemini API key missing — falling back to local model."
                        .if_supports_color(Stream::Stderr, |s| s.yellow())
                );
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::config::model::Config;

    #[test]
    fn ask_completion_options_are_bounded() {
        let options = crate::commands::ask::ask_completion_options();
        assert_eq!(options.max_tokens, 512);
        assert!(options.max_tokens < Config::default().local_model.context_window);
    }

    /// 0073 Codex R1 P2: Forbidden + cloud-only credentials must surface
    /// `cloud_policy_forbidden` (not the generic "not configured" message).
    #[test]
    #[serial_test::serial(env)]
    fn forbidden_cloud_only_returns_structured_policy_error() {
        mod env_guard {
            include!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/integration/common/env_guard.rs"
            ));
        }
        use crate::local_model::cloud_policy::{
            CLOUD_POLICY_ENV, CLOUD_POLICY_FORBIDDEN_CODE, CLOUD_POLICY_FORBIDDEN_VALUE,
            MCP_ALLOW_CLOUD_EGRESS_ENV,
        };
        use env_guard::TempEnv;

        // Isolate from ambient keys / repo .env.
        // nosemgrep: rust.lang.security.temp-dir.temp-dir
        if let Ok(tmp) = std::env::temp_dir().canonicalize() {
            let _ = std::env::set_current_dir(tmp);
        }
        let _pol = TempEnv::set(CLOUD_POLICY_ENV, CLOUD_POLICY_FORBIDDEN_VALUE);
        let _allow = TempEnv::remove(MCP_ALLOW_CLOUD_EGRESS_ENV);
        let _or = TempEnv::set("OPENROUTER_API_KEY", "sk-or-v1-test-not-real");
        let _gem = TempEnv::set("GEMINI_API_KEY", "test-gemini-key-not-real");

        let mut config = Config::default();
        config.local_model.base_url.clear();
        config.local_model.generation_url = None;
        config.local_model.ollama_cloud_url = Some("https://api.ollama.com".to_string());
        config.local_model.ollama_cloud_api_key = Some("ollama-key".to_string());
        config.local_model.ollama_cloud_model = Some("model:cloud".to_string());

        assert!(
            !crate::local_model::client::is_configured(&config.local_model),
            "cloud keys must not count as configured under Forbidden"
        );
        assert!(crate::local_model::CloudPolicy::from_env().is_forbidden());

        // Mirror the execute_ask branch: Forbidden + !is_configured → structured error.
        let err = crate::local_model::cloud_policy_forbidden_error(
            "local model required; cloud keys are ignored under Forbidden",
        );
        assert!(err.contains(CLOUD_POLICY_FORBIDDEN_CODE));
        assert!(err.contains(MCP_ALLOW_CLOUD_EGRESS_ENV));
    }
}
