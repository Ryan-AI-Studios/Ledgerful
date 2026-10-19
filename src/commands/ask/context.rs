use crate::config::model::LocalModelConfig;
use crate::gemini::modes::GeminiMode;
use crate::impact::packet::ImpactPacket;
use crate::local_model::pruner;
use crate::state::storage::StorageManager;

/// Greppable honesty sentence for a live-clean working tree (prompt + stderr).
pub(crate) const WORKING_TREE_NO_PENDING_CHANGES: &str = "No pending changes found";

/// Assemble the `ask` user prompt from the resolved query string, the
/// global/diff mode flag, the narrative flag, the (possibly pruned)
/// `ImpactPacket`, whether the live working tree is clean, `GeminiMode`,
/// and an optional ReviewPatch diff payload.
/// `is_global` selects the codebase-oracle template vs the diff packet;
/// `live_tree_clean` is the only switch that injects
/// [`WORKING_TREE_NO_PENDING_CHANGES`]. Do not infer cleanliness from
/// `review_diff`. Extracted as a pure helper so the contract is
/// unit-testable without driving the full `execute_ask` flow.
#[allow(clippy::too_many_arguments)]
pub fn build_ask_user_prompt(
    query_string: &str,
    is_global: bool,
    narrative: bool,
    latest_packet: &ImpactPacket,
    live_tree_clean: bool,
    mode: GeminiMode,
    review_diff: Option<&str>,
) -> String {
    if is_global {
        if live_tree_clean {
            format!(
                "Answer the following codebase query:\n\n{WORKING_TREE_NO_PENDING_CHANGES}.\n\nQuery: {query_string}"
            )
        } else {
            format!("Answer the following codebase query:\n\nQuery: {query_string}")
        }
    } else if mode == GeminiMode::ReviewPatch {
        crate::gemini::modes::build_user_prompt(
            GeminiMode::ReviewPatch,
            latest_packet,
            query_string,
            review_diff,
        )
    } else if mode == GeminiMode::Narrative || narrative {
        crate::gemini::prompt::build_architect_prompt(latest_packet, query_string)
    } else {
        crate::gemini::prompt::build_suggest_prompt(latest_packet, query_string)
    }
}

/// Map live-tree cleanliness and assembled summaries into a ReviewPatch
/// `review_diff` payload. No git. Never returns `None` on a dirty tree.
pub(crate) fn review_diff_payload(
    live_tree_clean: bool,
    joined_summaries: &str,
    change_paths: &[&str],
) -> Option<String> {
    if live_tree_clean {
        return None;
    }
    if !joined_summaries.is_empty() {
        return Some(joined_summaries.to_string());
    }
    let capped: Vec<&str> = change_paths.iter().copied().take(8).collect();
    if !capped.is_empty() {
        return Some(capped.join("\n"));
    }
    Some("Working tree is dirty but no change summaries are available.".to_string())
}

/// Per-file byte ceiling for a ReviewPatch diff summary (the 50-line cap in
/// `get_diff_summary` does not bound minified one-line files).
pub(crate) const REVIEW_DIFF_FILE_MAX_BYTES: usize = 8_192;
/// Total byte ceiling for the joined ReviewPatch diff before the
/// `char_limit / 4` window share is applied.
pub(crate) const REVIEW_DIFF_TOTAL_MAX_BYTES: usize = 65_536;

/// Cut `s` to at most `max` bytes on a char boundary.
fn cut_at_char_boundary(s: &str, max: usize) -> &str {
    let mut end = max.min(s.len());
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Bound the joined per-file diff summaries by bytes. Each file gets
/// `min(REVIEW_DIFF_FILE_MAX_BYTES, remaining)`; cut bodies carry an explicit
/// marker and files past the budget are counted in a trailing marker.
/// Markers are not charged against `total_budget`. Pure and deterministic.
pub(crate) fn cap_review_diffs(summaries: &[String], total_budget: usize) -> String {
    if total_budget == 0 {
        return String::new();
    }
    let mut remaining = total_budget;
    let mut chunks: Vec<String> = Vec::new();
    let mut visited = 0usize;
    for summary in summaries {
        visited += 1;
        if summary.is_empty() {
            continue;
        }
        let cap = REVIEW_DIFF_FILE_MAX_BYTES.min(remaining);
        let cut_by_remaining = summary.len() > cap && cap == remaining;
        let body = cut_at_char_boundary(summary, cap);
        let mut chunk = body.to_string();
        if body.len() < summary.len() {
            let marker = format!(
                "[... diff truncated: {} bytes omitted ...]",
                summary.len() - body.len()
            );
            if !body.is_empty() && !body.ends_with('\n') {
                chunk.push('\n');
            }
            chunk.push_str(&marker);
        }
        chunks.push(chunk);
        remaining = remaining.saturating_sub(body.len());
        if remaining == 0 || cut_by_remaining {
            break;
        }
    }
    let omitted = summaries
        .iter()
        .skip(visited)
        .filter(|s| !s.is_empty())
        .count();
    let mut out = chunks.join("\n");
    if omitted > 0 {
        let marker = format!("[... {omitted} more file diff(s) omitted: byte budget ...]");
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&marker);
    }
    out
}

