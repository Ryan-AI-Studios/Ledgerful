use crate::config::model::LocalModelConfig;
use crate::contracts::AffectedContract;
use crate::embed::client::embed_long_text;
use crate::gemini::modes::GeminiMode;
use crate::impact::packet::{
    AiInsight, ChangedFile, Hotspot, ImpactPacket, RelevantDecision, RiskLevel, TemporalCoupling,
};
use crate::observability::signal::ObservabilitySignal;
use crate::retrieval::query::{self, RetrievedChunk};
use rusqlite::Connection;
use std::collections::HashSet;

/// A relevance-ranked context chunk with its source and score.
#[derive(Debug, Clone)]
pub struct RankedChunk {
    pub content: String,
    pub source: String,
    pub score: f32,
}

/// Zero-copy lifetime-bound subset view of an ImpactPacket after mode-aware pruning.
pub struct PrunedPacket<'a> {
    pub risk_level: &'a RiskLevel,
    pub risk_reasons: &'a [String],
    pub changes: &'a [ChangedFile],
    pub temporal_couplings: &'a [TemporalCoupling],
    pub hotspots: &'a [Hotspot],
    pub decisions: &'a [RelevantDecision],
    pub observability: &'a [ObservabilitySignal],
    pub contracts: &'a [AffectedContract],
    pub ai_insights: &'a [AiInsight],
}

/// Count tokens in text using the char-length heuristic (tokens ~ chars/4).
/// A production system would use `tiktoken-rs` or `tokenizers`; this is a
/// reasonable approximation for budget enforcement.
pub fn estimate_tokens(text: &str) -> usize {
    text.len() / 4
}

/// Count tokens in a slice of RankedChunks.
pub fn estimate_chunks_tokens(chunks: &[RankedChunk]) -> usize {
    chunks.iter().map(|c| estimate_tokens(&c.content)).sum()
}

/// Compute a simple word-level Jaccard similarity between two strings.
/// Used for near-duplicate detection during chunk deduplication.
fn word_jaccard(a: &str, b: &str) -> f32 {
    let words_a: HashSet<&str> = a.split_whitespace().collect();
    let words_b: HashSet<&str> = b.split_whitespace().collect();

    if words_a.is_empty() || words_b.is_empty() {
        return 0.0;
    }

    let intersection = words_a.intersection(&words_b).count();
    let union = words_a.union(&words_b).count();

    intersection as f32 / union as f32
}

/// Deduplicate ranked chunks, removing any chunk whose word-level Jaccard
/// similarity to an already-kept chunk exceeds the threshold. Keeps higher-scored
/// chunks when duplicates are found.
pub fn deduplicate_chunks(chunks: &[RankedChunk], threshold: f32) -> Vec<RankedChunk> {
    let mut kept: Vec<RankedChunk> = Vec::new();

    for chunk in chunks {
        let is_dup = kept.iter().any(|existing| {
            let sim = word_jaccard(&existing.content, &chunk.content);
            sim > threshold
        });

        if !is_dup {
            kept.push(chunk.clone());
        }
    }

    kept
}

/// Prune an ImpactPacket down to only the fields relevant for the given mode.
///
/// - `ReviewPatch`: keeps diff-related fields; drops observability, contracts, decisions.
/// - `Analyze`: keeps risk, changed files, temporal couplings, decisions; drops
///   observability unless risk level is High.
/// - `Narrative`: keeps risk, hotspots, decisions; trims contracts to empty.
pub fn prune_impact_packet<'a>(packet: &'a ImpactPacket, mode: GeminiMode) -> PrunedPacket<'a> {
    match mode {
        GeminiMode::ReviewPatch => PrunedPacket {
            risk_level: &packet.risk_level,
            risk_reasons: &packet.risk_reasons,
            changes: &packet.changes,
            temporal_couplings: &[],
            hotspots: &[],
            decisions: &[],
            observability: &[],
            contracts: &[],
            ai_insights: &[],
        },
        GeminiMode::Analyze => {
            let observability = if packet.risk_level == RiskLevel::High {
                &packet.observability
            } else {
                &[][..]
            };
            PrunedPacket {
                risk_level: &packet.risk_level,
                risk_reasons: &packet.risk_reasons,
                changes: &packet.changes,
                temporal_couplings: &packet.temporal_couplings,
                hotspots: &packet.hotspots,
                decisions: &packet.relevant_decisions,
                observability,
                contracts: &packet.affected_contracts,
                ai_insights: &packet.ai_insights,
            }
        }
        GeminiMode::Narrative => PrunedPacket {
            risk_level: &packet.risk_level,
            risk_reasons: &packet.risk_reasons,
            changes: &[],
            temporal_couplings: &[],
            hotspots: &packet.hotspots,
            decisions: &packet.relevant_decisions,
            observability: &packet.observability,
            contracts: &[],
            ai_insights: &packet.ai_insights,
        },
        GeminiMode::Suggest => PrunedPacket {
            risk_level: &packet.risk_level,
            risk_reasons: &packet.risk_reasons,
            changes: &packet.changes,
            temporal_couplings: &packet.temporal_couplings,
            hotspots: &packet.hotspots,
            decisions: &packet.relevant_decisions,
            observability: &packet.observability,
            contracts: &packet.affected_contracts,
            ai_insights: &packet.ai_insights,
        },
    }
}

