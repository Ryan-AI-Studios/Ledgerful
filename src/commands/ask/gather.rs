//! Impact packet, prune, bridge, and semantic/KG gather for `ask`.
//!
//! Backend validation stays in `execute.rs` (routing/readiness, not gather).

use crate::commands::ask::context::{SemanticGather, WORKING_TREE_NO_PENDING_CHANGES};
use crate::commands::ask::mix::{
    CandidateOrigin, RankedListItem, SymbolRow, content_words, extract_identifier_candidates,
    is_definition_shaped, is_kg_source, rewrite_identifiers, rrf_merge, split_ranked_source,
};
use crate::commands::ask::{
    Backend, fetch_kg_bm25, fetch_kg_neighborhood, gather_semantic_chunks, should_prune_impact,
};
use crate::config::model::Config;
use crate::config::model::Provider;
use crate::impact::packet::ImpactPacket;
use crate::local_model::pruner::{self, RankedChunk};
use crate::retrieval::query::{QueryIntent, classify_query};
use crate::search::TantivySearchEngine;
use crate::search::tantivy_engine::normalize_search_path;
use crate::state::layout::Layout;
use crate::state::storage::StorageManager;
use miette::Result;
use owo_colors::{OwoColorize, Stream, Style};

/// Outcome of semantic gather for honest KG-fallback notes (0096).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SemanticGatherKind {
    Succeeded,
    Skipped,
    Failed,
}

/// Named gather outputs consumed by `execute_ask` after backend validation.
pub(crate) struct GatherResult {
    pub latest_packet: ImpactPacket,
    pub is_global: bool,
    #[allow(dead_code)]
    pub had_real_packet: bool,
    #[allow(dead_code)]
    pub fresh_packet: bool,
    pub pruned_for_intent: bool,
    /// Live git (or in-memory auto-scan) showed an empty change set.
    /// Honesty for the "no pending changes" prompt/stderr; not `is_global`.
    pub live_tree_clean: bool,
    pub query_string: String,
    pub relevant_chunks: Vec<RankedChunk>,
    pub semantic_gather_kind: SemanticGatherKind,
    pub evidence: EvidenceCounts,
    /// Skip embed+KG+FTS (LLM-instruction / ping on the global arm).
    pub gather_skipped_trivial: bool,
    /// Same `GatherPlan` flag that drives KG neighborhood + banner + prompt.
    pub include_kg_neighborhood: bool,
}

/// Pinned stderr evidence tokens (0312). `structural` is omit-empty (0395).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct EvidenceCounts {
    pub semantic: usize,
    pub bm25: usize,
    pub kg: usize,
    pub snippets: usize,
    pub read_failed: usize,
    pub structural: usize,
}

pub(crate) fn format_evidence_line(counts: &EvidenceCounts) -> String {
    let mut line = format!(
        "[Evidence] semantic={} bm25={} kg={} snippets={}",
        counts.semantic, counts.bm25, counts.kg, counts.snippets
    );
    if counts.structural > 0 {
        line.push_str(&format!(" structural={}", counts.structural));
    }
    line
}

/// CLI kebab tokens for stdout `[AskMeta] provider=` (not serde snake_case).
pub(crate) fn ask_backend_token(backend: Backend) -> &'static str {
    match backend {
        Backend::Local => "local",
        Backend::Gemini => "gemini",
        Backend::OllamaCloud => "ollama-cloud",
        Backend::OpenRouter => "openrouter",
    }
}

/// CLI kebab tokens for the provider-priority arm.
pub(crate) fn ask_provider_token(provider: Provider) -> &'static str {
    match provider {
        Provider::Local => "local",
        Provider::Gemini => "gemini",
        Provider::OllamaCloud => "ollama-cloud",
        Provider::OpenRouter => "openrouter",
    }
}

/// Stdout trailer after a model answer. `length_stopped` is **not** the
/// packet `truncate_for_context` boolean; the printed key stays `truncated=`.
pub(crate) fn format_ask_meta_line(
    counts: &EvidenceCounts,
    gather_ms: u128,
    provider: &str,
    length_stopped: bool,
) -> String {
    let truncated = if length_stopped { "yes" } else { "no" };
    let structural = if counts.structural > 0 {
        format!(" structural={}", counts.structural)
    } else {
        String::new()
    };
    format!(
        "[AskMeta] semantic={} bm25={} kg={} snippets={}{structural} gatherMs={} provider={} truncated={}",
        counts.semantic, counts.bm25, counts.kg, counts.snippets, gather_ms, provider, truncated
    )
}

const INSTRUCTION_PHRASES: &[&str] = &[
    "reply with",
    "respond with",
    "say only",
    "output only",
    "print only",
];

const PING_TOKENS: &[&str] = &["pong", "ping", "hello", "hi"];

/// Locked skip-banner suffixes (0405). Not optional.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GatherSkipKind {
    LlmInstruction,
    Ping,
}

/// One compute of skip + KG neighborhood (banner/prompt use the same values).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct GatherPlan {
    pub skip: bool,
    pub skip_kind: Option<GatherSkipKind>,
    pub include_kg_neighborhood: bool,
}

fn tokenize_ask_query(s: &str) -> Vec<String> {
    s.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .collect()
}

fn starts_with_phrase_tokens(tokens: &[String], phrase: &str) -> bool {
    let phrase_tokens = tokenize_ask_query(phrase);
    if phrase_tokens.is_empty() || phrase_tokens.len() > tokens.len() {
        return false;
    }
    &tokens[..phrase_tokens.len()] == phrase_tokens.as_slice()
}

pub(crate) fn is_llm_instruction_query(query: &str) -> bool {
    let tokens = tokenize_ask_query(query);
    INSTRUCTION_PHRASES
        .iter()
        .any(|phrase| starts_with_phrase_tokens(&tokens, phrase))
}

pub(crate) fn is_ping_token(query: &str) -> bool {
    let tokens = tokenize_ask_query(query);
    matches!(tokens.as_slice(), [t] if PING_TOKENS.contains(&t.as_str()))
}

