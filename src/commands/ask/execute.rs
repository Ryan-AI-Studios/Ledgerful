use crate::commands::ask::{
    Backend, build_ask_user_prompt, degrade_to_context, fetch_kg_bm25, fetch_kg_neighborhood,
    gather_semantic_chunks, resolve_backend, resolve_provider_entries, run_gemini_synthesis,
};
use crate::commands::helpers::{get_layout, load_ledger_config};
use crate::gemini::modes::GeminiMode;
use crate::index::warn_if_stale;
use crate::local_model::pruner;
use crate::state::storage::StorageManager;
use miette::Result;
use owo_colors::OwoColorize;
use std::env;

const MIN_CONTEXT_CHARS: usize = 32_768;

/// Entry point for the `ledgerful ask` CLI subcommand.
///
/// `timeout_secs` is the per-request LLM timeout (U22). It is the primary
/// value the `--timeout` CLI flag (default 15) is wired into.
#[allow(clippy::too_many_arguments)]
pub fn execute_ask(
    query: Option<String>,
    semantic: bool,
    limit: usize,
    mode: GeminiMode,
    narrative: bool,
    backend: Option<Backend>,
    auto_index: bool,
    timeout_secs: u64,
    no_kg_fallback: bool,
    auto_scan: bool,
) -> Result<()> {
    let layout = get_layout()?;
    let config = load_ledger_config(&layout)?;

    layout.ensure_state_dir()?;
    let storage_path = layout.state_subdir().join("ledger.db");
    let storage = StorageManager::init(storage_path.as_std_path())?;

    // --- Staleness check ---
    let threshold = config.index.stale_threshold_days;
    let non_interactive = crate::index::staleness::is_non_interactive();
    let storage = if auto_index {
        crate::index::staleness::try_auto_index(storage, threshold)?
    } else if non_interactive {
        // Non-interactive mode: skip auto-index prompt, just warn
        warn_if_stale(&storage, threshold);
        storage
    } else {
        let is_stale = warn_if_stale(&storage, threshold);
        if is_stale && crate::util::term::is_interactive() {
            use inquire::Confirm;
            if let Ok(true) = Confirm::new("Index is stale. Would you like to run auto-index now?")
                .with_default(true)
                .prompt()
            {
                eprintln!("Running auto-indexing...");
                crate::index::staleness::try_auto_index(storage, threshold)?
            } else {
                storage
            }
        } else {
            storage
        }
    };

    // Graph-first routing (CG-F20): exact structural questions (callers,
    // callees, route ownership, symbol definitions) are answered directly
    // from the index/graph, with file+line citations, instead of being
    // handed to the LLM as just another context chunk. This runs before any
    // LLM-backend validation or bridge/context work so these queries never
    // require a configured LLM backend. The LLM is only consulted as a
    // fallback when the intent isn't recognized or the structured resolver
    // finds nothing.
    if let Some(ref q) = query
        && let Some(intent) = crate::commands::ask_routing::parse_intent(q)
    {
        match crate::commands::ask_routing::resolve_intent(&intent, storage.get_connection()) {
            Ok(Some(resolved)) => {
                println!(
                    "{}",
                    "Exact structural query resolved via index routing.".cyan()
                );
                println!("\n{resolved}");
                return Ok(());
            }
            Ok(None) => {
                let explanation = match intent {
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
                    crate::commands::ask_routing::ExactIntent::SymbolDefinition(ref t) => {
                        format!("searched for definition of `{}`", t)
                    }
                };
                eprintln!(
                    "{}",
                    format!(
                        "No structural results found ({}). Falling back to semantic search...",
                        explanation
                    )
                    .yellow()
                );
                // No indexed results for this structural query; fall through to semantic/LLM
            }
            Err(e) => {
                tracing::warn!("Exact intent routing failed: {e}; falling through to semantic");
            }
        }
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
    if let Some(ref q) = query
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
                "Command-discovery query resolved via live CLI metadata.".cyan()
            );
            println!("\n{answer}");
            return Ok(());
        }
        // Low confidence: no corpus entry scored above zero. Fall through to
        // semantic/LLM routing rather than answering with nothing useful.
    }

    let auto_scan_effective = auto_scan || config.ask.auto_scan_default;
    let (mut latest_packet, mut is_global, had_real_packet, fresh_packet) = if auto_scan_effective {
        eprintln!("{}", "Auto-scanning for fresh impact context…".cyan());
        match crate::commands::impact::compute_impact_in_memory(&storage, &config) {
            Ok(packet) => {
                let has_changes = !packet.changes.is_empty();
                if has_changes {
                    tracing::debug!(
                        "ask: auto-scan produced fresh impact packet with {} changed files",
                        packet.changes.len()
                    );
                } else {
                    tracing::debug!("ask: auto-scan found clean tree — defaulting to global mode");
                }
                (packet, !has_changes, has_changes, true)
            }
            Err(e) => {
                tracing::warn!(
                    "auto-scan failed ({e}); falling back to latest stored impact packet"
                );
                match storage.get_latest_packet()? {
                    Some(pkt) => (pkt, false, true, false),
                    None => {
                        tracing::info!(
                            "No impact report found — falling back to global knowledge retrieval mode."
                        );
                        (
                            crate::impact::packet::ImpactPacket::default(),
                            true,
                            false,
                            false,
                        )
                    }
                }
            }
        }
    } else {
        match storage.get_latest_packet()? {
            Some(pkt) => (pkt, false, true, false),
            None => {
                tracing::info!(
                    "No impact report found — falling back to global knowledge retrieval mode."
                );
                (
                    crate::impact::packet::ImpactPacket::default(),
                    true,
                    false,
                    false,
                )
            }
        }
    };

    if !is_global && latest_packet.changes.is_empty() {
        tracing::debug!("Latest impact packet is clean (no changes) — defaulting to global mode.");
        is_global = true;
    }

    let query_string = match &query {
        Some(q) => q.clone(),
        None => {
            if is_global {
                "Give me an overview of this codebase and its key components.".to_string()
            } else {
                "Analyze the current impact and risk.".to_string()
            }
        }
    };

    let mut pruned_for_intent = false;
    if crate::commands::ask::should_prune_impact(&query_string) {
        if had_real_packet && !latest_packet.changes.is_empty() {
            is_global = true;
            latest_packet = crate::impact::packet::ImpactPacket::default();
            pruned_for_intent = true;
        }
        tracing::debug!("ask: impact context pruned — query classified as GlobalConceptual");
    } else {
        match crate::retrieval::query::classify_query(&query_string) {
            crate::retrieval::query::QueryIntent::DiffTask => {
                tracing::debug!("ask: impact context included — query classified as DiffTask");
            }
            crate::retrieval::query::QueryIntent::Unknown => {
                tracing::debug!(
                    "ask: intent unknown — preserving existing impact context behavior"
                );
            }
            _ => {}
        }
    }

    if had_real_packet
        && !pruned_for_intent
        && !fresh_packet
        && let Some(reason) = crate::state::reports::warn_if_impact_stale(&layout, &config)
    {
        eprintln!(
            "{}",
            format!(
                "Warning: {reason} — using it as ask context anyway; results may not reflect the current working tree."
            )
            .yellow()
        );
    }

    // Integrate external context
    if let Some(ref q) = query
        && let Ok(bridge_records) = crate::bridge::client::query_unified(q)
    {
        for record in bridge_records {
            if let crate::bridge::model::BridgePayload::Insight {
                memory_id,
                relevance,
                content,
            } = record.payload
            {
                // 0073 / RT-A2+A3: fence + size-cap bridge insights as data
                // (they re-enter ask context via the impact packet user prompt).
                let fenced = crate::ai::fence_bridge_insight(&content);
                latest_packet
                    .ai_insights
                    .push(crate::impact::packet::AiInsight {
                        memory_id,
                        relevance,
                        content: fenced,
                    });
            }
        }
    }

    let resolved_backend = resolve_backend(&config, backend);

    // Check if the chosen/resolved backend is actually configured
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
                    "Gemini API key missing — falling back to local model.".yellow()
                );
            }
        }
    }

    let semantic = semantic || is_global;

    if semantic
        && !auto_index
        && let Some(ref cozo) = storage.cozo
        && let Ok(semantic_engine) =
            crate::semantic::SemanticDiscovery::new(config.local_model.clone(), cozo)
        && let Ok(readiness) = semantic_engine.check_readiness()
        && readiness.vector_count == 0
        && crate::util::term::is_interactive()
    {
        use inquire::Confirm;
        if let Ok(true) = Confirm::new(
            "Semantic index is empty. Would you like to run 'ledgerful index --semantic' now?",
        )
        .with_default(true)
        .prompt()
        {
            eprintln!("Running semantic indexing...");
            crate::commands::index::execute_index(crate::commands::index::IndexArgs {
                incremental: true,
                check: false,
                strict: false,
                json: false,
                analyze_graph: false,
                docs: false,
                contracts: false,
                semantic: false,
                scip: None,
                auto_scip: false,
                export_docs: false,

                doc_type: None,
                concurrency: None,
                semantic_dry_run: None,
                fast: false,
                repair_metadata: false,
                dry_run: false,
                yes: false,
            })?;
        }
    }

    if pruned_for_intent {
        eprintln!(
            "{}",
            "[Global Mode] Conceptual query — querying the full Knowledge Graph (active diff context pruned for intent)."
                .cyan()
        );
    } else if is_global {
        eprintln!(
            "{}",
            "[Global Mode] No pending changes found — querying the full Knowledge Graph for context."
                .cyan()
        );
    }

    let mut relevant_chunks = gather_semantic_chunks(
        &storage,
        &query_string,
        limit,
        &config.local_model,
        is_global,
    );

    if relevant_chunks.is_empty() {
        relevant_chunks = pruner::query_relevant_chunks(
            &query_string,
            &config.local_model,
            storage.get_connection(),
            limit,
            config.local_model.chunk_min_similarity,
            config.local_model.chunk_dedup_threshold,
        )
        .unwrap_or_else(|e| {
            tracing::warn!("Chunk retrieval failed: {e}, proceeding without chunks");
            Vec::new()
        });

        // KG Fallback logic
        if is_global
            && relevant_chunks.is_empty()
            && !no_kg_fallback
            && let Some(cozo) = &storage.cozo
            && let Some(kg_bm25_context) = fetch_kg_bm25(cozo, &query_string, limit)
        {
            eprintln!(
                "{}",
                "Note: semantic index empty — using KG text search for context".yellow()
            );
            relevant_chunks.push(pruner::RankedChunk {
                source: "Knowledge Graph (BM25)".to_string(),
                content: kg_bm25_context,
                score: 1.0,
            });
        }

        // CR7: Apply KG neighborhood to pruner fallback chunks as well.
        if is_global
            && !relevant_chunks.is_empty()
            && let Some(cozo) = &storage.cozo
        {
            let syms = relevant_chunks.iter().filter_map(|c| {
                let path = std::path::Path::new(&c.source);
                path.file_stem()?.to_str()
            });
            if let Some(kg_ctx) = fetch_kg_neighborhood(cozo, syms) {
                relevant_chunks.push(pruner::RankedChunk {
                    source: "Knowledge Graph".to_string(),
                    content: kg_ctx,
                    score: 1.0,
                });
            }
        }
    }

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
    let truncated = latest_packet.truncate_for_context(char_limit);

    let user_prompt = build_ask_user_prompt(&query_string, is_global, narrative, &latest_packet);

    let base_system_prompt = if is_global {
        let mut base = "You are Ledgerful, an expert software engineering assistant. You act as a codebase oracle answering architectural and implementation questions based on retrieved knowledge graph and semantic context snippets. Provide direct, technical, and accurate answers citing the retrieved snippets where relevant.".to_string();
        if relevant_chunks.is_empty() {
            base.push_str("\n\nNote: no project context available for this query.");
        }
        base
    } else {
        crate::local_model::context::get_system_prompt(&mode.to_string())
    };

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
            &relevant_chunks,
            timeout_secs,
            mode,
            &latest_packet,
            adaptive_mode,
            truncated,
            &entries,
        );
    }

    // Legacy path: single backend, no provider fallback chain
    match resolved_backend {
        Backend::Local | Backend::OllamaCloud | Backend::OpenRouter => {
            let max_tokens = config.local_model.context_window;

            // Phase 1: Probe local model completions endpoint for fail-fast
            let mut probe_config = config.local_model.clone();
            probe_config.timeout_secs = 5;
            if let Err(e) = crate::local_model::client::ping_completions(&probe_config) {
                if crate::local_model::client::has_cloud_fallback(&config.local_model) {
                    tracing::warn!(
                        "Local completion probe failed ({e}); cloud fallback is configured"
                    );
                } else if crate::commands::ask::render::is_degradable_error(&e.to_string()) {
                    // Unreachable local model with no cloud fallback — degrade
                    // to rendering the gathered retrieval context instead of hard-failing.
                    return degrade_to_context(&config, &relevant_chunks, &e.to_string(), || {
                        run_gemini_synthesis(
                            &config,
                            &base_system_prompt,
                            &user_prompt,
                            &relevant_chunks,
                            timeout_secs,
                            mode,
                            &latest_packet,
                            adaptive_mode,
                            truncated,
                        )
                    });
                } else {
                    return Err(miette::miette!(
                        "Local completion model probe failed ({}). Check your server or use --backend gemini.",
                        e
                    ));
                }
            }

            let messages = crate::local_model::context::assemble_context(
                &base_system_prompt,
                &user_prompt,
                &relevant_chunks,
                max_tokens,
                adaptive_mode,
            );

            // Show progress indicator before LLM call with backend selection
            eprintln!("Using local/cloud model...");
            eprintln!("Contacting LLM...");

            match crate::local_model::client::complete_with_hard_deadline(
                &config.local_model,
                &messages,
                &crate::commands::ask::ask_completion_options(),
                Some(timeout_secs),
            ) {
                Ok(response) => {
                    println!("\n{}", "Local Model Response:".bold().green());
                    println!("{response}");
                    Ok(())
                }
                Err(e) => {
                    let err_str = crate::commands::ask::sanitize_error_for_logging(&e.to_string());
                    if crate::commands::ask::render::is_degradable_error(&e.to_string()) {
                        // Transport-level failure during synthesis — degrade
                        // to context render instead of hard-failing.
                        return degrade_to_context(&config, &relevant_chunks, &err_str, || {
                            run_gemini_synthesis(
                                &config,
                                &base_system_prompt,
                                &user_prompt,
                                &relevant_chunks,
                                timeout_secs,
                                mode,
                                &latest_packet,
                                adaptive_mode,
                                truncated,
                            )
                        });
                    }
                    eprintln!("{}", err_str.red());
                    if e.to_string().contains("401") {
                        eprintln!(
                            "{}",
                            "Hint: Check your OLLAMA_CLOUD_API_KEY or ollama_key in config.toml"
                                .yellow()
                        );
                    }
                    if e.to_string().contains("api.ollama.com") {
                        eprintln!("{}", "Hint: Use ollama_cloud_url = \"https://ollama.com/api\" (native) or \"https://ollama.com\" (OpenAI-compatible)".yellow());
                    }
                    Err(miette::miette!("Local model failed: {e}"))
                }
            }
        }
        Backend::Gemini => run_gemini_synthesis(
            &config,
            &base_system_prompt,
            &user_prompt,
            &relevant_chunks,
            timeout_secs,
            mode,
            &latest_packet,
            adaptive_mode,
            truncated,
        ),
    }
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
}
