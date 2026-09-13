use crate::config::model::LocalModelConfig;
use crate::embed::client::embed_long_text;
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