/// Skip embed+KG+FTS on the **global** arm for LLM-instruction / ping tokens.
pub(crate) fn skip_oversized_global_gather(
    query: &str,
    explicit_semantic: bool,
    is_global: bool,
) -> bool {
    let instruction = is_llm_instruction_query(query);
    let ping = is_ping_token(query);
    is_global
        && !explicit_semantic
        && !is_definition_shaped(query)
        && extract_identifier_candidates(query).is_empty()
        && classify_query(query) == QueryIntent::Unknown
        && (instruction || ping)
}

pub(crate) fn gather_plan(query: &str, explicit_semantic: bool, is_global: bool) -> GatherPlan {
    let instruction = is_llm_instruction_query(query);
    let skip = skip_oversized_global_gather(query, explicit_semantic, is_global);
    let skip_kind = if skip {
        if instruction {
            Some(GatherSkipKind::LlmInstruction)
        } else {
            Some(GatherSkipKind::Ping)
        }
    } else {
        None
    };
    let include_kg_neighborhood = !skip
        && (explicit_semantic
            || is_definition_shaped(query)
            || classify_query(query) == QueryIntent::GlobalConceptual);
    GatherPlan {
        skip,
        skip_kind,
        include_kg_neighborhood,
    }
}

pub(crate) fn format_gather_elapsed_ms(ms: u128) -> String {
    format!("gather {ms}ms")
}

pub(crate) fn format_skip_global_banner(kind: GatherSkipKind) -> String {
    let suffix = match kind {
        GatherSkipKind::LlmInstruction => "llm-instruction",
        GatherSkipKind::Ping => "ping",
    };
    format!("[Global Mode] {WORKING_TREE_NO_PENDING_CHANGES} — gather skipped ({suffix}).")
}

pub(crate) fn format_live_clean_gather_banner(include_kg_neighborhood: bool) -> String {
    if include_kg_neighborhood {
        format!(
            "[Global Mode] {WORKING_TREE_NO_PENDING_CHANGES} — querying the full Knowledge Graph for context."
        )
    } else {
        format!(
            "[Global Mode] {WORKING_TREE_NO_PENDING_CHANGES} — gathering semantic and lexical context."
        )
    }
}

pub(crate) fn global_system_prompt(skip: bool, include_kg_neighborhood: bool) -> &'static str {
    if skip {
        "You are Ledgerful, an expert software engineering assistant. Answer the user query directly. No retrieved knowledge graph or semantic context snippets were gathered for this query. Do not invent file paths."
    } else if include_kg_neighborhood {
        "You are Ledgerful, an expert software engineering assistant. You act as a codebase oracle answering architectural and implementation questions based on retrieved knowledge graph and semantic context snippets. Provide direct, technical, and accurate answers citing the retrieved snippets where relevant."
    } else {
        "You are Ledgerful, an expert software engineering assistant. You act as a codebase oracle answering architectural and implementation questions based on retrieved semantic and lexical context snippets. Provide direct, technical, and accurate answers citing the retrieved snippets where relevant."
    }
}

/// Hermetic inject for Ask mix (0395). CLI path uses [`GatherMixOpts::default`].
#[derive(Default)]
pub(crate) struct GatherMixOpts {
    pub semantic_override: Option<SemanticGather>,
    pub symbol_rows_override: Option<Vec<SymbolRow>>,
}

/// Auto-scan / latest packet / prune / QueryIntent / stale-warn / bridge
/// (today `execute.rs` :205–337).
pub(crate) fn gather_impact_and_bridge(
    storage: &StorageManager,
    layout: &Layout,
    config: &Config,
    query: &Option<String>,
    auto_scan: bool,
) -> Result<GatherResult> {
    let auto_scan_effective = auto_scan || config.ask.auto_scan_default;
    let mut live_tree_clean = false;
    let (mut latest_packet, mut is_global, had_real_packet, fresh_packet) =
        match crate::git::status::collect_changed_files_for_filter(layout) {
            Ok(v) if v.is_empty() => {
                live_tree_clean = true;
                (ImpactPacket::default(), true, false, true)
            }
            Ok(_) | Err(_) => {
                if auto_scan_effective {
                    eprintln!(
                        "{}",
                        "Auto-scanning for fresh impact context…"
                            .if_supports_color(Stream::Stderr, |s| s.cyan())
                    );
                    match crate::commands::impact::compute_impact_in_memory(storage, config) {
                        Ok(packet) => {
                            let has_changes = !packet.changes.is_empty();
                            if has_changes {
                                tracing::debug!(
                                    "ask: auto-scan produced fresh impact packet with {} changed files",
                                    packet.changes.len()
                                );
                            } else {
                                live_tree_clean = true;
                                tracing::debug!(
                                    "ask: auto-scan found clean tree — defaulting to global mode"
                                );
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
                                    (ImpactPacket::default(), true, false, false)
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
                            (ImpactPacket::default(), true, false, false)
                        }
                    }
                }
            }
        };

    if !is_global && latest_packet.changes.is_empty() {
        tracing::debug!("Latest impact packet is clean (no changes) — defaulting to global mode.");
        is_global = true;
    }

    let query_string = match query {
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
            latest_packet = ImpactPacket::default();
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
            crate::retrieval::query::QueryIntent::GlobalConceptual => {
                // Predicate should have pruned already. Safe default: keep packet.
                tracing::debug!(
                    "ask: GlobalConceptual after prune predicate false — keeping impact"
                );
            }
        }
    }

    if had_real_packet
        && !pruned_for_intent
        && !fresh_packet
        && let Some(reason) = crate::state::reports::warn_if_impact_stale(layout, config)
    {
        eprintln!(
            "{}",
            format!(
                "Warning: {reason} — using it as ask context anyway; results may not reflect the current working tree."
            ).if_supports_color(Stream::Stderr, |s| s.yellow())

        );
    }

    // Integrate external context
    if let Some(q) = query
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

    Ok(GatherResult {
        latest_packet,
        is_global,
        had_real_packet,
        fresh_packet,
        pruned_for_intent,
        live_tree_clean,
        query_string,
        relevant_chunks: Vec::new(),
        semantic_gather_kind: SemanticGatherKind::Skipped,
        evidence: EvidenceCounts::default(),
        gather_skipped_trivial: false,
        include_kg_neighborhood: false,
    })
}