/// Format a PrunedPacket as human-readable text for inclusion in the user prompt.
pub fn format_pruned_packet(pruned: &PrunedPacket<'_>) -> String {
    let mut out = String::new();

    out.push_str("## Change Impact Summary\n\n");
    out.push_str(&format!("Risk Level: {:?}\n", pruned.risk_level));

    if !pruned.risk_reasons.is_empty() {
        out.push_str("\nRisk Reasons:\n");
        for reason in pruned.risk_reasons {
            out.push_str(&format!("- {reason}\n"));
        }
    }

    if !pruned.changes.is_empty() {
        out.push_str(&format!("\nChanged Files ({}):\n", pruned.changes.len()));
        for change in pruned.changes {
            out.push_str(&format!(
                "- {} ({})\n",
                change.path.display(),
                change.status
            ));
        }
    }

    if !pruned.temporal_couplings.is_empty() {
        out.push_str(&format!(
            "\nTemporal Couplings ({}):\n",
            pruned.temporal_couplings.len()
        ));
        for coupling in pruned.temporal_couplings {
            out.push_str(&format!(
                "- {} <-> {} (score: {:.2})\n",
                coupling.file_a.display(),
                coupling.file_b.display(),
                coupling.score
            ));
        }
    }

    if !pruned.hotspots.is_empty() {
        out.push_str(&format!("\nHotspots ({}):\n", pruned.hotspots.len()));
        for hs in pruned.hotspots {
            out.push_str(&format!(
                "- {} (score: {:.2}, freq: {})\n",
                hs.path.display(),
                hs.score,
                hs.frequency
            ));
        }
    }

    if !pruned.decisions.is_empty() {
        out.push_str(&format!(
            "\nRelevant Architecture Documents ({}):\n",
            pruned.decisions.len()
        ));
        for decision in pruned.decisions {
            let heading = decision.heading.as_deref().unwrap_or("(untitled)");
            out.push_str(&format!("- {heading} ({})\n", decision.file_path.display()));
            // Include snippet of excerpt (first 200 chars)
            let excerpt = truncate_at_boundary(&decision.excerpt, 200);
            out.push_str(&format!("  {excerpt}\n"));
        }
    }

    if !pruned.ai_insights.is_empty() {
        out.push_str(&format!(
            "\nExternal AI-Brains Context ({}):\n",
            pruned.ai_insights.len()
        ));
        for insight in pruned.ai_insights {
            out.push_str(&format!(
                "- [{:.2}] {}\n",
                insight.relevance, insight.content
            ));
        }
    }

    if !pruned.observability.is_empty() {
        out.push_str(&format!(
            "\nObservability Signals ({}):\n",
            pruned.observability.len()
        ));
        for signal in pruned.observability {
            let sev = format!("{:?}", signal.severity);
            out.push_str(&format!(
                "- {type}: {label} [{sev}]\n",
                type = signal.signal_type,
                label = signal.signal_label,
            ));
        }
    }

    if !pruned.contracts.is_empty() {
        out.push_str(&format!(
            "\nAffected API Contracts ({}):\n",
            pruned.contracts.len()
        ));
        for contract in pruned.contracts {
            out.push_str(&format!(
                "- {} {} (spec: {})\n",
                contract.method, contract.path, contract.spec_file
            ));
        }
    }

    out
}

