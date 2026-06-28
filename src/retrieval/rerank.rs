use crate::retrieval::query::RetrievedChunk;
use serde::Deserialize;
use std::time::Duration;

#[derive(Debug, Deserialize)]
struct RerankResponse {
    results: Vec<RerankResult>,
}

#[derive(Debug, Deserialize)]
struct RerankResult {
    index: usize,
    relevance_score: f32,
}

pub fn rerank(
    base_url: &str,
    model: &str,
    query: &str,
    mut chunks: Vec<RetrievedChunk>,
    timeout_secs: u64,
) -> Vec<RetrievedChunk> {
    if model.is_empty() {
        return chunks;
    }
    if chunks.is_empty() {
        return chunks;
    }

    let url = format!("{base_url}/v1/rerank");

    let documents: Vec<&str> = chunks.iter().map(|c| c.content.as_str()).collect();

    let body = serde_json::json!({
        "model": model,
        "query": query,
        "documents": documents,
    });

    let agent = ureq::AgentBuilder::new()
        .timeout_read(Duration::from_secs(timeout_secs))
        .timeout_write(Duration::from_secs(30))
        .build();

    let response = match agent
        .post(&url)
        .set("Content-Type", "application/json")
        .send_json(&body)
    {
        Ok(resp) => resp,
        Err(e) => {
            let msg = match e {
                ureq::Error::Status(code, response) => {
                    let body = response.into_string().unwrap_or_default();
                    format!(
                        "Reranker returned {code}: {}",
                        body.chars().take(200).collect::<String>()
                    )
                }
                ureq::Error::Transport(inner) => {
                    format!("Reranker server unreachable: {inner}")
                }
            };
            tracing::warn!("Reranker call failed: {msg}");
            return chunks;
        }
    };

    let parsed: RerankResponse = match response.into_json() {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!("Failed to parse reranker response: {e}");
            return chunks;
        }
    };

    // Build index -> relevance_score map
    let mut score_map: std::collections::HashMap<usize, f32> = std::collections::HashMap::new();
    for result in &parsed.results {
        score_map.insert(result.index, result.relevance_score);
    }

    // Assign rerank scores, falling back to existing similarity if not in result
    for (i, chunk) in chunks.iter_mut().enumerate() {
        if let Some(&score) = score_map.get(&i) {
            chunk.similarity = score;
        }
    }

    // Sort descending by new similarity
    chunks.sort_unstable_by(|a, b| {
        b.similarity
            .partial_cmp(&a.similarity)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.entity_id.cmp(&b.entity_id))
    });

    chunks
}

#[cfg(test)]
mod tests {
    use super::*;
    use httpmock::prelude::*;

    fn make_chunks() -> Vec<RetrievedChunk> {
        vec![
            RetrievedChunk {
                entity_id: "a::0".to_string(),
                similarity: 0.8,
                content: "Content A".to_string(),
                heading: Some("A".to_string()),
                file_path: "docs/a.md".to_string(),
            },
            RetrievedChunk {
                entity_id: "b::0".to_string(),
                similarity: 0.6,
                content: "Content B".to_string(),
                heading: Some("B".to_string()),
                file_path: "docs/b.md".to_string(),
            },
            RetrievedChunk {
                entity_id: "c::0".to_string(),
                similarity: 0.4,
                content: "Content C".to_string(),
                heading: Some("C".to_string()),
                file_path: "docs/c.md".to_string(),
            },
        ]
    }

    #[test]
    fn rerank_empty_model_returns_original() {
        let chunks = make_chunks();
        let result = rerank("http://localhost:1234", "", "query", chunks.clone(), 10);
        assert_eq!(result, chunks);
    }

    #[test]
    fn rerank_empty_chunks_returns_empty() {
        let result = rerank("http://localhost:1234", "model", "query", vec![], 10);
        assert!(result.is_empty());
    }

    #[test]
    fn rerank_reorders_by_server_scores() {
        let server = MockServer::start();

        // Server returns reversed scores: c(0.99) > b(0.5) > a(0.1)
        server.mock(|when, then| {
            when.method(httpmock::Method::POST).path("/v1/rerank");
            then.status(200)
                .header("Content-Type", "application/json")
                .json_body(serde_json::json!({
                    "results": [
                        {"index": 0, "relevance_score": 0.1},
                        {"index": 1, "relevance_score": 0.5},
                        {"index": 2, "relevance_score": 0.99}
                    ]
                }));
        });

        let chunks = make_chunks();
        let result = rerank(&server.base_url(), "test-reranker", "query", chunks, 10);

        assert_eq!(result.len(), 3);
        // Reordered: c (0.99) first, then b (0.5), then a (0.1)
        assert_eq!(result[0].entity_id, "c::0");
        assert!((result[0].similarity - 0.99).abs() < 1e-6);
        assert_eq!(result[1].entity_id, "b::0");
        assert!((result[1].similarity - 0.5).abs() < 1e-6);
        assert_eq!(result[2].entity_id, "a::0");
        assert!((result[2].similarity - 0.1).abs() < 1e-6);
    }

    #[test]
    fn rerank_server_unreachable_returns_original() {
        let chunks = make_chunks();
        let result = rerank("http://127.0.0.1:1", "model", "query", chunks.clone(), 1);
        // Should return original chunks unmodified (cosine-scored fallback)
        assert_eq!(result.len(), 3);
        assert_eq!(result, chunks);
    }

    #[test]
    fn rerank_server_error_returns_original() {
        let server = MockServer::start();

        server.mock(|when, then| {
            when.method(httpmock::Method::POST).path("/v1/rerank");
            then.status(503).body("Service Unavailable");
        });

        let chunks = make_chunks();
        let result = rerank(&server.base_url(), "model", "query", chunks.clone(), 10);
        assert_eq!(result, chunks);
    }

    #[test]
    fn rerank_missing_indices_fall_back_to_cosine() {
        let server = MockServer::start();

        // Only returns scores for indices 0 and 2, missing index 1
        server.mock(|when, then| {
            when.method(httpmock::Method::POST).path("/v1/rerank");
            then.status(200)
                .header("Content-Type", "application/json")
                .json_body(serde_json::json!({
                    "results": [
                        {"index": 0, "relevance_score": 0.95},
                        {"index": 2, "relevance_score": 0.7}
                    ]
                }));
        });

        let chunks = make_chunks();
        let result = rerank(&server.base_url(), "model", "query", chunks, 10);

        assert_eq!(result.len(), 3);
        // Index 0 keeps 0.95, index 2 keeps 0.7, index 1 keeps original 0.6
        // Sorted: 0.95 (a) > 0.7 (c) > 0.6 (b)
        assert_eq!(result[0].entity_id, "a::0");
        assert_eq!(result[1].entity_id, "c::0");
        assert_eq!(result[2].entity_id, "b::0");
    }
}