/// Semantic readiness WARN + KG fallback (today `execute.rs` :393–513).
#[allow(clippy::too_many_arguments)]
pub(crate) fn gather_semantic_and_kg(
    gathered: &mut GatherResult,
    storage: &StorageManager,
    layout: &Layout,
    config: &Config,
    explicit_semantic: bool,
    auto_index: bool,
    limit: usize,
    no_kg_fallback: bool,
) {
    gather_semantic_and_kg_with(
        gathered,
        storage,
        layout,
        config,
        explicit_semantic,
        auto_index,
        limit,
        no_kg_fallback,
        GatherMixOpts::default(),
    );
}

/// Core gather with injectable semantic chunks / symbol rows (0395).
#[allow(clippy::too_many_arguments)]
pub(crate) fn gather_semantic_and_kg_with(
    gathered: &mut GatherResult,
    storage: &StorageManager,
    layout: &Layout,
    config: &Config,
    explicit_semantic: bool,
    auto_index: bool,
    limit: usize,
    no_kg_fallback: bool,
    opts: GatherMixOpts,
) {
    let plan = gather_plan(
        &gathered.query_string,
        explicit_semantic,
        gathered.is_global,
    );
    gathered.gather_skipped_trivial = plan.skip;
    gathered.include_kg_neighborhood = plan.include_kg_neighborhood;
    let semantic = (explicit_semantic || gathered.is_global) && !plan.skip;

    // 0096 DoD-5: removed interactive `index --semantic` prompt (same defect as
    // search — named semantic index, ran non-semantic incremental; re-prompted
    // forever on empty repos). State-driven warnings replace it.
    if semantic
        && !auto_index
        && let Some(cozo) = storage.cozo()
        && cozo.snippet_embedding_dim().ok().flatten().is_some()
        && let Ok(semantic_engine) =
            crate::semantic::SemanticDiscovery::new(config.local_model.clone(), cozo)
        && let Ok(readiness) = semantic_engine.check_readiness()
    {
        for msg in crate::semantic::semantic_readiness_messages(&readiness) {
            eprintln!(
                "{} {}",
                "WARN".if_supports_color(Stream::Stderr, |s| s.style(Style::new().yellow().bold())),
                msg
            );
        }
    }

    if gathered.pruned_for_intent {
        eprintln!(
            "{}",
            "[Global Mode] Conceptual query — querying the full Knowledge Graph (active diff context pruned for intent).".if_supports_color(Stream::Stderr, |s| s.cyan())

        );
    } else if gathered.live_tree_clean && !gathered.pruned_for_intent {
        let line = if let Some(kind) = plan.skip_kind {
            format_skip_global_banner(kind)
        } else {
            format_live_clean_gather_banner(plan.include_kg_neighborhood)
        };
        eprintln!("{}", line.if_supports_color(Stream::Stderr, |s| s.cyan()));
    }

    if plan.skip {
        gathered.relevant_chunks.clear();
        gathered.semantic_gather_kind = SemanticGatherKind::Skipped;
        gathered.evidence = EvidenceCounts::default();
        eprintln!("{}", format_evidence_line(&gathered.evidence));
        return;
    }

    // DoD-4/8: never treat embed/query Err as "no semantic matches".
    let mut evidence = EvidenceCounts::default();
    let semantic_result = match opts.semantic_override {
        Some(over) => over,
        None => gather_semantic_chunks(
            storage,
            layout.root.as_std_path(),
            &gathered.query_string,
            limit,
            &config.local_model,
            gathered.is_global,
            plan.include_kg_neighborhood,
        ),
    };
    let (semantic_chunks, semantic_gather_kind) = match semantic_result {
        SemanticGather::Chunks {
            chunks,
            read_failed,
        } => {
            evidence.read_failed = read_failed;
            (chunks, SemanticGatherKind::Succeeded)
        }
        SemanticGather::Skipped { reason } => {
            tracing::warn!("Semantic context skipped: {reason}");
            (Vec::new(), SemanticGatherKind::Skipped)
        }
        SemanticGather::Failed { reason } => {
            tracing::warn!("Semantic context failed: {reason}");
            eprintln!(
                "{} Semantic search failed (continuing with non-semantic context): {}",
                "WARN".if_supports_color(Stream::Stderr, |s| s.style(Style::new().yellow().bold())),
                reason
            );
            (Vec::new(), SemanticGatherKind::Failed)
        }
    };

    let mut kg_chunks: Vec<RankedChunk> = Vec::new();
    let mut vector_items: Vec<RankedListItem> = Vec::new();
    for chunk in semantic_chunks {
        if is_kg_source(&chunk.source) {
            kg_chunks.push(chunk);
            continue;
        }
        let (path, symbol_name) = split_ranked_source(&chunk.source);
        vector_items.push(RankedListItem {
            path,
            symbol_name,
            content: chunk.content,
        });
    }
    evidence.semantic = vector_items.len();
    evidence.kg = kg_chunks.len();

    let engine = TantivySearchEngine::open_or_create(layout.search_index_dir().as_std_path()).ok();
    let fts_chunks = match engine.as_ref() {
        Some(eng) => tantivy_fallback_chunks_with(eng, &gathered.query_string, limit),
        None => Vec::new(),
    };
    evidence.bm25 = fts_chunks.len();
    let fts_items: Vec<RankedListItem> = fts_chunks
        .into_iter()
        .map(|chunk| {
            let (path, symbol_name) = split_ranked_source(&chunk.source);
            RankedListItem {
                path,
                symbol_name,
                content: chunk.content,
            }
        })
        .collect();

    let explicit_ids = extract_identifier_candidates(&gathered.query_string);
    let definition_shaped = is_definition_shaped(&gathered.query_string);
    let mut structural_items: Vec<RankedListItem> = Vec::new();
    if !explicit_ids.is_empty() || definition_shaped || opts.symbol_rows_override.is_some() {
        let rows = match opts.symbol_rows_override {
            Some(rows) => rows,
            None if definition_shaped => lookup_symbols_by_content_words(
                storage.get_connection(),
                &content_words(&gathered.query_string),
            ),
            None => Vec::new(),
        };
        let names = rewrite_identifiers(&gathered.query_string, &rows);
        if let Some(eng) = engine.as_ref() {
            structural_items = structural_chunks_with(eng, &names, limit);
        }
    }
    evidence.structural = structural_items.len();

    let mut lists: Vec<(CandidateOrigin, Vec<RankedListItem>)> = Vec::new();
    if !vector_items.is_empty() {
        lists.push((CandidateOrigin::Vector, vector_items));
    }
    if !fts_items.is_empty() {
        lists.push((CandidateOrigin::Fts, fts_items));
    }
    if !structural_items.is_empty() {
        lists.push((CandidateOrigin::Structural, structural_items));
    }
    let mut relevant_chunks = rrf_merge(&lists, limit);

    if relevant_chunks.is_empty() {
        relevant_chunks = pruner::query_relevant_chunks(
            &gathered.query_string,
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

        if gathered.is_global
            && relevant_chunks.is_empty()
            && !no_kg_fallback
            && let Some(cozo) = storage.cozo()
            && let Some(kg_bm25_context) = fetch_kg_bm25(cozo, &gathered.query_string, limit)
        {
            let note = match semantic_gather_kind {
                SemanticGatherKind::Failed => {
                    "Note: semantic search failed — using KG text search for context"
                }
                SemanticGatherKind::Skipped => {
                    "Note: semantic search did not run — using KG text search for context"
                }
                SemanticGatherKind::Succeeded => {
                    "Note: semantic index empty — using KG text search for context"
                }
            };
            eprintln!("{}", note.if_supports_color(Stream::Stderr, |s| s.yellow()));
            relevant_chunks.push(pruner::RankedChunk {
                source: "Knowledge Graph (BM25)".to_string(),
                content: kg_bm25_context,
                score: 1.0,
            });
            evidence.kg += 1;
        }

        if plan.include_kg_neighborhood
            && gathered.is_global
            && !relevant_chunks.is_empty()
            && let Some(cozo) = storage.cozo()
        {
            let syms = relevant_chunks.iter().filter_map(|c| {
                let path = std::path::Path::new(&c.source);
                path.file_stem()?.to_str()
            });
            if let Some(kg_ctx) = fetch_kg_neighborhood(cozo, syms) {
                relevant_chunks.push(pruner::RankedChunk {
                    source: "Knowledge Graph".to_string(),
                    content: kg_ctx,
                    score: 0.0,
                });
                evidence.kg += 1;
            }
        }
    } else if gathered.is_global {
        for mut kg in kg_chunks {
            kg.score = 0.0;
            relevant_chunks.push(kg);
        }
    }

    evidence.snippets = relevant_chunks.len();
    eprintln!("{}", format_evidence_line(&evidence));
    gathered.relevant_chunks = relevant_chunks;
    gathered.semantic_gather_kind = semantic_gather_kind;
    gathered.evidence = evidence;
}

fn keep_max_ranked_chunk(
    merged: &mut std::collections::BTreeMap<String, RankedChunk>,
    path: String,
    content: String,
    score: f32,
) {
    merged
        .entry(path.clone())
        .and_modify(|e| {
            if score > e.score {
                e.score = score;
                e.content = content.clone();
            }
        })
        .or_insert(RankedChunk {
            source: path,
            content,
            score,
        });
}

fn tantivy_fallback_chunks_with(
    engine: &TantivySearchEngine,
    query: &str,
    limit: usize,
) -> Vec<RankedChunk> {
    let mut queries = Vec::new();
    let trimmed = query.trim();
    if !trimmed.is_empty() && !crate::commands::search::is_regex_likely(trimmed) {
        queries.push(trimmed.to_string());
    }
    queries.extend(crate::commands::search::tokenize_search_query(query));
    let mut merged: std::collections::BTreeMap<String, RankedChunk> =
        std::collections::BTreeMap::new();
    for q in queries {
        let Ok(hits) = engine.search(&q, limit.saturating_mul(2).max(1)) else {
            continue;
        };
        for hit in hits {
            let path = normalize_search_path(&hit.path);
            let content = hit.snippet.unwrap_or_default();
            if content.is_empty() {
                continue;
            }
            keep_max_ranked_chunk(&mut merged, path, content, hit.score);
        }
    }
    let mut out: Vec<RankedChunk> = merged.into_values().collect();
    out.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.source.cmp(&b.source))
    });
    out.truncate(limit);
    out
}