fn truncate_at_boundary(s: &str, max_chars: usize) -> &str {
    if s.len() <= max_chars {
        return s;
    }
    let boundary = s.floor_char_boundary(max_chars);
    let prefix = &s[..boundary];
    // Try to break at last space before boundary
    if let Some(last_space) = prefix.rfind(' ') {
        &s[..last_space]
    } else {
        prefix
    }
}

/// Query the embedding server for chunks relevant to the user's query.
///
/// 1. Embeds the query text via the local embedding server.
/// 2. Retrieves top candidates from `doc_chunks` and `project_symbols` by cosine similarity.
/// 3. Filters out chunks below `min_similarity`.
/// 4. Deduplicates near-duplicate chunks (word Jaccard > `dedup_threshold`).
/// 5. Returns top-K chunks sorted by similarity.
pub fn query_relevant_chunks(
    query: &str,
    config: &LocalModelConfig,
    conn: &Connection,
    top_k: usize,
    min_similarity: f32,
    dedup_threshold: f32,
) -> Result<Vec<RankedChunk>, String> {
    if config.base_url.is_empty() || top_k == 0 {
        return Ok(Vec::new());
    }

    // Graceful degradation: if embedding fails, fall back to empty chunks
    let query_vec = match embed_long_text(config, query) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("Embedding server unavailable ({e}), skipping semantic retrieval");
            return Ok(Vec::new());
        }
    };

    let mut all_retrieved: Vec<RetrievedChunk> = Vec::new();

    // Query doc_chunks
    if let Ok(docs) = query::retrieve_top_k(
        conn,
        &query_vec,
        "doc_chunk",
        &config.embedding_model,
        top_k * 2,
    ) {
        all_retrieved.extend(docs);
    }

    // Query project_symbols
    if let Ok(symbols) = query::retrieve_top_k(
        conn,
        &query_vec,
        "project_symbol",
        &config.embedding_model,
        top_k,
    ) {
        all_retrieved.extend(symbols);
    }

    if all_retrieved.is_empty() {
        return Ok(Vec::new());
    }

    // Sort by similarity descending
    all_retrieved.sort_unstable_by(|a, b| {
        b.similarity
            .partial_cmp(&a.similarity)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.entity_id.cmp(&b.entity_id))
    });

    // Convert to RankedChunk, filtering by min_similarity
    let mut scored: Vec<RankedChunk> = all_retrieved
        .iter()
        .filter(|rc| rc.similarity >= min_similarity)
        .map(|rc| RankedChunk {
            content: rc.content.clone(),
            source: rc.file_path.clone(),
            score: rc.similarity,
        })
        .collect();

    // Deduplicate near-duplicates
    scored = deduplicate_chunks(&scored, dedup_threshold);

    // Limit to top_k
    scored.truncate(top_k);

    Ok(scored)
}

/// Perform keyword-based fallback retrieval when the embedding server is unavailable.
/// Ranks chunks by the number of query words found in the content.
pub fn keyword_fallback(
    query: &str,
    conn: &Connection,
    top_k: usize,
    min_similarity: f32,
    dedup_threshold: f32,
) -> Result<Vec<RankedChunk>, String> {
    let query_words: Vec<&str> = query.split_whitespace().filter(|w| w.len() >= 3).collect();

    if query_words.is_empty() {
        return Ok(Vec::new());
    }

    // Load all doc_chunks from DB
    let mut stmt = conn
        .prepare("SELECT content, file_path FROM doc_chunks LIMIT 500")
        .map_err(|e| e.to_string())?;

    let rows: Vec<(String, String)> = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .map_err(|e| e.to_string())?
        .filter_map(|r| r.ok())
        .collect();

    let mut scored: Vec<RankedChunk> = rows
        .iter()
        .map(|(content, file_path)| {
            let hits: usize = query_words
                .iter()
                .filter(|qw| content.to_lowercase().contains(&qw.to_lowercase()))
                .count();
            let score = if query_words.is_empty() {
                0.0
            } else {
                hits as f32 / query_words.len() as f32
            };
            RankedChunk {
                content: content.clone(),
                source: file_path.clone(),
                score,
            }
        })
        .filter(|rc| rc.score >= min_similarity)
        .collect();

    scored.sort_unstable_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    scored = deduplicate_chunks(&scored, dedup_threshold);
    scored.truncate(top_k);

    Ok(scored)
}