/// Compose the byte-capped ReviewPatch diff payload: clean tree -> `None`,
/// otherwise cap to `min(REVIEW_DIFF_TOTAL_MAX_BYTES, char_limit / 4)` and
/// defer to the unchanged [`review_diff_payload`].
pub(crate) fn assemble_review_diff(
    summaries: &[String],
    change_paths: &[&str],
    live_tree_clean: bool,
    char_limit: usize,
) -> Option<String> {
    if live_tree_clean {
        return None;
    }
    let budget = REVIEW_DIFF_TOTAL_MAX_BYTES.min(char_limit / 4);
    let capped = cap_review_diffs(summaries, budget);
    review_diff_payload(live_tree_clean, &capped, change_paths)
}

/// Packet `truncate_for_context` target after charging the escaped diff
/// (`escape_code_chunk` turns each backtick into 2 bytes). Never below
/// `char_limit / 2`.
pub(crate) fn packet_target_after_diff(char_limit: usize, review_diff: Option<&str>) -> usize {
    let charge = review_diff.map_or(0, |d| d.len() + d.bytes().filter(|&b| b == b'`').count());
    char_limit.saturating_sub(charge).max(char_limit / 2)
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

/// CR8: Escape a symbol name for safe interpolation inside a Cozo Datalog string literal.
/// Cozo uses single-quoted string literals; a single quote must be doubled to escape it.
/// Backslashes are also escaped to prevent unintended Datalog escaping sequences.
pub fn escape_cozo_string(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\'', "''")
}

/// Outcome of gathering semantic context for `ask` (DoD-4 / DoD-8 honesty).
/// Distinguishes successful empty gathers from skipped (not usable) and
/// runtime failures so callers never treat embed/query Err as "no matches".
#[derive(Debug)]
pub(crate) enum SemanticGather {
    /// Embed+query completed (chunks may be empty — true no-match / filtered).
    Chunks {
        chunks: Vec<pruner::RankedChunk>,
        read_failed: usize,
    },
    /// Semantic path not usable (no Cozo, backend not configured, etc.).
    Skipped { reason: String },
    /// Backend should work but embed/query failed at runtime.
    Failed { reason: String },
}

/// Classify Ask gather from the shared resolver **before** opening a store.
/// `stored_read_failed` is Failed (not Skipped) even though `open_dim` is None.
pub(crate) fn ask_resolution_gate(
    resolved: &crate::semantic::QueryDimResolution,
) -> Option<SemanticGather> {
    if resolved.dimension_mismatch {
        return Some(SemanticGather::Failed {
            reason:
                "embedding dimension mismatch with stored snippet_embedding (index not modified)"
                    .to_string(),
        });
    }
    if resolved.stored_read_failed {
        return Some(SemanticGather::Failed {
            reason: "stored embedding dimension read failed (index not modified)".to_string(),
        });
    }
    if resolved.open_dim.is_none() {
        return Some(SemanticGather::Skipped {
            reason: "embedding dimension unset and no stored snippet_embedding".to_string(),
        });
    }
    None
}

