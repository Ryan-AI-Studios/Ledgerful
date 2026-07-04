use crate::config::model::LocalModelConfig;
use crate::impact::packet::ImpactPacket;
use crate::local_model::pruner;
use crate::state::storage::StorageManager;

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
    latest_packet: &ImpactPacket,
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

/// CR8: Escape a symbol name for safe interpolation inside a Cozo Datalog string literal.
/// Cozo uses single-quoted string literals; a single quote must be doubled to escape it.
/// Backslashes are also escaped to prevent unintended Datalog escaping sequences.
pub fn escape_cozo_string(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\'', "''")
}

pub(crate) fn gather_semantic_chunks(
    storage: &StorageManager,
    query_string: &str,
    limit: usize,
    config: &LocalModelConfig,
    is_global: bool,
) -> Vec<pruner::RankedChunk> {
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
                    relevant_chunks.push(pruner::RankedChunk {
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
            relevant_chunks.push(pruner::RankedChunk {
                source: "Knowledge Graph".to_string(),
                content: kg_ctx,
                score: 1.0,
            });
        }
    }

    relevant_chunks
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
    use crate::impact::packet::ImpactPacket;

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
        let prompt = build_ask_user_prompt(query, true, false, &pruned_packet);
        assert!(
            !prompt.contains("Impact Packet:"),
            "GlobalConceptual prompt must not contain an `Impact Packet:` block: {prompt}"
        );
        assert!(
            !prompt.contains("Impact Packet"),
            "GlobalConceptual prompt must not mention `Impact Packet` at all: {prompt}"
        );
        assert!(prompt.contains(query));
    }

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
}
