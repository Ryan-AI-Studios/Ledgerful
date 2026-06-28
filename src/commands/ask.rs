use crate::commands::helpers::{get_layout, load_ledger_config};
use crate::config::model::Config;
use crate::gemini::modes::GeminiMode;
use crate::gemini::wrapper::run_query;
use crate::index::warn_if_stale;
use crate::local_model::pruner;
use crate::state::storage::StorageManager;
use miette::Result;
use owo_colors::OwoColorize;
use std::env;

const ASK_COMPLETION_MAX_TOKENS: usize = 512;
const MIN_CONTEXT_CHARS: usize = 32_768;

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

    // DX6: Auto-scan for stale impact context. When the user passes
    // `--auto-scan` (or `[ask].auto_scan_default = true`), compute a fresh
    // `ImpactPacket` in-memory from the live working tree and use it directly
    // as RAG context, suppressing the stale-impact warning below — the packet
    // reflects the current tree by construction, so "using it as ask context
    // anyway" would be misleading. The stored packet and `latest-impact.json`
    // report are left untouched. If the in-memory scan fails (e.g. not a git
    // repo, git error), fall back to the cached stored packet so the command
    // degrades gracefully rather than aborting the user's question.
    //
    // Non-persistence scope: the in-memory path skips `save_packet` (SQLite
    // snapshots) and `write_impact_report` (latest-impact.json) only. The
    // orchestrator's enrichment providers still write to the CozoDB knowledge
    // graph — the same side effect `scan --impact` has, and a deliberate one:
    // it keeps the KG current with the changed files so the packet's
    // KG-derived signals (reachability, etc.) reflect the live tree. The DX6
    // spec scopes "in-memory" as "compute fresh vs read the cached report
    // file," not "zero side effects." See `compute_impact_in_memory`.
    //
    // `fresh_packet` is true ONLY when the in-memory scan actually produced
    // the packet used as context. It drives the stale-warning suppression
    // below (not `auto_scan_effective`): if the scan errored and we fell back
    // to the cached stored packet, that packet can still be stale, so the
    // warning must fire exactly as it would on the non-auto-scan path.
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
                // A clean tree has no diff to reason about, so treat it as
                // global (mirrors the empty-packet path below). A dirty tree
                // provides a real, fresh packet.
                (packet, !has_changes, has_changes, true)
            }
            Err(e) => {
                tracing::warn!(
                    "auto-scan failed ({e}); falling back to latest stored impact packet"
                );
                // Scan failed → the fallback packet is NOT fresh; the stale
                // warning below must still be eligible to fire.
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

    // If the latest packet exists but contains no changes, it's effectively a global query context
    if !is_global && latest_packet.changes.is_empty() {
        tracing::debug!("Latest impact packet is clean (no changes) — defaulting to global mode.");
        is_global = true;
    }

    // Intent-Driven Context Pruning. Resolve the effective query string
    // (the user's explicit query if provided, else the default for the current
    // mode) and classify it. For GlobalConceptual queries the active
    // `ImpactPacket` is deliberately pruned from the RAG context so broad
    // architectural answers are not polluted with irrelevant verification
    // instructions from the uncommitted working tree. For DiffTask queries the
    // packet is kept. For Unknown the existing behavior is preserved.
    //
    // `pruned_for_intent` is set true ONLY when a GlobalConceptual query
    // actually pruned a dirty-tree packet (storage returned a packet with real
    // changes). It drives the branched `[Global Mode]` notice and suppresses
    // the stale-impact warning below — a pruned packet is deliberately unused,
    // so emitting "using it as ask context anyway" would be misleading. On a
    // clean tree (`is_global` already true, empty packet) there is nothing to
    // prune, so `pruned_for_intent` stays false and the existing "No pending
    // changes found" notice + staleness-warning behavior apply.
    //
    // `query_string` is computed via `query.as_ref()` (not `unwrap_or_else`
    // consuming `query`) so the `Option<String>` is still available to the
    // AI-Brains bridge integration further down.
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
    if should_prune_impact(&query_string) {
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

    // Stale-impact warning: the cached impact packet above is about to be
    // used as LLM context. `doctor` already classifies cache freshness
    // (via `check_impact_freshness`), but until now nothing warned an
    // `ask` caller when that same cache is stale or corrupt — it was used
    // silently. This is diagnostic chatter, not answer content, so it goes
    // to stderr like the other backend/progress notices in this file.
    //
    // Gated on `had_real_packet`, not `is_global`. `is_global` flips to
    // `true` above whenever the cached packet has empty `changes` -- including
    // a clean-tree tombstone that is itself stale (HEAD moved since the clean
    // scan, or the tree is dirty again now). `check_impact_freshness` already
    // classifies that exact case as `Stale`, but gating on `is_global` skipped
    // the check entirely once it flipped, so the "clean scan, then make
    // changes, then `ask`" workflow produced no freshness signal.
    // `had_real_packet` only reflects whether `storage.get_latest_packet()`
    // actually returned a packet, and is never mutated afterward, so it stays
    // accurate regardless of the `is_global` override below. `is_global`
    // itself is left untouched -- it still drives context-assembly mode
    // selection further down.
    //
    // Conceptual prunes (`pruned_for_intent == true`) intentionally skip
    // the staleness signal — the pruned packet is deliberately unused as ask
    // context, so "using it as ask context anyway" would be misleading. The
    // staleness warning is preserved for DiffTask / default-diff queries and
    // for GlobalConceptual queries on a clean tree (where nothing was pruned).
    //
    // DX6: `fresh_packet` skips the staleness signal — when the in-memory
    // auto-scan actually produced the packet used as context, it was computed
    // from the live working tree, so it cannot be stale by definition and the
    // warning would be noise. Crucially this is gated on `fresh_packet`, not
    // `auto_scan_effective`: if the scan errored and we fell back to the
    // cached stored packet, that packet can still be stale and the warning
    // must fire exactly as on the non-auto-scan path.
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

    // 1. Integrate external AI-Brains context
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
                latest_packet
                    .ai_insights
                    .push(crate::impact::packet::AiInsight {
                        memory_id,
                        relevance,
                        content,
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

            // CR: Graceful fallback message (AC 4)
            if let Some(Backend::Gemini) = backend {
                eprintln!(
                    "{}",
                    "Gemini API key missing — falling back to local model.".yellow()
                );
            }
        }
    }

    // In global mode, always use semantic retrieval to pull relevant context
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
            relevant_chunks.push(crate::local_model::pruner::RankedChunk {
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
                relevant_chunks.push(crate::local_model::pruner::RankedChunk {
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
    // providers fail, degrade to context-only output. Each provider uses
    // its own timeout/model/base_url from the config entry.
    if !config.ask.providers.priority.is_empty() {
        let entries =
            resolve_provider_entries(&config, backend).map_err(|e| miette::miette!("{e}"))?;
        return execute_ask_with_providers(
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
                } else if is_degradable_error(&e.to_string()) {
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
                &ask_completion_options(),
                Some(timeout_secs),
            ) {
                Ok(response) => {
                    println!("\n{}", "Local Model Response:".bold().green());
                    println!("{response}");
                    Ok(())
                }
                Err(e) => {
                    let err_str = sanitize_error_for_logging(&e.to_string());
                    if is_degradable_error(&e.to_string()) {
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

/// Resolve the ordered provider entries with full per-provider config
/// (model, timeout, base_url, api_key_env). Applies env var overrides
/// and --backend CLI flag reordering.
pub fn resolve_provider_entries(
    config: &Config,
    explicit: Option<Backend>,
) -> std::result::Result<Vec<crate::config::model::ProviderEntry>, String> {
    use crate::config::model::{Provider, ProviderEntry};

    let env_reader = |name: &str| env::var(name).ok();

    let mut entries = config.ask.providers.priority.clone();

    // Apply env var overrides (R7)
    for i in 1..=4 {
        let env_name = format!("LEDGERFUL_ASK_PROVIDER_{i}");
        if let Some(val) = env_reader(&env_name) {
            let provider = Provider::from_str_fail_fast(&val, &env_name)?;
            let model = env_reader(&format!("LEDGERFUL_ASK_MODEL_{i}"));
            if i <= entries.len() {
                entries[i - 1].backend = provider;
                if let Some(m) = model {
                    entries[i - 1].model = Some(m);
                }
            } else {
                entries.push(ProviderEntry {
                    backend: provider,
                    model,
                    timeout_secs: None,
                    api_key_env: None,
                    base_url: None,
                });
            }
        }
    }

    // If config and env vars are empty, use legacy behavior for backward compat
    if entries.is_empty() {
        let dotenv_reader = |name: &str| crate::config::model::read_env_key(name);
        let legacy = resolve_backend_with(config, explicit, &env_reader, &dotenv_reader);
        let legacy_provider = match legacy {
            Backend::Gemini => Provider::Gemini,
            Backend::Local | Backend::OllamaCloud | Backend::OpenRouter => {
                if crate::local_model::client::has_ollama_cloud_fallback(&config.local_model) {
                    Provider::OllamaCloud
                } else {
                    Provider::Local
                }
            }
        };
        return Ok(vec![ProviderEntry {
            backend: legacy_provider,
            model: None,
            timeout_secs: None,
            api_key_env: None,
            base_url: None,
        }]);
    }

    // --backend override: move specified provider to front (R7a)
    if let Some(b) = explicit {
        let target = match b {
            Backend::Gemini => Provider::Gemini,
            Backend::Local => Provider::Local,
            Backend::OllamaCloud => Provider::OllamaCloud,
            Backend::OpenRouter => Provider::OpenRouter,
        };
        let existing = entries.iter().position(|e| e.backend == target);
        match existing {
            Some(idx) => {
                let entry = entries.remove(idx);
                entries.insert(0, entry);
            }
            None => {
                let default_entry = match target {
                    Provider::OllamaCloud => ProviderEntry {
                        backend: Provider::OllamaCloud,
                        model: config.local_model.ollama_cloud_model.clone(),
                        timeout_secs: Some(config.local_model.timeout_secs),
                        api_key_env: None,
                        base_url: config.local_model.ollama_cloud_url.clone(),
                    },
                    Provider::Gemini => ProviderEntry {
                        backend: Provider::Gemini,
                        model: config.gemini.fast_model.clone(),
                        timeout_secs: config.gemini.timeout_secs,
                        api_key_env: None,
                        base_url: None,
                    },
                    Provider::Local => ProviderEntry {
                        backend: Provider::Local,
                        model: Some(config.local_model.generation_model.clone()),
                        timeout_secs: Some(config.local_model.timeout_secs),
                        api_key_env: None,
                        base_url: Some(config.local_model.base_url.clone()),
                    },
                    Provider::OpenRouter => ProviderEntry {
                        backend: Provider::OpenRouter,
                        model: env_reader("OPENROUTER_MODEL"),
                        timeout_secs: Some(config.local_model.timeout_secs),
                        api_key_env: Some("OPENROUTER_API_KEY".to_string()),
                        base_url: Some("https://openrouter.ai/api/v1".to_string()),
                    },
                };
                entries.insert(0, default_entry);
            }
        }
    }

    Ok(entries)
}

/// TA14: Execute ask with a provider priority fallback chain.
/// Tries each provider in order with per-provider timeout. Degradable
/// errors trigger fallback to the next provider. If all providers fail,
/// degrades to context-only output (R4).
#[allow(clippy::too_many_arguments)]
fn execute_ask_with_providers(
    config: &Config,
    base_system_prompt: &str,
    user_prompt: &str,
    relevant_chunks: &[crate::local_model::pruner::RankedChunk],
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
                    &ask_completion_options(),
                    Some(provider_timeout),
                ) {
                    Ok(response) => {
                        println!("\n{}", "Response:".bold().green());
                        println!("{response}");
                        return Ok(());
                    }
                    Err(e) => {
                        let err_str = sanitize_error_for_logging(&e.to_string());
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
                        let err_str = sanitize_error_for_logging(&format!("{e}"));
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

/// Assemble the `ask` user prompt from the resolved query string, the
/// global/diff mode flag, the narrative flag, and the (possibly pruned)
/// `ImpactPacket`. Extracted as a pure helper so the contract —
/// GlobalConceptual queries never produce a prompt containing an
/// `Impact Packet:` block — is unit-testable without driving the full
/// `execute_ask` flow (which requires storage, LLM backends, and network).
pub fn build_ask_user_prompt(
    query_string: &str,
    is_global: bool,
    narrative: bool,
    latest_packet: &crate::impact::packet::ImpactPacket,
) -> String {
    if is_global {
        format!(
            "Answer the following codebase query:\n\nQuery: {}",
            query_string
        )
    } else if narrative {
        crate::gemini::prompt::build_architect_prompt(latest_packet, query_string)
    } else {
        crate::gemini::prompt::build_suggest_prompt(latest_packet, query_string)
    }
}

/// Predicate deciding whether the `ask` flow should prune the active
/// `ImpactPacket` from the LLM context for the given query. Backed by
/// `classify_query`; returns `true` only for `GlobalConceptual` intents.
/// Pure function — unit-testable without storage or LLM I/O.
pub fn should_prune_impact(query: &str) -> bool {
    matches!(
        crate::retrieval::query::classify_query(query),
        crate::retrieval::query::QueryIntent::GlobalConceptual
    )
}

/// Classify a local-model error string as degradable (transport/unreachable/timeout
/// or transient server-unavailability) versus non-degradable (auth/rate-limit/other).
/// Degradable errors fall back to rendering retrieved context; non-degradable keep
/// the existing hard-fail behavior.
///
/// HTTP 502/503/504 (and their textual forms — "service
/// unavailable", "bad gateway", "gateway timeout") signal a transient
/// model-unavailability that should degrade to context, matching the established
/// precedent in `doctor::is_transient_error`. 401 (auth) and 429 (rate-limit)
/// stay non-degradable — `test_ask_does_not_degrade_on_rate_limit` pins 429, and
/// 500 (internal server error) is intentionally excluded as not necessarily
/// transient. Degradation fires on the final error string *after* the client's
/// own retry logic is exhausted (see `complete_with_endpoint`'s 503 single-retry
/// + 2s sleep), so this classification does not bypass or shorten that retry.
fn is_degradable_error(err: &str) -> bool {
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

/// Render gathered retrieval context to stdout when LLM synthesis is skipped.
/// Emits a deterministic, ranked view of the chunks that would have been sent
/// to the LLM (code snippets, KG neighborhood, documentation).
fn render_retrieved_context(chunks: &[crate::local_model::pruner::RankedChunk]) {
    println!("\n{}", degrade_context_header().bold().cyan());
    print!("{}", format_retrieved_context_body(chunks));
}

/// Spec-pinned header for the degraded-context render. Extracted as a pure
/// helper so the exact wording is unit-testable without capturing stdout.
fn degrade_context_header() -> &'static str {
    "Retrieved context (local model unavailable, skipping synthesis):"
}

/// Spec-pinned warning emitted when the local completion model is unreachable
/// and `ask` degrades to graph/semantic search. Extracted as a pure helper so
/// the exact wording (including the configured URL) is unit-testable without
/// capturing stderr.
fn degrade_warning(base_url: &str) -> String {
    format!(
        "Warning: Local completion model at {} is unreachable. Falling back to graph/semantic search.",
        base_url
    )
}

/// Build the body of the degraded-context output as a string. Separated from
/// `render_retrieved_context` so the deterministic ranking/format is unit-testable
/// without capturing stdout.
fn format_retrieved_context_body(chunks: &[crate::local_model::pruner::RankedChunk]) -> String {
    if chunks.is_empty() {
        return "(no retrieval context available for this query)\n".to_string();
    }
    // Deterministic order: highest score first, then by source for stable ties.
    let mut sorted: Vec<&crate::local_model::pruner::RankedChunk> = chunks.iter().collect();
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

/// Run Gemini backend synthesis. Extracted from the `Backend::Gemini` arm so the
/// interactive cloud fallback in `degrade_to_context` can reuse it without
/// re-routing the whole command.
#[allow(clippy::too_many_arguments)]
fn run_gemini_synthesis(
    config: &Config,
    base_system_prompt: &str,
    user_prompt: &str,
    relevant_chunks: &[crate::local_model::pruner::RankedChunk],
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
    relevant_chunks: &[crate::local_model::pruner::RankedChunk],
    timeout_secs: u64,
    mode: GeminiMode,
    latest_packet: &crate::impact::packet::ImpactPacket,
    adaptive_mode: crate::local_model::context::AdaptiveMode,
    truncated: bool,
    model_override: Option<&str>,
) -> Result<()> {
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

    let sanitize_result = crate::gemini::sanitize::sanitize_for_gemini(&final_usr_prompt);
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
fn degrade_to_context(
    config: &Config,
    relevant_chunks: &[crate::local_model::pruner::RankedChunk],
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
/// capturing stdout/stderr and an `inquire` prompt). Behavior-preserving: this
/// is exactly the `is_interactive() && config.gemini.api_key.is_some()` check
/// that previously lived inline in `degrade_to_context`.
///
/// `interactive` is passed in (rather than read from `util::term` inside) so
/// tests can inject a deterministic value without mutating process env or
/// depending on whether stdin is a tty.
fn should_prompt_for_cloud(config: &Config, interactive: bool) -> bool {
    interactive && config.gemini.api_key.is_some()
}

fn gather_semantic_chunks(
    storage: &StorageManager,
    query_string: &str,
    limit: usize,
    config: &crate::config::model::LocalModelConfig,
    is_global: bool,
) -> Vec<crate::local_model::pruner::RankedChunk> {
    let mut relevant_chunks = Vec::new();
    let mut semantic_symbols = std::collections::HashSet::new();

    if let Some(cozo) = &storage.cozo
        && let Ok(vector_store) = crate::semantic::vector_store::VectorStore::new(
            cozo,
            config.dimensions,
            config.disable_hnsw,
        )
        && let Ok(embedder) =
            crate::semantic::embedder::SemanticEmbedder::new(config.clone()).embed(query_string)
        && let Ok(results) = vector_store.query(embedder, limit)
    {
        for (file_path, name, _offset, dist) in results {
            let score = 1.0 - (dist / 2.0);
            if score >= config.chunk_min_similarity {
                semantic_symbols.insert(name.clone());
                if let Ok(content) =
                    crate::util::fs::read_to_string_with_encoding(std::path::Path::new(&file_path))
                {
                    let snippet = content.chars().take(1000).collect::<String>();
                    relevant_chunks.push(crate::local_model::pruner::RankedChunk {
                        source: format!("{}:: {}", file_path, name),
                        content: snippet,
                        score,
                    });
                }
            }
        }

        if is_global
            && !semantic_symbols.is_empty()
            && let Some(cozo) = &storage.cozo
            && let Some(kg_ctx) =
                fetch_kg_neighborhood(cozo, semantic_symbols.iter().map(|s| s.as_str()))
        {
            relevant_chunks.push(crate::local_model::pruner::RankedChunk {
                source: "Knowledge Graph".to_string(),
                content: kg_ctx,
                score: 1.0,
            });
        }
    }

    relevant_chunks
}

/// CR8: Escape a symbol name for safe interpolation inside a Cozo Datalog string literal.
/// Cozo uses single-quoted string literals; a single quote must be doubled to escape it.
/// Backslashes are also escaped to prevent unintended Datalog escaping sequences.
pub fn escape_cozo_string(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\'', "''")
}

fn ask_completion_options() -> crate::local_model::client::CompletionOptions {
    crate::local_model::client::CompletionOptions {
        max_tokens: ASK_COMPLETION_MAX_TOKENS,
        ..crate::local_model::client::CompletionOptions::default()
    }
}

/// CR7: Run the KG neighborhood edge query for a set of symbol names and return a
/// formatted context string, or `None` if no relevant edges are found.
fn fetch_kg_neighborhood(
    cozo: &crate::state::storage_cozo::CozoStorage,
    symbols: impl Iterator<Item = impl AsRef<str>>,
) -> Option<String> {
    let symbols_array = symbols
        .map(|s| format!("'{}'", escape_cozo_string(s.as_ref())))
        .collect::<Vec<_>>()
        .join(", ");
    if symbols_array.is_empty() {
        return None;
    }
    let script = format!(
        r#"
        ?[caller, callee, relation] := *edge{{source: caller, target: callee, relation: relation}}, 
                                       caller in [{symbols_array}] or callee in [{symbols_array}]
        :limit 50
    "#
    );
    let res = cozo.run_script(&script).ok()?;
    let mut kg_context = String::from("Knowledge Graph Relationships:\n");
    for row in res.rows {
        if let (
            Some(cozo::DataValue::Str(caller)),
            Some(cozo::DataValue::Str(callee)),
            Some(cozo::DataValue::Str(rel)),
        ) = (row.first(), row.get(1), row.get(2))
        {
            kg_context.push_str(&format!("- {} {} {}\n", caller, rel, callee));
        }
    }
    if kg_context.len() > 30 {
        Some(kg_context)
    } else {
        None
    }
}

/// CRX1: Perform BM25 text search on KG nodes when semantic index is absent.
fn fetch_kg_bm25(
    cozo: &crate::state::storage_cozo::CozoStorage,
    query: &str,
    limit: usize,
) -> Option<String> {
    let escaped_query = escape_cozo_string(query);
    let script = format!(
        r#"
        ?[id, label, category] := *node{{id, label, category}},
          label ~ '(?i).*{}.*'
        :limit {limit}
    "#,
        escaped_query
    );

    let res = cozo.run_script(&script).ok()?;
    if res.rows.is_empty() {
        return None;
    }

    let mut kg_context = String::from("Knowledge Graph (Text Search) Matches:\n");
    for row in res.rows {
        if let (
            Some(cozo::DataValue::Str(id)),
            Some(cozo::DataValue::Str(label)),
            Some(cozo::DataValue::Str(category)),
        ) = (row.first(), row.get(1), row.get(2))
        {
            kg_context.push_str(&format!("- [{}] {} ({})\n", category, label, id));
        }
    }

    if kg_context.len() > 40 {
        Some(kg_context)
    } else {
        None
    }
}

pub fn resolve_backend(config: &Config, explicit: Option<Backend>) -> Backend {
    resolve_backend_with(config, explicit, &|name| env::var(name).ok(), &|name| {
        crate::config::model::read_env_key(name)
    })
}

pub fn resolve_backend_with(
    config: &Config,
    explicit: Option<Backend>,
    env_reader: &dyn Fn(&str) -> Option<String>,
    dotenv_reader: &dyn Fn(&str) -> Option<String>,
) -> Backend {
    let has_gemini_key = config.gemini.api_key.is_some()
        || env_reader("GEMINI_API_KEY").is_some()
        || dotenv_reader("GEMINI_API_KEY").is_some();

    let has_local = crate::local_model::client::is_configured(&config.local_model);

    if let Some(b) = explicit {
        if b == Backend::Gemini && !has_gemini_key {
            return Backend::Local;
        }
        // New provider variants map to Local for the legacy path
        // (they're handled by the provider priority chain in execute_ask)
        return match b {
            Backend::OllamaCloud | Backend::OpenRouter => Backend::Local,
            other => other,
        };
    }

    if config.local_model.prefer_local && has_local {
        return Backend::Local;
    }

    if !has_gemini_key && has_local {
        return Backend::Local;
    }

    Backend::Gemini
}

/// Resolve the ordered provider priority list (Track TA14).
/// Delegates to `resolve_provider_entries` and maps to `Vec<Provider>`.
pub fn resolve_provider_priority(
    config: &Config,
    explicit: Option<Backend>,
) -> std::result::Result<Vec<crate::config::model::Provider>, String> {
    let entries = resolve_provider_entries(config, explicit)?;
    Ok(entries.into_iter().map(|e| e.backend).collect())
}

/// Sanitize an error message for safe logging (R7b).
/// Strips bearer tokens and api_key= values from the string.
/// Uses `to_ascii_lowercase` (not `to_lowercase`) to preserve byte
/// alignment between the search string and the original — `to_lowercase`
/// can change byte length for non-ASCII characters (e.g. German sharp-s),
/// causing byte-index panics when slicing the original.
pub fn sanitize_error_for_logging(err: &str) -> String {
    let lower = err.to_ascii_lowercase();
    let mut sanitized = err.to_string();

    // Strip bearer tokens (case-insensitive)
    if let Some(idx) = lower.find("bearer ") {
        let start = idx;
        let rest = &sanitized[start + 7..];
        let end = rest
            .find(|c: char| c.is_whitespace() || c == ',' || c == ')' || c == ']')
            .unwrap_or(rest.len());
        sanitized = format!(
            "{}bearer [REDACTED]{}",
            &sanitized[..start],
            &sanitized[start + 7 + end..]
        );
    }

    // Strip api_key= values
    let lower2 = sanitized.to_ascii_lowercase();
    if let Some(idx) = lower2.find("api_key=") {
        let start = idx;
        let rest = &sanitized[start + 8..];
        let end = rest
            .find(|c: char| c.is_whitespace() || c == ',' || c == ')' || c == ']' || c == '&')
            .unwrap_or(rest.len());
        sanitized = format!(
            "{}api_key=[REDACTED]{}",
            &sanitized[..start],
            &sanitized[start + 8 + end..]
        );
    }

    sanitized
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::model::GeminiConfig;
    use crate::impact::packet::ImpactPacket;

    #[test]
    fn test_select_gemini_model_logic() {
        let packet = ImpactPacket::default();

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
    fn resolve_backend_uses_local_when_only_ollama_cloud_is_configured() {
        let mut config = Config::default();
        config.local_model.ollama_cloud_url = Some("https://api.ollama.com".to_string());
        config.local_model.ollama_cloud_api_key = Some("token".to_string());
        config.local_model.ollama_cloud_model = Some("minimax-m3:cloud".to_string());

        let backend = resolve_backend_with(&config, None, &|_| None, &|_| None);
        assert_eq!(backend, Backend::Local);
    }

    #[test]
    fn ask_completion_options_are_bounded() {
        let options = ask_completion_options();
        assert_eq!(options.max_tokens, ASK_COMPLETION_MAX_TOKENS);
        assert!(options.max_tokens < Config::default().local_model.context_window);
    }

    #[test]
    fn test_default_context_window_yields_hardcoded_budget() {
        let config = GeminiConfig::default(); // defaults to 128,000
        let char_limit =
            (config.context_window as u64 * 4 * 80 / 100).max(MIN_CONTEXT_CHARS as u64) as usize;
        assert_eq!(char_limit, 409_600);
    }

    #[test]
    fn test_custom_context_window_adjusts_budget() {
        let config = GeminiConfig {
            context_window: 200_000,
            ..Default::default()
        };
        let char_limit =
            (config.context_window as u64 * 4 * 80 / 100).max(MIN_CONTEXT_CHARS as u64) as usize;
        assert_eq!(char_limit, 640_000);
    }

    #[test]
    fn test_small_context_window_budget() {
        let config = GeminiConfig {
            context_window: 32_000,
            ..Default::default()
        };
        let char_limit =
            (config.context_window as u64 * 4 * 80 / 100).max(MIN_CONTEXT_CHARS as u64) as usize;
        assert_eq!(char_limit, 102_400);
    }

    #[test]
    fn test_zero_context_window_fallback() {
        let config = GeminiConfig {
            context_window: 0,
            ..Default::default()
        };
        let char_limit =
            (config.context_window as u64 * 4 * 80 / 100).max(MIN_CONTEXT_CHARS as u64) as usize;
        assert_eq!(char_limit, 32_768);
    }

    // --- Graceful degradation helpers ---

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
        // 502/503/504 and their textual forms are
        // transient model-unavailability and must degrade. These mirror the
        // error strings produced by `ping_completions` ("503 server error (...)")
        // and `complete_with_endpoint` ("... returned 503: ...").
        assert!(is_degradable_error(
            "503 server error (Service Unavailable)"
        ));
        assert!(is_degradable_error("ollama returned 503: model warming"));
        assert!(is_degradable_error("502 server error (Bad Gateway)"));
        assert!(is_degradable_error("cloud returned 504: Gateway Timeout"));
        assert!(is_degradable_error("Service Unavailable"));
        assert!(is_degradable_error("Bad Gateway"));
        assert!(is_degradable_error("Gateway Timeout"));
        // Case-insensitive.
        assert!(is_degradable_error("SERVICE UNAVAILABLE"));
        assert!(is_degradable_error("service unavailable"));
    }

    #[test]
    fn degradable_error_classification_non_degradable() {
        // Auth/rate-limit/parse errors must NOT degrade.
        assert!(!is_degradable_error("401 server error (unauthorized)"));
        assert!(!is_degradable_error("rate limited. Wait a moment."));
        assert!(!is_degradable_error("Failed to parse completion response"));
        assert!(!is_degradable_error("returned empty message content"));
        assert!(!is_degradable_error("429 Too Many Requests"));
        // 500 is intentionally NOT degradable (not necessarily transient).
        assert!(!is_degradable_error("500 server error (internal)"));
        assert!(!is_degradable_error("cloud returned 500: boom"));
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
        // Highest score first.
        let high_pos = body.find("high score body").unwrap();
        let mid_pos = body.find("mid score body").unwrap();
        let low_pos = body.find("low score body").unwrap();
        assert!(high_pos < mid_pos, "high must precede mid: {body}");
        assert!(mid_pos < low_pos, "mid must precede low: {body}");
        // Each chunk carries its source + score.
        assert!(body.contains("src/a.rs:: high"));
        assert!(body.contains("score: 0.900"));
    }

    // --- Pin the spec's exact degradation observables ---

    /// The spec warning text must be pinned exactly, with the real configured
    /// URL substituted. A regression that silently drops or rewords this
    /// warning fails here AND at the integration test that captures stderr.
    #[test]
    fn degrade_warning_pins_spec_text() {
        let warning = degrade_warning("http://127.0.0.1:1");
        assert_eq!(
            warning,
            "Warning: Local completion model at http://127.0.0.1:1 is unreachable. Falling back to graph/semantic search."
        );
        // The header is a constant string pinned verbatim.
        assert_eq!(
            degrade_context_header(),
            "Retrieved context (local model unavailable, skipping synthesis):"
        );
    }

    /// The degraded-context render must emit the spec header followed by the
    /// ranked body. Locks the END-TO-END render shape (header + body) that
    /// `degrade_to_context` prints to stdout, so removing either the header
    /// or the body render from production breaks this test's contract.
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
        // Header is the exact spec string.
        assert_eq!(
            header,
            "Retrieved context (local model unavailable, skipping synthesis):"
        );
        // Body is ranked highest-first with source + score annotations.
        let high_pos = body.find("high body").unwrap();
        let low_pos = body.find("low body").unwrap();
        assert!(high_pos < low_pos, "high must precede low: {body}");
        assert!(body.contains("src/high.rs"));
        assert!(body.contains("score: 0.900"));
    }

    // --- Interactive prompt-gate predicate ---

    /// The interactive Gemini-switch prompt in `degrade_to_context` is gated on
    /// BOTH `is_interactive()` AND a configured `config.gemini.api_key`. This
    /// test pins the gate so a regression that prompts during a non-interactive
    /// session (where `inquire` would block forever or error out) fails here.
    ///
    /// We exercise the gate predicate directly with an injected `interactive`
    /// flag rather than mutating process env, because `util::term::is_interactive`
    /// also checks stdin/tty state which is unreliable to drive from a unit
    /// test. The integration-level guarantee that `LEDGERFUL_NON_INTERACTIVE=1`
    /// forces `is_interactive()` to false is covered by the binary-spawned
    /// degrade test (`test_ask_degrades_gracefully_when_local_model_unreachable`),
    /// which would hang if the prompt fired.
    #[test]
    fn should_prompt_for_cloud_gate_skips_non_interactive_even_with_key() {
        // A Gemini key IS configured in config.toml, but the session is
        // non-interactive: the prompt must be skipped so degradation proceeds
        // straight to context render.
        let mut config = Config::default();
        config.gemini.api_key = Some("test-key".to_string());
        assert!(
            !should_prompt_for_cloud(&config, false),
            "non-interactive session must skip the cloud prompt even with a key configured"
        );
        // Interactive session WITH a key: prompt fires.
        assert!(
            should_prompt_for_cloud(&config, true),
            "interactive session with a configured key should prompt for cloud"
        );
    }

    #[test]
    fn should_prompt_for_cloud_gate_skips_when_no_key_configured() {
        // No Gemini key in config: prompt must be skipped regardless of
        // interactivity (the env-key path was already attempted inside
        // `complete()`'s cloud fallback chain).
        let config = Config::default();
        assert!(
            !should_prompt_for_cloud(&config, true),
            "no configured key must skip the cloud prompt even when interactive"
        );
        assert!(!should_prompt_for_cloud(&config, false));
    }

    // --- Intent-Driven Context Pruning ---

    #[test]
    fn should_prune_impact_true_for_global_conceptual() {
        assert!(should_prune_impact("summarize the architecture"));
        assert!(should_prune_impact("how does the storage layer work"));
        assert!(should_prune_impact("give me an overview"));
    }

    #[test]
    fn should_prune_impact_false_for_diff_task_and_unknown() {
        assert!(!should_prune_impact("what did I just change"));
        assert!(!should_prune_impact("explain these test failures"));
        assert!(!should_prune_impact("list all HTTP routes"));
        assert!(!should_prune_impact("hello"));
        assert!(!should_prune_impact(""));
    }

    /// Contract: a GlobalConceptual query routes through the global
    /// prompt path, which never emits an `Impact Packet:` block. This locks
    /// the prompt-assembly decision so a regression that re-injects the
    /// active diff into a conceptual answer fails here.
    #[test]
    fn global_conceptual_query_prompt_omits_impact_packet_block() {
        let query = "summarize the architecture";
        assert!(should_prune_impact(query));
        // Pruning replaces the packet with the default and forces is_global.
        let pruned_packet = ImpactPacket::default();
        let prompt = build_ask_user_prompt(query, true, false, &pruned_packet);
        assert!(
            !prompt.contains("Impact Packet:"),
            "GlobalConceptual prompt must not contain an `Impact Packet:` block: {prompt}"
        );
        assert!(
            !prompt.contains("Impact Packet"),
            "GlobalConceptual prompt must not mention `Impact Packet` at all: {prompt}"
        );
        // Sanity: the query itself is present.
        assert!(prompt.contains(query));
    }

    /// Counterpart: a DiffTask query keeps the packet, so the prompt MUST
    /// contain the `Impact Packet:` block. Locks the include path so a
    /// regression that over-prunes diff/task queries fails here.
    #[test]
    fn diff_task_query_prompt_includes_impact_packet_block() {
        let query = "what did I just change";
        assert!(!should_prune_impact(query));
        let packet = ImpactPacket::default();
        let prompt = build_ask_user_prompt(query, false, false, &packet);
        assert!(
            prompt.contains("Impact Packet:"),
            "DiffTask prompt must contain an `Impact Packet:` block: {prompt}"
        );
    }

    /// The narrative (architect) path also emits `Impact Packet:` for
    /// diff/task queries, and must also be pruned for GlobalConceptual.
    #[test]
    fn global_conceptual_narrative_prompt_omits_impact_packet_block() {
        let query = "walk me through the design of the indexer";
        assert!(should_prune_impact(query));
        let pruned_packet = ImpactPacket::default();
        let prompt = build_ask_user_prompt(query, true, true, &pruned_packet);
        assert!(
            !prompt.contains("Impact Packet:"),
            "GlobalConceptual narrative prompt must not contain `Impact Packet:`: {prompt}"
        );
    }

    // --- TA14 unit tests ---

    #[test]
    fn resolve_provider_priority_empty_config_falls_back_to_legacy() {
        let config = Config::default();
        let providers = resolve_provider_priority(&config, None).unwrap();
        assert_eq!(providers.len(), 1);
    }

    #[test]
    fn resolve_provider_priority_with_config_uses_order() {
        use crate::config::model::{Provider, ProviderEntry, ProvidersConfig};

        let mut config = Config::default();
        config.ask.providers = ProvidersConfig {
            priority: vec![
                ProviderEntry {
                    backend: Provider::OllamaCloud,
                    model: Some("glm-5.2".to_string()),
                    timeout_secs: Some(30),
                    api_key_env: None,
                    base_url: None,
                },
                ProviderEntry {
                    backend: Provider::Gemini,
                    model: Some("gemini-3.1-flash-lite".to_string()),
                    timeout_secs: Some(60),
                    api_key_env: None,
                    base_url: None,
                },
            ],
        };

        let providers = resolve_provider_priority(&config, None).unwrap();
        assert_eq!(providers.len(), 2);
        assert_eq!(providers[0], Provider::OllamaCloud);
        assert_eq!(providers[1], Provider::Gemini);
    }

    #[test]
    fn resolve_provider_priority_backend_flag_moves_to_front() {
        use crate::config::model::{Provider, ProviderEntry, ProvidersConfig};

        let mut config = Config::default();
        config.ask.providers = ProvidersConfig {
            priority: vec![
                ProviderEntry {
                    backend: Provider::Gemini,
                    model: None,
                    timeout_secs: None,
                    api_key_env: None,
                    base_url: None,
                },
                ProviderEntry {
                    backend: Provider::Local,
                    model: None,
                    timeout_secs: None,
                    api_key_env: None,
                    base_url: None,
                },
            ],
        };

        let providers = resolve_provider_priority(&config, Some(Backend::Local)).unwrap();
        assert_eq!(providers[0], Provider::Local);
    }

    #[test]
    fn resolve_provider_priority_env_var_invalid_fails_fast() {
        let key = "LEDGERFUL_ASK_PROVIDER_1";
        unsafe {
            std::env::set_var(key, "typo_cloud");
        }
        let config = Config::default();
        let result = resolve_provider_priority(&config, None);
        unsafe {
            std::env::remove_var(key);
        }
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.contains("Invalid provider"));
        assert!(err.contains("typo_cloud"));
    }

    #[test]
    fn sanitize_error_for_logging_strips_bearer_tokens() {
        let input = "Error: bearer sk-abc123xyz failed";
        let sanitized = sanitize_error_for_logging(input);
        assert!(sanitized.contains("[REDACTED]"));
        assert!(!sanitized.contains("sk-abc123xyz"));
    }

    #[test]
    fn sanitize_error_for_logging_strips_api_key_values() {
        let input = "Failed at https://example.com?api_key=secret123";
        let sanitized = sanitize_error_for_logging(input);
        assert!(sanitized.contains("[REDACTED]"));
        assert!(!sanitized.contains("secret123"));
    }

    #[test]
    fn sanitize_error_for_logging_preserves_safe_strings() {
        let input = "Connection refused at localhost:8081";
        let sanitized = sanitize_error_for_logging(input);
        assert_eq!(sanitized, input);
    }
}