pub(crate) fn gather_semantic_chunks(
    storage: &StorageManager,
    work_root: &std::path::Path,
    query_string: &str,
    limit: usize,
    config: &LocalModelConfig,
    is_global: bool,
    include_kg_neighborhood: bool,
) -> SemanticGather {
    let _ = is_global;
    let Some(cozo) = storage.cozo() else {
        return SemanticGather::Skipped {
            reason: "CozoDB storage not available".to_string(),
        };
    };

    if !crate::embed::client::is_embedding_backend_configured(config) {
        return SemanticGather::Skipped {
            reason: "embedding backend not configured".to_string(),
        };
    }

    let resolved = crate::semantic::resolve_query_dimensions(config, cozo);
    if let Some(gate) = ask_resolution_gate(&resolved) {
        return gate;
    }
    let Some(dim) = resolved.open_dim else {
        return SemanticGather::Skipped {
            reason: "embedding dimension unset and no stored snippet_embedding".to_string(),
        };
    };
    if matches!(cozo.snippet_embedding_dim(), Ok(None)) {
        return SemanticGather::Skipped {
            reason: "semantic index not present".to_string(),
        };
    }

    let vector_store =
        match crate::semantic::vector_store::VectorStore::new(cozo, dim, config.disable_hnsw) {
            Ok(vs) => vs,
            Err(e) => {
                return SemanticGather::Failed {
                    reason: format!("vector store open failed: {e}"),
                };
            }
        };

    let query_vector = match crate::semantic::embedder::SemanticEmbedder::new(config.clone())
        .embed(query_string)
    {
        Ok(v) => v,
        Err(e) => {
            return SemanticGather::Failed {
                reason: format!("embedding failed: {e}"),
            };
        }
    };

    // 0152: work-root scoped query (filter-before-truncate + foreign residual).
    let results = match vector_store.query_scoped(work_root, query_vector, limit) {
        Ok((r, foreign)) => {
            if foreign > 0 {
                tracing::debug!(
                    "ask semantic gather filtered {foreign} foreign path hit(s) outside work root"
                );
            }
            r
        }
        Err(e) => {
            return SemanticGather::Failed {
                reason: format!("semantic query failed: {e}"),
            };
        }
    };

    let mut relevant_chunks = Vec::new();
    let mut semantic_symbols = std::collections::HashSet::new();

    let mut read_failed = 0;
    for (file_path, name, offset, dist) in results {
        let score = 1.0 - (dist / 2.0);
        if score >= config.chunk_min_similarity {
            semantic_symbols.insert(name.clone());
            let preview = crate::commands::search::preview_semantic_hit(
                work_root,
                &file_path,
                &name,
                offset,
                crate::commands::search::ASK_PREVIEW_CHARS,
            );
            if preview.read_failed {
                read_failed += 1;
                continue;
            }
            if preview.content.is_empty() {
                continue;
            }
            relevant_chunks.push(pruner::RankedChunk {
                source: format!("{}::{}", preview.path, preview.name),
                content: preview.content,
                score,
            });
        }
    }

    if include_kg_neighborhood
        && !semantic_symbols.is_empty()
        && let Some(kg_ctx) =
            fetch_kg_neighborhood(cozo, semantic_symbols.iter().map(|s| s.as_str()))
    {
        relevant_chunks.push(pruner::RankedChunk {
            source: "Knowledge Graph".to_string(),
            content: kg_ctx,
            score: 1.0,
        });
    }

    SemanticGather::Chunks {
        chunks: relevant_chunks,
        read_failed,
    }
}