fn structural_chunks_with(
    engine: &TantivySearchEngine,
    names: &[String],
    limit: usize,
) -> Vec<RankedListItem> {
    let mut out: Vec<RankedListItem> = Vec::new();
    let per = limit.saturating_mul(2).max(1);
    for name in names {
        let Ok(hits) = engine.search_term_exact(name, per) else {
            continue;
        };
        for hit in hits {
            let content = hit
                .snippet
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| format!("{}::{}", normalize_search_path(&hit.path), name));
            out.push(RankedListItem {
                path: normalize_search_path(&hit.path),
                symbol_name: Some(name.clone()),
                content,
            });
        }
    }
    out
}

fn lookup_symbols_by_content_words(
    conn: &rusqlite::Connection,
    words: &[String],
) -> Vec<SymbolRow> {
    let mut out: Vec<SymbolRow> = Vec::new();
    let sql = "SELECT pf.file_path, ps.symbol_name, ps.symbol_kind
         FROM project_symbols ps
         JOIN project_files pf ON ps.file_id = pf.id
         WHERE instr(lower(ps.symbol_name), lower(?1)) > 0
         ORDER BY (CASE WHEN instr(lower(pf.file_path), 'config') > 0 THEN 0 ELSE 1 END) ASC,
                  (CASE WHEN ps.symbol_kind IN ('Function', 'Method') THEN 0 ELSE 1 END) ASC,
                  length(ps.symbol_name) ASC,
                  ps.symbol_name ASC
         LIMIT 32";
    for word in words {
        if word.is_empty() {
            continue;
        }
        let pat = word.as_str();
        let Ok(mut stmt) = conn.prepare(sql) else {
            continue;
        };
        let Ok(rows) = stmt.query_map([&pat], |row| {
            Ok(SymbolRow {
                path: row.get::<_, String>(0)?,
                name: row.get::<_, String>(1)?,
                kind: row.get::<_, String>(2).unwrap_or_else(|_| String::new()),
            })
        }) else {
            continue;
        };
        for row in rows.flatten() {
            out.push(row);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::ask::context::{WORKING_TREE_NO_PENDING_CHANGES, build_ask_user_prompt};
    use crate::impact::packet::ChangedFile;
    use std::fs;
    use std::path::Path;
    use std::process::Command;
    use tempfile::tempdir;

    fn git(cwd: &Path, args: &[&str]) -> std::process::Output {
        Command::new("git")
            .args(args)
            .current_dir(cwd)
            .env("GIT_AUTHOR_NAME", "test")
            .env("GIT_AUTHOR_EMAIL", "test@example.com")
            .env("GIT_COMMITTER_NAME", "test")
            .env("GIT_COMMITTER_EMAIL", "test@example.com")
            .output()
            .expect("git command")
    }

    fn init_repo_with_commit(dir: &Path) {
        assert!(git(dir, &["init", "-b", "main"]).status.success());
        assert!(
            git(dir, &["config", "user.email", "test@example.com"])
                .status
                .success()
        );
        assert!(git(dir, &["config", "user.name", "test"]).status.success());
        fs::write(dir.join("tracked.txt"), "hello\n").expect("write tracked");
        assert!(git(dir, &["add", "."]).status.success());
        assert!(git(dir, &["commit", "-m", "init"]).status.success());
    }

    fn dirty_cached_packet() -> ImpactPacket {
        ImpactPacket {
            tree_clean: false,
            head_hash: Some("deadbeef".to_string()),
            branch_name: Some("old-branch".to_string()),
            changes: vec![ChangedFile {
                path: std::path::PathBuf::from("src/old.rs"),
                status: "Modified".to_string(),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    fn gather_with_saved_packet(
        dir: &Path,
        packet: Option<&ImpactPacket>,
        query: &str,
        auto_scan: bool,
    ) -> GatherResult {
        let root = camino::Utf8Path::from_path(dir).expect("utf8 path");
        let layout = Layout::new(root);
        layout.ensure_state_dir().expect("state dir");
        let db_path = layout.state_subdir().join("ledger.db");
        let storage = StorageManager::init(db_path.as_std_path()).expect("storage init");
        if let Some(pkt) = packet {
            storage.save_packet(pkt).expect("save_packet");
        }
        let config = Config::default();
        let gathered = gather_impact_and_bridge(
            &storage,
            &layout,
            &config,
            &Some(query.to_string()),
            auto_scan,
        )
        .expect("gather");
        storage.shutdown().expect("shutdown");
        gathered
    }

    #[test]
    #[allow(non_snake_case)]
    fn gather_tantivy_chunks__token_score_beats_full_query__keeps_max() {
        let mut merged = std::collections::BTreeMap::new();
        keep_max_ranked_chunk(
            &mut merged,
            "src/a.rs".to_string(),
            "weak full-query snippet".to_string(),
            0.2,
        );
        keep_max_ranked_chunk(
            &mut merged,
            "src/a.rs".to_string(),
            "strong token snippet".to_string(),
            4.5,
        );
        let hit = merged.get("src/a.rs").expect("path");
        assert!((hit.score - 4.5).abs() < f32::EPSILON);
        assert_eq!(hit.content, "strong token snippet");
        keep_max_ranked_chunk(
            &mut merged,
            "src/a.rs".to_string(),
            "weaker later".to_string(),
            1.0,
        );
        let hit = merged.get("src/a.rs").expect("path");
        assert!((hit.score - 4.5).abs() < f32::EPSILON);
        assert_eq!(hit.content, "strong token snippet");
    }

    #[test]
    fn format_evidence_line_pins_tokens() {
        let line = format_evidence_line(&EvidenceCounts {
            semantic: 1,
            bm25: 2,
            kg: 3,
            snippets: 4,
            read_failed: 0,
            structural: 0,
        });
        assert_eq!(line, "[Evidence] semantic=1 bm25=2 kg=3 snippets=4");
    }

    #[test]
    #[allow(non_snake_case)]
    fn format_evidence_line__structural_omit_empty() {
        let none = format_evidence_line(&EvidenceCounts {
            semantic: 2,
            bm25: 1,
            kg: 0,
            snippets: 3,
            read_failed: 0,
            structural: 0,
        });
        assert_eq!(none, "[Evidence] semantic=2 bm25=1 kg=0 snippets=3");
        let some = format_evidence_line(&EvidenceCounts {
            semantic: 2,
            bm25: 1,
            kg: 0,
            snippets: 3,
            read_failed: 0,
            structural: 4,
        });
        assert_eq!(
            some,
            "[Evidence] semantic=2 bm25=1 kg=0 snippets=3 structural=4"
        );
    }

    #[test]
    fn ask_backend_token_pins_kebab_literals() {
        assert_eq!(ask_backend_token(Backend::Local), "local");
        assert_eq!(ask_backend_token(Backend::Gemini), "gemini");
        assert_eq!(ask_backend_token(Backend::OllamaCloud), "ollama-cloud");
        assert_eq!(ask_backend_token(Backend::OpenRouter), "openrouter");
        assert_eq!(ask_provider_token(Provider::Local), "local");
        assert_eq!(ask_provider_token(Provider::Gemini), "gemini");
        assert_eq!(ask_provider_token(Provider::OllamaCloud), "ollama-cloud");
        assert_eq!(ask_provider_token(Provider::OpenRouter), "openrouter");
    }

    #[test]
    fn ask_meta_line_names_counts_and_truncated() {
        let zeros = format_ask_meta_line(&EvidenceCounts::default(), 12, "local", false);
        assert_eq!(
            zeros,
            "[AskMeta] semantic=0 bm25=0 kg=0 snippets=0 gatherMs=12 provider=local truncated=no"
        );
        let stopped = format_ask_meta_line(
            &EvidenceCounts {
                semantic: 1,
                bm25: 2,
                kg: 3,
                snippets: 4,
                read_failed: 0,
                structural: 0,
            },
            8,
            "gemini",
            true,
        );
        assert_eq!(
            stopped,
            "[AskMeta] semantic=1 bm25=2 kg=3 snippets=4 gatherMs=8 provider=gemini truncated=yes"
        );
        let with_structural = format_ask_meta_line(
            &EvidenceCounts {
                semantic: 2,
                bm25: 1,
                kg: 0,
                snippets: 3,
                read_failed: 0,
                structural: 4,
            },
            5,
            "ollama-cloud",
            false,
        );
        assert_eq!(
            with_structural,
            "[AskMeta] semantic=2 bm25=1 kg=0 snippets=3 structural=4 gatherMs=5 provider=ollama-cloud truncated=no"
        );
        // Packet truncate_for_context must not flip the key when length_stopped is false.
        let packet_truncated_context =
            format_ask_meta_line(&EvidenceCounts::default(), 1, "openrouter", false);
        assert!(packet_truncated_context.contains("truncated=no"));
        assert!(!packet_truncated_context.contains("truncated=yes"));
    }

    #[test]
    #[allow(non_snake_case)]
    fn evidence__pre_fusion_lists_may_exceed_snippets() {
        let counts = EvidenceCounts {
            semantic: 2,
            bm25: 2,
            kg: 1,
            snippets: 3,
            read_failed: 0,
            structural: 2,
        };
        assert!(
            counts.semantic + counts.bm25 + counts.kg + counts.structural > counts.snippets,
            "pre-fusion list sizes may exceed post-fusion snippets"
        );
        let line = format_evidence_line(&counts);
        assert!(line.contains("structural=2"));
        assert!(line.contains("snippets=3"));
    }

    #[test]
    #[allow(non_snake_case)]
    fn lookup_symbols_by_content_words__uses_symbol_name_join() {
        let dir = tempdir().expect("tempdir");
        let root = camino::Utf8Path::from_path(dir.path()).expect("utf8");
        let layout = Layout::new(root);
        layout.ensure_state_dir().expect("state");
        let storage = StorageManager::init(layout.state_subdir().join("ledger.db").as_std_path())
            .expect("storage");
        {
            let conn = storage.get_connection();
            conn.execute(
                "INSERT INTO project_files (id, file_path, last_indexed_at) VALUES
                 (1, 'src/commands/config_verify.rs', '2026-01-01T00:00:00Z'),
                 (2, 'src/ledger/db.rs', '2026-01-01T00:00:00Z'),
                 (3, 'src/ledger/provenance.rs', '2026-01-01T00:00:00Z')",
                [],
            )
            .expect("files");
            conn.execute(
                "INSERT INTO project_symbols (id, file_id, qualified_name, symbol_name, symbol_kind, last_indexed_at) VALUES
                 (1, 1, 'apply_provenance', 'apply_provenance', 'Function', '2026-01-01T00:00:00Z'),
                 (2, 2, 'find_transactions_by_file', 'find_transactions_by_file', 'Function', '2026-01-01T00:00:00Z'),
                 (3, 3, 'ProvenanceAction', 'ProvenanceAction', 'Enum', '2026-01-01T00:00:00Z')",
                [],
            )
            .expect("symbols");
            let rows = lookup_symbols_by_content_words(conn, &["provenance".into()]);
            assert!(
                rows.iter().any(|r| r.name == "apply_provenance"),
                "JOIN symbol_name must find apply_provenance: {:?}",
                rows.iter().map(|r| r.name.clone()).collect::<Vec<_>>()
            );
            assert!(
                rows.iter().all(|r| !r.path.is_empty()),
                "file_path comes from project_files JOIN"
            );
        }
        storage.shutdown().expect("shutdown");
    }

    #[test]
    fn skip_oversized_global_gather_pong_instruction_and_ping_token() {
        assert!(skip_oversized_global_gather(
            "Reply with the single word pong.",
            false,
            true
        ));
        assert!(skip_oversized_global_gather("pong", false, true));
        assert!(skip_oversized_global_gather("hello", false, true));
        assert!(!skip_oversized_global_gather("architecture", false, true));
        assert!(!skip_oversized_global_gather("config", false, true));
        assert!(!skip_oversized_global_gather("MCP", false, true));
        assert!(!skip_oversized_global_gather(
            "Where is configuration provenance resolved?",
            false,
            true
        ));
        assert!(!skip_oversized_global_gather(
            "what is change-context",
            false,
            true
        ));
        assert!(!skip_oversized_global_gather("pong", true, true));
        assert!(!skip_oversized_global_gather("pong", false, false));
        assert!(!skip_oversized_global_gather("test", false, true));
        assert!(!skip_oversized_global_gather(
            "How is single word matching implemented in the tokenizer?",
            false,
            true
        ));
        assert!(!skip_oversized_global_gather(
            "Does ask output only JSON when json is set?",
            false,
            true
        ));
        assert!(!skip_oversized_global_gather(
            "Can the agent reply with file paths from Evidence?",
            false,
            true
        ));
        let instruction = gather_plan("Reply with the single word pong.", false, true);
        assert_eq!(instruction.skip_kind, Some(GatherSkipKind::LlmInstruction));
        assert!(!instruction.include_kg_neighborhood);
        let ping = gather_plan("pong", false, true);
        assert_eq!(ping.skip_kind, Some(GatherSkipKind::Ping));
        let conceptual = gather_plan("architecture", false, true);
        assert!(!conceptual.skip);
        assert!(conceptual.include_kg_neighborhood);
        let subsystem = gather_plan("config", false, true);
        assert!(!subsystem.skip);
        assert!(!subsystem.include_kg_neighborhood);
    }

    #[test]
    fn skip_banner_and_gather_elapsed_format_are_locked() {
        let instruction = format_skip_global_banner(GatherSkipKind::LlmInstruction);
        assert!(instruction.contains(WORKING_TREE_NO_PENDING_CHANGES));
        assert!(instruction.contains("gather skipped (llm-instruction)"));
        assert!(!instruction.contains("full Knowledge Graph"));
        let ping = format_skip_global_banner(GatherSkipKind::Ping);
        assert!(ping.contains("gather skipped (ping)"));
        let kg = format_live_clean_gather_banner(true);
        assert!(kg.contains("full Knowledge Graph"));
        let lexical = format_live_clean_gather_banner(false);
        assert!(lexical.contains("semantic and lexical context"));
        assert!(!lexical.contains("full Knowledge Graph"));
        let elapsed = format_gather_elapsed_ms(12);
        assert!(
            regex_gather_elapsed().is_match(&elapsed),
            "elapsed={elapsed}"
        );
        let skip_prompt = global_system_prompt(true, false);
        assert!(
            !skip_prompt.contains("based on retrieved knowledge graph"),
            "skip prompt must not claim snippets were gathered"
        );
        assert!(skip_prompt.contains("No retrieved knowledge graph"));
        let kg_prompt = global_system_prompt(false, true);
        assert!(kg_prompt.contains("knowledge graph"));
        let lexical_prompt = global_system_prompt(false, false);
        assert!(!lexical_prompt.contains("knowledge graph"));
        assert!(lexical_prompt.contains("semantic and lexical"));
    }

    fn regex_gather_elapsed() -> regex::Regex {
        regex::Regex::new(r"\bgather \d+ms\b").expect("gather elapsed regex")
    }

    #[test]
    fn skip_path_evidence_zeros_and_does_not_embed() {
        let dir = tempdir().expect("tempdir");
        init_repo_with_commit(dir.path());
        let root = camino::Utf8Path::from_path(dir.path()).expect("utf8");
        let layout = Layout::new(root);
        layout.ensure_state_dir().expect("state");
        let storage = StorageManager::init(layout.state_subdir().join("ledger.db").as_std_path())
            .expect("storage");
        let mut gathered = GatherResult {
            latest_packet: ImpactPacket::default(),
            is_global: true,
            had_real_packet: false,
            fresh_packet: true,
            pruned_for_intent: false,
            live_tree_clean: true,
            query_string: "Reply with the single word pong.".into(),
            relevant_chunks: vec![RankedChunk {
                source: "should-be-cleared".into(),
                content: "x".into(),
                score: 1.0,
            }],
            semantic_gather_kind: SemanticGatherKind::Succeeded,
            evidence: EvidenceCounts {
                semantic: 9,
                ..EvidenceCounts::default()
            },
            gather_skipped_trivial: false,
            include_kg_neighborhood: false,
        };
        gather_semantic_and_kg_with(
            &mut gathered,
            &storage,
            &layout,
            &Config::default(),
            false,
            false,
            3,
            true,
            GatherMixOpts {
                semantic_override: Some(SemanticGather::Chunks {
                    chunks: vec![RankedChunk {
                        source: "must-not-run".into(),
                        content: "if skip used this, evidence would be nonempty".into(),
                        score: 0.9,
                    }],
                    read_failed: 0,
                }),
                symbol_rows_override: None,
            },
        );
        assert!(gathered.gather_skipped_trivial);
        assert!(!gathered.include_kg_neighborhood);
        assert!(gathered.relevant_chunks.is_empty());
        assert_eq!(gathered.evidence, EvidenceCounts::default());
        assert!(
            !gathered
                .relevant_chunks
                .iter()
                .any(|c| c.source.starts_with("Knowledge Graph"))
        );
        storage.shutdown().expect("shutdown");
    }

    #[test]
    fn fallback_neighborhood_site_is_gated_on_include_kg() {
        let src = include_str!("gather.rs");
        let fn_at = src
            .find("fn gather_semantic_and_kg_with")
            .expect("gather_semantic_and_kg_with");
        let body = &src[fn_at..];
        let call_at = body
            .find("fetch_kg_neighborhood")
            .expect("pruner-fallback neighborhood call");
        let window_start = call_at.saturating_sub(400);
        let window = &body[window_start..call_at];
        assert!(
            window.contains("include_kg_neighborhood"),
            "fetch_kg_neighborhood must sit behind include_kg_neighborhood; window={window}"
        );
        assert!(
            !window.contains("if gathered.is_global\n            && !relevant_chunks.is_empty()"),
            "must not attach fallback neighborhood on mere is_global"
        );
    }

    #[test]
    #[allow(non_snake_case)]
    fn gather_semantic_and_kg__nonempty_vectors__still_runs_tantivy() {
        use crate::search::trigram::extract_trigrams;
        use tantivy::TantivyDocument;

        let dir = tempdir().expect("tempdir");
        init_repo_with_commit(dir.path());
        let root = camino::Utf8Path::from_path(dir.path()).expect("utf8");
        let layout = Layout::new(root);
        layout.ensure_state_dir().expect("state");
        let storage = StorageManager::init(layout.state_subdir().join("ledger.db").as_std_path())
            .expect("storage");

        let engine = TantivySearchEngine::open_or_create(layout.search_index_dir().as_std_path())
            .expect("engine");
        {
            let schema = engine.schema();
            let path_field = schema.get_field("path").expect("path");
            let content_field = schema.get_field("content").expect("content");
            let line_count_field = schema.get_field("line_count").expect("line_count");
            let trigrams_field = schema.get_field("trigrams").expect("trigrams");
            let mut writer = engine.get_writer(15_000_000).expect("writer");
            let docs = [
                (
                    "src/commands/config_verify.rs",
                    "fn apply_provenance(row: &mut ConfigRow, ctx: &ProvenanceContext) { }",
                ),
                (
                    "src/ledger/provenance.rs",
                    "provenance provenance provenance token provenance ledger",
                ),
            ];
            for (path, content) in docs {
                let tgrams_str = extract_trigrams(content)
                    .into_iter()
                    .collect::<Vec<_>>()
                    .join(" ");
                let mut doc = TantivyDocument::default();
                doc.add_text(path_field, path);
                doc.add_text(content_field, content);
                doc.add_u64(line_count_field, 1);
                doc.add_text(trigrams_field, &tgrams_str);
                writer.add_document(doc).expect("add");
            }
            writer.commit().expect("commit");
            engine.reload_reader().expect("reload");
        }

        let mut gathered = GatherResult {
            latest_packet: ImpactPacket::default(),
            is_global: true,
            had_real_packet: false,
            fresh_packet: true,
            pruned_for_intent: false,
            live_tree_clean: true,
            query_string: "Where is configuration provenance resolved? Cite file paths.".into(),
            relevant_chunks: Vec::new(),
            semantic_gather_kind: SemanticGatherKind::Skipped,
            evidence: EvidenceCounts::default(),
            gather_skipped_trivial: false,
            include_kg_neighborhood: false,
        };
        let config = Config::default();
        gather_semantic_and_kg_with(
            &mut gathered,
            &storage,
            &layout,
            &config,
            true,
            false,
            3,
            true,
            GatherMixOpts {
                semantic_override: Some(SemanticGather::Chunks {
                    chunks: vec![
                        RankedChunk {
                            source: "src/ledger/db.rs::mod".into(),
                            content: "mod provenance;".into(),
                            score: 0.78,
                        },
                        RankedChunk {
                            source: "Knowledge Graph".into(),
                            content: "Knowledge Graph Relationships:\n- x calls y".into(),
                            score: 1.0,
                        },
                    ],
                    read_failed: 0,
                }),
                symbol_rows_override: Some(vec![
                    SymbolRow {
                        name: "apply_provenance".into(),
                        path: "src/commands/config_verify.rs".into(),
                        kind: "Function".into(),
                    },
                    SymbolRow {
                        name: "find_transactions_by_file".into(),
                        path: "src/ledger/db.rs".into(),
                        kind: "Function".into(),
                    },
                ]),
            },
        );
        assert!(
            gathered.evidence.bm25 > 0,
            "nonempty vectors must not skip Tantivy: {:?}",
            gathered.evidence
        );
        assert!(
            gathered
                .relevant_chunks
                .iter()
                .any(|c| c.source.contains("config_verify.rs")),
            "fused snippets must include config_verify.rs: {:?}",
            gathered
                .relevant_chunks
                .iter()
                .map(|c| c.source.clone())
                .collect::<Vec<_>>()
        );
        assert_eq!(gathered.evidence.semantic, 1, "KG excluded from semantic");
        let kg = gathered
            .relevant_chunks
            .iter()
            .find(|c| c.source.starts_with("Knowledge Graph"));
        if let Some(kg) = kg {
            assert!(kg.score < 0.001, "KG must not steal rank-1: {}", kg.score);
        }
        storage.shutdown().expect("shutdown");
    }

    #[test]
    fn gather_live_tree_clean_wall_overrides_dirty_cached_packet() {
        let dir = tempdir().expect("tempdir");
        init_repo_with_commit(dir.path());
        let gathered = gather_with_saved_packet(
            dir.path(),
            Some(&dirty_cached_packet()),
            "what is change-context",
            false,
        );
        assert!(gathered.is_global);
        assert!(gathered.live_tree_clean);
        assert!(gathered.latest_packet.changes.is_empty());
        assert!(!gathered.pruned_for_intent);

        let gathered_auto = gather_with_saved_packet(
            dir.path(),
            Some(&dirty_cached_packet()),
            "what is change-context",
            true,
        );
        assert!(gathered_auto.live_tree_clean);
        assert!(gathered_auto.is_global);
        assert!(gathered_auto.latest_packet.changes.is_empty());
    }

    #[test]
    fn gather_watch_ignored_target_dirt_is_live_clean() {
        let dir = tempdir().expect("tempdir");
        init_repo_with_commit(dir.path());
        let target = dir.path().join("target");
        fs::create_dir_all(&target).expect("mkdir target");
        fs::write(target.join("artifact.o"), "obj\n").expect("write target artifact");
        let gathered = gather_with_saved_packet(
            dir.path(),
            Some(&dirty_cached_packet()),
            "what is change-context",
            false,
        );
        assert!(
            gathered.live_tree_clean,
            "watch-ignored target/ dirt must still take the live-clean wall"
        );
        assert!(gathered.is_global);
        assert!(gathered.latest_packet.changes.is_empty());
        assert!(!gathered.pruned_for_intent);
    }

    #[test]
    fn gather_git_err_does_not_take_live_clean_wall() {
        let dir = tempdir().expect("tempdir");
        let gathered = gather_with_saved_packet(
            dir.path(),
            Some(&dirty_cached_packet()),
            "what is change-context",
            false,
        );
        assert!(
            !gathered.live_tree_clean,
            "git discovery Err must not take the live-clean wall"
        );
        assert!(!gathered.latest_packet.changes.is_empty());
        assert!(!gathered.is_global);
    }

    #[test]
    fn gather_dirty_tree_empty_snapshot_is_not_live_clean() {
        let dir = tempdir().expect("tempdir");
        init_repo_with_commit(dir.path());
        fs::write(dir.path().join("tracked.txt"), "dirty\n").expect("dirty write");
        let gathered = gather_with_saved_packet(dir.path(), None, "what is change-context", false);
        assert!(
            !gathered.live_tree_clean,
            "dirty tree + empty/missing snapshot must not claim live-clean"
        );
        let prompt = build_ask_user_prompt(
            &gathered.query_string,
            gathered.is_global,
            false,
            &gathered.latest_packet,
            gathered.live_tree_clean,
        );
        assert!(
            !prompt.contains(WORKING_TREE_NO_PENDING_CHANGES),
            "dirty-empty-snapshot prompt must not include the no-pending constant: {prompt}"
        );
    }

    #[test]
    fn gather_dirty_tree_loads_cached_packet() {
        let dir = tempdir().expect("tempdir");
        init_repo_with_commit(dir.path());
        fs::write(dir.path().join("tracked.txt"), "dirty\n").expect("dirty write");
        let gathered = gather_with_saved_packet(
            dir.path(),
            Some(&dirty_cached_packet()),
            "what is change-context",
            false,
        );
        assert!(!gathered.live_tree_clean);
        assert!(!gathered.is_global);
        assert_eq!(gathered.latest_packet.changes.len(), 1);
        assert!(!gathered.pruned_for_intent);
    }
}