/// CR7: Run the KG neighborhood edge query for a set of symbol names and return a
/// formatted context string, or `None` if no relevant edges are found.
pub(crate) fn fetch_kg_neighborhood(
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
pub(crate) fn fetch_kg_bm25(
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::model::LocalModelConfig;
    use crate::gemini::modes::GeminiMode;
    use crate::impact::packet::ImpactPacket;
    use crate::state::layout::Layout;
    use crate::state::storage::StorageManager;

    /// `None` from this function is one gather reason: a script error
    /// (`run_script().ok()`), zero rows, or a formatted context of 40
    /// characters or fewer. The header alone is under that floor.
    #[test]
    #[allow(non_snake_case)]
    fn fetch_kg_bm25__zero_rows__returns_none() {
        let header = "Knowledge Graph (Text Search) Matches:\n";
        assert!(
            header.len() <= 40,
            "header-only BM25 context stays at or under 40 characters, len={}",
            header.len()
        );
        let cozo = crate::state::storage_cozo::CozoStorage::new_in_memory().expect("cozo");
        assert!(fetch_kg_bm25(&cozo, "zzzz-absent-label", 5).is_none());
    }

    /// 0152: relative semantic keys must open via work_root join, not CWD.
    #[test]
    fn ask_open_path_is_root_joined_not_cwd() {
        use crate::util::path::resolve_under_work_root;

        let root = tempfile::tempdir().expect("tempdir");
        let rel = "src/nested/foo.rs";
        let full = root.path().join("src").join("nested").join("foo.rs");
        std::fs::create_dir_all(full.parent().expect("parent")).expect("mkdir");
        std::fs::write(&full, "fn foo() {}").expect("write");

        let open = resolve_under_work_root(root.path(), rel);
        assert_eq!(open, full);
        assert!(
            open.exists(),
            "root-joined path must exist when file is under work root"
        );
        // Contrast: bare Path::new(rel) is CWD-relative and must not be used.
        assert!(
            !std::path::Path::new(rel).is_absolute(),
            "relative key is not absolute"
        );
    }

    #[test]
    fn gather_semantic_chunks_mismatch_is_failed_not_drop() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = camino::Utf8Path::from_path(tmp.path()).expect("utf8 path");
        let layout = Layout::new(root);
        layout.ensure_state_dir().expect("state dir");
        let db_path = layout.state_subdir().join("ledger.db");
        let storage = StorageManager::init(db_path.as_std_path()).expect("storage init");
        let cozo = storage.cozo().expect("cozo");
        crate::semantic::vector_store::VectorStore::new_without_hnsw(cozo, 768).expect("768 store");

        let config = LocalModelConfig {
            dimensions: 384,
            base_url: "http://127.0.0.1:1".to_string(),
            embedding_model: "nomic-embed-text".to_string(),
            disable_hnsw: true,
            ..Default::default()
        };
        let outcome = gather_semantic_chunks(
            &storage,
            tmp.path(),
            "configuration provenance",
            3,
            &config,
            true,
            true,
        );
        match outcome {
            SemanticGather::Failed { reason } => {
                assert!(
                    reason.contains("mismatch"),
                    "expected mismatch failure, got: {reason}"
                );
            }
            other => panic!("expected Failed, got {other:?}"),
        }
        assert_eq!(
            cozo.snippet_embedding_dim().expect("dim"),
            Some(768),
            "Ask gather must not drop stored 768"
        );
    }

    #[test]
    #[allow(non_snake_case)]
    fn ask_context__stored_read_failed__failed_not_skipped() {
        let resolved = crate::semantic::QueryDimResolution {
            open_dim: None,
            dimension_mismatch: false,
            preferred: Some(768),
            stored: None,
            stored_read_failed: true,
        };
        match ask_resolution_gate(&resolved) {
            Some(SemanticGather::Failed { reason }) => {
                assert_eq!(
                    reason,
                    "stored embedding dimension read failed (index not modified)"
                );
                assert!(
                    !reason.to_lowercase().contains("unset"),
                    "must not look like Skipped unset: {reason}"
                );
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[test]
    fn gather_semantic_chunks_unconfigured_is_skipped_not_empty_success() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = camino::Utf8Path::from_path(tmp.path()).expect("utf8 path");
        let layout = Layout::new(root);
        layout.ensure_state_dir().expect("state dir");
        let db_path = layout.state_subdir().join("ledger.db");
        let storage = StorageManager::init(db_path.as_std_path()).expect("storage init");

        let config = LocalModelConfig::default();
        assert!(
            !crate::embed::client::is_embedding_backend_configured(&config),
            "precondition: default config unconfigured"
        );

        let outcome = gather_semantic_chunks(
            &storage,
            tmp.path(),
            "how does indexing work",
            5,
            &config,
            true,
            true,
        );
        match outcome {
            SemanticGather::Skipped { reason } => {
                assert!(
                    reason.contains("not configured") || reason.contains("Cozo"),
                    "skipped reason should name config/backend: {reason}"
                );
            }
            SemanticGather::Chunks { chunks, .. } => {
                panic!("unconfigured must not look like successful empty gather: {chunks:?}")
            }
            SemanticGather::Failed { reason } => {
                panic!("unconfigured should be Skipped, not Failed: {reason}")
            }
        }
    }

    #[test]
    fn gather_semantic_chunks_neighborhood_param_is_include_kg() {
        let src = include_str!("context.rs");
        let idx = src
            .find("if include_kg_neighborhood")
            .expect("KG attach must use include_kg_neighborhood");
        let window = &src[idx..idx.saturating_add(180).min(src.len())];
        assert!(
            window.contains("fetch_kg_neighborhood"),
            "include_kg_neighborhood gate must wrap fetch_kg_neighborhood: {window}"
        );
        assert!(
            !window.contains("if is_global"),
            "must not attach neighborhood on mere is_global: {window}"
        );
    }

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

    #[test]
    fn global_conceptual_query_prompt_omits_impact_packet_block() {
        let query = "summarize the architecture";
        assert!(should_prune_impact(query));
        let pruned_packet = ImpactPacket::default();
        let prompt = build_ask_user_prompt(
            query,
            true,
            false,
            &pruned_packet,
            false,
            GeminiMode::Analyze,
            None,
        );
        assert!(
            !prompt.contains("Impact Packet:"),
            "GlobalConceptual prompt must not contain an `Impact Packet:` block: {prompt}"
        );
        assert!(
            !prompt.contains("Impact Packet"),
            "GlobalConceptual prompt must not mention `Impact Packet` at all: {prompt}"
        );
        assert!(
            !prompt.contains(WORKING_TREE_NO_PENDING_CHANGES),
            "pruned-global must not claim the working tree is clean: {prompt}"
        );
        assert!(prompt.contains(query));
    }

    #[test]
    fn diff_task_query_prompt_includes_impact_packet_block() {
        let query = "what did I just change";
        assert!(!should_prune_impact(query));
        let packet = ImpactPacket::default();
        let prompt = build_ask_user_prompt(
            query,
            false,
            false,
            &packet,
            false,
            GeminiMode::Analyze,
            None,
        );
        assert!(
            prompt.contains("Impact Packet:"),
            "DiffTask prompt must contain an `Impact Packet:` block: {prompt}"
        );
        assert!(
            prompt.contains("DATA (untrusted repository content"),
            "DiffTask packet must be DATA-framed: {prompt}"
        );
        assert!(
            !prompt.contains(WORKING_TREE_NO_PENDING_CHANGES),
            "dirty DiffTask must not claim the working tree is clean: {prompt}"
        );
    }

    #[test]
    fn diff_task_prompt_escapes_ai_insights_with_backticks() {
        let query = "what did I just change";
        let mut packet = ImpactPacket::default();
        packet.ai_insights.push(crate::impact::packet::AiInsight {
            memory_id: "disk-insight".to_string(),
            relevance: 0.5,
            content: "```evil```".to_string(),
        });
        let prompt = build_ask_user_prompt(
            query,
            false,
            false,
            &packet,
            false,
            GeminiMode::Analyze,
            None,
        );
        assert!(
            !prompt.contains('`'),
            "ai_insights backticks must be escaped"
        );
        assert!(prompt.contains("DATA (untrusted repository content"));
        assert!(
            !prompt.contains(WORKING_TREE_NO_PENDING_CHANGES),
            "dirty DiffTask must not claim the working tree is clean: {prompt}"
        );
    }

    #[test]
    fn global_conceptual_narrative_prompt_omits_impact_packet_block() {
        let query = "walk me through the design of the indexer";
        assert!(should_prune_impact(query));
        let pruned_packet = ImpactPacket::default();
        let prompt = build_ask_user_prompt(
            query,
            true,
            true,
            &pruned_packet,
            false,
            GeminiMode::Analyze,
            None,
        );
        assert!(
            !prompt.contains("Impact Packet:"),
            "GlobalConceptual narrative prompt must not contain `Impact Packet:`: {prompt}"
        );
        assert!(
            !prompt.contains(WORKING_TREE_NO_PENDING_CHANGES),
            "pruned-global narrative must not claim the working tree is clean: {prompt}"
        );
    }

    #[test]
    fn clean_global_prompt_contains_working_tree_no_pending_changes() {
        let query = "what is change-context";
        let packet = ImpactPacket::default();
        let prompt =
            build_ask_user_prompt(query, true, false, &packet, true, GeminiMode::Analyze, None);
        assert!(
            prompt.contains(WORKING_TREE_NO_PENDING_CHANGES),
            "clean-global prompt must include the no-pending constant: {prompt}"
        );
        assert!(
            !prompt.contains("Impact Packet:"),
            "clean-global prompt must not contain an `Impact Packet:` block: {prompt}"
        );
        assert!(prompt.contains(query));
    }

    #[test]
    fn review_patch_user_prompt_with_diff_contains_data_frame() {
        let packet = ImpactPacket::default();
        let prompt = build_ask_user_prompt(
            "review this",
            false,
            false,
            &packet,
            false,
            GeminiMode::ReviewPatch,
            Some("diff --git a/file.rs\n+new line"),
        );
        assert!(prompt.contains("Diff:"));
        assert!(prompt.contains("new line"));
        assert!(prompt.contains("DATA (untrusted repository content"));
        assert!(prompt.contains("review the diff"));
    }

    #[test]
    fn review_patch_user_prompt_none_says_no_diff_available() {
        let packet = ImpactPacket::default();
        let prompt = build_ask_user_prompt(
            "review this",
            false,
            false,
            &packet,
            false,
            GeminiMode::ReviewPatch,
            None,
        );
        assert!(prompt.contains("No diff available"));
    }

    #[test]
    fn review_patch_narrative_flag_does_not_override_review() {
        let packet = ImpactPacket::default();
        let prompt = build_ask_user_prompt(
            "review this",
            false,
            true,
            &packet,
            false,
            GeminiMode::ReviewPatch,
            Some("diff --git a/file.rs\n+new line"),
        );
        assert!(prompt.contains("review the diff"));
        assert!(!prompt.contains("narrative risk assessment"));
    }

    #[test]
    fn narrative_mode_without_narrative_flag_uses_architect_prompt() {
        let packet = ImpactPacket::default();
        let prompt = build_ask_user_prompt(
            "what is the risk?",
            false,
            false,
            &packet,
            false,
            GeminiMode::Narrative,
            None,
        );
        assert!(prompt.contains("narrative risk assessment"));
        assert!(prompt.contains("architectural health"));
        assert!(!prompt.contains("verification steps"));
    }

    #[test]
    fn analyze_mode_without_narrative_uses_suggest_prompt() {
        let packet = ImpactPacket::default();
        let prompt = build_ask_user_prompt(
            "what is the risk?",
            false,
            false,
            &packet,
            false,
            GeminiMode::Analyze,
            None,
        );
        assert!(prompt.contains("verification steps"));
        assert!(!prompt.contains("narrative risk assessment"));
        assert!(!prompt.contains("Diff:"));
    }

    #[test]
    fn global_prompt_ignores_review_diff_and_uses_live_tree_clean() {
        let packet = ImpactPacket::default();
        let prompt = build_ask_user_prompt(
            "what is change-context",
            true,
            false,
            &packet,
            true,
            GeminiMode::ReviewPatch,
            Some("diff --git a/x"),
        );
        assert!(prompt.contains(WORKING_TREE_NO_PENDING_CHANGES));
        assert!(!prompt.contains("Impact Packet:"));
        assert!(!prompt.contains("Diff:"));
    }

    #[test]
    fn review_diff_payload_dirty_empty_is_not_clean() {
        let payload = review_diff_payload(false, "", &[]).expect("dirty never None");
        assert_eq!(
            payload,
            "Working tree is dirty but no change summaries are available."
        );
        assert!(!payload.to_lowercase().contains("working tree is clean"));
    }

    #[test]
    fn review_diff_payload_dirty_summaries_pass_through() {
        let payload = review_diff_payload(false, "diff --git a/x", &["x.rs"]).expect("some");
        assert_eq!(payload, "diff --git a/x");
    }

    #[test]
    fn review_diff_payload_dirty_empty_summaries_lists_paths() {
        let payload = review_diff_payload(false, "", &["a.rs", "b.rs"]).expect("paths");
        assert_eq!(payload, "a.rs\nb.rs");
    }

    #[test]
    fn review_diff_payload_caps_eight_paths() {
        let paths = ["p1", "p2", "p3", "p4", "p5", "p6", "p7", "p8", "p9"];
        let payload = review_diff_payload(false, "", &paths).expect("capped");
        let lines: Vec<&str> = payload.lines().collect();
        assert_eq!(lines.len(), 8);
        assert_eq!(lines[0], "p1");
        assert_eq!(lines[7], "p8");
        assert!(!payload.contains("p9"));
    }

    #[test]
    fn review_diff_payload_live_clean_is_none() {
        assert_eq!(review_diff_payload(true, "diff", &["a.rs"]), None);
        assert_eq!(review_diff_payload(true, "", &[]), None);
    }

    fn one_mib() -> String {
        "x".repeat(1024 * 1024)
    }

    #[test]
    fn cap_review_diffs_one_mib_single_line_is_bounded_with_marker() {
        let out = cap_review_diffs(&[one_mib()], 65_536);
        assert!(
            out.len() <= REVIEW_DIFF_FILE_MAX_BYTES + 80,
            "len={}",
            out.len()
        );
        assert!(out.contains("[... diff truncated: "));
        assert!(out.starts_with(&"x".repeat(REVIEW_DIFF_FILE_MAX_BYTES)));
    }

    #[test]
    fn cap_review_diffs_exactly_at_cap_has_no_marker() {
        let s = "y".repeat(REVIEW_DIFF_FILE_MAX_BYTES);
        let out = cap_review_diffs(std::slice::from_ref(&s), 65_536);
        assert_eq!(out, s);
    }

    #[test]
    fn cap_review_diffs_multibyte_straddling_cap_cuts_on_boundary() {
        let mut s = "a".repeat(REVIEW_DIFF_FILE_MAX_BYTES - 1);
        s.push('\u{20AC}');
        let out = cap_review_diffs(&[s], 65_536);
        assert!(out.starts_with(&"a".repeat(REVIEW_DIFF_FILE_MAX_BYTES - 1)));
        assert!(!out.contains('\u{20AC}'));
        assert!(out.contains("diff truncated: 3 bytes omitted"));
    }

    #[test]
    fn cap_review_diffs_partial_last_file_and_omitted_marker() {
        let files: Vec<String> = (0..5).map(|_| "z".repeat(8_192)).collect();
        let out = cap_review_diffs(&files, 30_400);
        let marker_at = out.find("[... diff truncated: ").expect("per-file marker");
        assert!(out[marker_at..].contains("2368 bytes omitted"));
        assert!(out.ends_with("\n[... 1 more file diff(s) omitted: byte budget ...]"));
        assert_eq!(out.matches('z').count(), 30_400);
    }

    #[test]
    fn cap_review_diffs_total_exhaustion_omits_rest() {
        let files: Vec<String> = (0..3).map(|_| "q".repeat(8_192)).collect();
        let out = cap_review_diffs(&files, 8_192);
        assert!(out.ends_with("\n[... 2 more file diff(s) omitted: byte budget ...]"));
        assert!(!out.contains("diff truncated"));
    }

    #[test]
    fn cap_review_diffs_empty_and_zero_budget() {
        assert_eq!(cap_review_diffs(&[], 100), "");
        assert_eq!(cap_review_diffs(&["abc".to_string()], 0), "");
    }

    #[test]
    fn cap_review_diffs_empty_bodies_report_no_omission() {
        let out = cap_review_diffs(&[String::new(), String::new()], 100);
        assert_eq!(out, "");
        let out = cap_review_diffs(&[String::new(), "ab".to_string()], 100);
        assert_eq!(out, "ab");
    }

    #[test]
    fn cap_review_diffs_multibyte_walkback_still_stops() {
        // remaining 5: body 4 ASCII bytes, then a 3-byte char cannot fit.
        let first = format!("abcd{}", '\u{20AC}');
        let out = cap_review_diffs(&[first, "SECOND".to_string()], 5);
        assert!(out.starts_with("abcd\n[... diff truncated: 3 bytes omitted ...]"));
        assert!(!out.contains("SECOND"));
        assert!(out.ends_with("\n[... 1 more file diff(s) omitted: byte budget ...]"));
    }

    #[test]
    fn cap_review_diffs_body_ending_in_newline_has_no_blank_line() {
        let mut s = "a".repeat(REVIEW_DIFF_FILE_MAX_BYTES - 1);
        s.push('\n');
        s.push_str("tail");
        let out = cap_review_diffs(&[s], 65_536);
        assert!(!out.contains("\n\n"));
        assert!(out.contains("a\n[... diff truncated: 4 bytes omitted ...]"));
    }

    #[test]
    fn cap_review_diffs_is_deterministic() {
        let files = vec!["m".repeat(20_000), "n".repeat(3), "o".repeat(9_000)];
        assert_eq!(
            cap_review_diffs(&files, 12_000),
            cap_review_diffs(&files, 12_000)
        );
    }

    #[test]
    fn assemble_review_diff_clean_tree_is_none() {
        assert_eq!(
            assemble_review_diff(&[one_mib()], &["a.rs"], true, 38_000),
            None
        );
    }

    #[test]
    fn assemble_review_diff_small_window_uses_quarter_char_limit() {
        let out = assemble_review_diff(&[one_mib()], &["a.rs"], false, 38_000).expect("some");
        assert!(out.matches('x').count() <= REVIEW_DIFF_FILE_MAX_BYTES);
        assert!(out.contains("diff truncated"));
    }

    #[test]
    fn assemble_review_diff_char_limit_zero_falls_back_to_paths() {
        let out = assemble_review_diff(&["abc".to_string()], &["a.rs", "b.rs"], false, 0);
        assert_eq!(out, Some("a.rs\nb.rs".to_string()));
    }

    #[test]
    fn review_patch_prompt_growth_is_bounded_for_one_mib_line() {
        let packet = ImpactPacket::default();
        let char_limit = 38_000;
        let diff = assemble_review_diff(&[one_mib()], &["a.rs"], false, char_limit);
        let with = build_ask_user_prompt(
            "review",
            false,
            false,
            &packet,
            false,
            GeminiMode::ReviewPatch,
            diff.as_deref(),
        );
        let without = build_ask_user_prompt(
            "review",
            false,
            false,
            &packet,
            false,
            GeminiMode::ReviewPatch,
            None,
        );
        assert!(
            with.len() < without.len() + char_limit / 4 + 300,
            "{}",
            with.len()
        );
        assert!(with.contains("diff truncated"));
    }

    #[test]
    fn packet_target_after_diff_charges_escaped_len_and_clamps() {
        assert_eq!(packet_target_after_diff(32_768, None), 32_768);
        assert_eq!(packet_target_after_diff(32_768, Some("abcd")), 32_764);
        assert_eq!(packet_target_after_diff(32_768, Some("``")), 32_764);
        let big = "`".repeat(20_000);
        assert_eq!(packet_target_after_diff(32_768, Some(&big)), 16_384);
        assert_eq!(packet_target_after_diff(10, Some(&big)), 5);
    }

    /// Inline copy of the pre-0544 packet pipeline (fence, pretty, escape).
    fn reference_prompt_len(packet: &ImpactPacket) -> usize {
        let mut view = packet.clone();
        for insight in &mut view.ai_insights {
            insight.content = crate::ai::fence_bridge_insight(&insight.content);
        }
        let json = serde_json::to_string_pretty(&view).expect("serialize reference");
        crate::ai::escape_code_chunk(&json).len()
    }

    /// 0544: packet text plus the charged diff stays inside `char_limit` when the
    /// diff charge is under the half-floor. The compact meter let a packet that
    /// fit compactly but not pretty through. The 0533 all-backtick clamp case
    /// (charge above `char_limit / 2`) is excluded: the clamp keeps the packet at
    /// `char_limit / 2` and the marker overshoot stays in the 20 percent reserve.
    #[test]
    fn ask_packet_target_holds_the_pretty_prompt_with_a_backtick_diff() {
        let char_limit = 32_768;
        let diff = "`".repeat(4_096);
        let charge = diff.len() + diff.bytes().filter(|&b| b == b'`').count();
        assert!(
            charge <= char_limit / 2,
            "fixture must stay under the half-floor"
        );
        let packet_target = packet_target_after_diff(char_limit, Some(&diff));

        let mut packet = ImpactPacket::default();
        let mut i = 0;
        while reference_prompt_len(&packet) <= packet_target {
            packet.changes.push(crate::impact::packet::ChangedFile {
                path: std::path::PathBuf::from(format!("src/module_{i}.rs")),
                status: "Modified".to_string(),
                old_path: None,
                is_staged: false,
                symbols: Some(vec![crate::index::symbols::Symbol {
                    name: format!("function_{i}"),
                    kind: crate::index::symbols::SymbolKind::Function,
                    is_public: true,
                    cognitive_complexity: None,
                    cyclomatic_complexity: None,
                    line_start: None,
                    line_end: None,
                    qualified_name: None,
                    byte_start: None,
                    byte_end: None,
                    entrypoint_kind: None,
                    metadata: std::collections::BTreeMap::new(),
                }]),
                imports: None,
                runtime_usage: None,
                analysis_status: Default::default(),
                analysis_warnings: Vec::new(),
                api_routes: Vec::new(),
                data_models: Vec::new(),
                ci_gates: Vec::new(),
            });
            i += 1;
        }
        let compact = serde_json::to_string(&packet).expect("compact").len();
        assert!(
            compact <= packet_target,
            "fixture must fit compactly ({compact} vs {packet_target}) so the old meter kept it"
        );

        assert!(packet.truncate_for_context(packet_target));

        let packet_len = reference_prompt_len(&packet);
        assert!(
            packet_len + charge <= char_limit,
            "packet {packet_len} plus diff charge {charge} exceeds {char_limit}"
        );
    }
}
