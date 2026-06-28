use crate::config::model::LocalModelConfig;
use std::time::Duration;

pub const MAX_BATCH_SIZE: usize = 8;

#[derive(Debug, Clone)]
pub struct Dimensions {
    pub dimensions: usize,
    pub model_name: String,
    pub active: bool,
}

impl Dimensions {
    pub fn new(dims: usize) -> Self {
        Self {
            dimensions: dims,
            model_name: String::new(),
            active: dims > 0,
        }
    }
}

pub fn check_local_model(config: &LocalModelConfig) -> Result<Dimensions, String> {
    if config.base_url.is_empty() && config.embedding_url.is_none() {
        return Ok(Dimensions {
            dimensions: 0,
            model_name: String::new(),
            active: false,
        });
    }

    let url = config.embedding_url.as_deref().unwrap_or(&config.base_url);

    // CR3: Increased from 150ms to 500ms to prevent false negatives on WSL/container hosts.
    if !crate::util::network::is_url_reachable(url, Duration::from_millis(500)) {
        return Err(format!(
            "Local embedding model server at {} is unreachable",
            url
        ));
    }

    let vectors = match embed_batch(url, &config.embedding_model, &["ping"], config.timeout_secs) {
        Ok(v) => v,
        Err(e) => {
            tracing::error!("Probe failed at {}: {}", url, e);
            return Err(e);
        }
    };

    let dims = vectors.first().map(|v| v.len()).unwrap_or(0);
    tracing::info!("Probe at {} returned {} dimensions", url, dims);

    Ok(Dimensions {
        dimensions: dims,
        model_name: config.embedding_model.clone(),
        active: dims > 0,
    })
}

pub fn embed_long_text(config: &LocalModelConfig, text: &str) -> Result<Vec<f32>, String> {
    // Cap chunk size to ~500 tokens (2000 chars) to avoid server batch limits.
    // Even if context_window is larger, many servers have a smaller physical batch size.
    let max_chars = std::cmp::min(config.context_window * 4, 2000);
    let overlap_chars = 256;

    let url = config.embedding_url.as_deref().unwrap_or(&config.base_url);

    if text.len() <= max_chars {
        let vectors = embed_batch(url, &config.embedding_model, &[text], config.timeout_secs)?;
        return vectors
            .into_iter()
            .next()
            .ok_or_else(|| "No embedding returned".to_string());
    }

    let mut chunks: Vec<&str> = Vec::new();
    let mut start = 0;
    while start < text.len() {
        let end = std::cmp::min(start + max_chars, text.len());
        chunks.push(&text[start..end]);
        if end == text.len() {
            break;
        }
        let step = if max_chars > overlap_chars {
            max_chars - overlap_chars
        } else {
            1
        };
        start += step;
    }

    let mut all_vectors: Vec<Vec<f32>> = Vec::new();
    for batch in chunks.chunks(MAX_BATCH_SIZE) {
        let chunk_refs: Vec<&str> = batch.to_vec();
        let batch_vectors = embed_batch(
            url,
            &config.embedding_model,
            &chunk_refs,
            config.timeout_secs,
        )?;
        all_vectors.extend(batch_vectors);
    }

    if all_vectors.is_empty() {
        return Err("No embeddings returned for chunks".to_string());
    }

    let dim = all_vectors[0].len();
    let mut pooled = vec![0.0_f32; dim];
    for v in &all_vectors {
        for (i, item) in pooled.iter_mut().enumerate().take(dim) {
            *item += v[i];
        }
    }
    for item in pooled.iter_mut().take(dim) {
        *item /= all_vectors.len() as f32;
    }

    Ok(pooled)
}

pub fn embed_batch(
    base_url: &str,
    model: &str,
    texts: &[&str],
    timeout_secs: u64,
) -> Result<Vec<Vec<f32>>, String> {
    if base_url.is_empty() {
        return Ok(Vec::new());
    }
    if texts.is_empty() {
        return Ok(Vec::new());
    }
    if texts.len() > MAX_BATCH_SIZE {
        return Err(format!(
            "Batch size {} exceeds maximum {MAX_BATCH_SIZE}",
            texts.len()
        ));
    }

    let url = format!("{base_url}/v1/embeddings");
    tracing::debug!("Using embedding URL: {}", url);

    // CR3: Fast network probe to prevent 20s TCP hangs when model server is down.
    if !crate::util::network::is_url_reachable(base_url, Duration::from_millis(500)) {
        return Err(format!(
            "Local embedding model server at {base_url} is unreachable"
        ));
    }

    let body = serde_json::json!({
        "model": model,
        "input": texts,
    });

    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(std::cmp::min(timeout_secs, 5)))
        .timeout_read(Duration::from_secs(timeout_secs))
        .timeout_write(Duration::from_secs(30))
        .build();

    let response = agent
        .post(&url)
        .set("Content-Type", "application/json")
        .send_json(&body)
        .map_err(|e| match e {
            ureq::Error::Status(code, response) => {
                let msg = response.into_string().unwrap_or_default();
                if code == 400 && msg.contains("pooling type none") {
                    "Embedding server returned 400: Model is using 'pooling type none'. \
                         To use embeddings, start llama-server with --embedding or --pooling flags."
                        .to_string()
                } else {
                    format!(
                        "Embedding server returned {code}: {}",
                        msg.chars().take(200).collect::<String>()
                    )
                }
            }
            ureq::Error::Transport(inner) => {
                format!("Embedding server unreachable: {inner}")
            }
        })?;

    let value: serde_json::Value = response
        .into_json()
        .map_err(|e| format!("Failed to parse embedding response: {e}"))?;

    let data = value["data"]
        .as_array()
        .ok_or_else(|| "Embedding response missing 'data' array".to_string())?;

    let mut results: Vec<Vec<f32>> = Vec::with_capacity(data.len());
    for item in data {
        let embedding = item["embedding"]
            .as_array()
            .ok_or_else(|| "Missing 'embedding' array in response data item".to_string())?;

        let floats: Vec<f32> = embedding
            .iter()
            .map(|v| {
                v.as_f64()
                    .ok_or_else(|| "Non-numeric value in embedding array".to_string())
                    .map(|f| f as f32)
            })
            .collect::<Result<Vec<f32>, String>>()?;

        results.push(floats);
    }

    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::model::LocalModelConfig;
    use httpmock::prelude::*;

    #[test]
    fn test_dimensions_new_active() {
        let d = Dimensions::new(768);
        assert_eq!(d.dimensions, 768);
        assert!(d.active);

        let d = Dimensions::new(0);
        assert_eq!(d.dimensions, 0);
        assert!(!d.active);
    }

    #[test]
    fn test_check_local_model_empty_url() {
        let config = LocalModelConfig::default();
        let result = check_local_model(&config).unwrap();
        assert!(!result.active);
        assert_eq!(result.dimensions, 0);
    }

    #[test]
    fn test_check_local_model_unreachable() {
        let config = LocalModelConfig {
            base_url: "http://127.0.0.1:1".to_string(),
            embedding_url: None,
            generation_url: None,
            embedding_model: "test-model".to_string(),
            timeout_secs: 1,
            ..LocalModelConfig::default()
        };
        let result = check_local_model(&config);
        assert!(result.is_err());
    }

    #[test]
    fn test_embed_long_text_short_delegates_to_batch() {
        let server = MockServer::start();

        server.mock(|when, then| {
            when.method(httpmock::Method::POST).path("/v1/embeddings");
            then.status(200)
                .header("Content-Type", "application/json")
                .json_body(serde_json::json!({
                    "data": [
                        {"embedding": [0.1, 0.2, 0.3]}
                    ]
                }));
        });

        let config = LocalModelConfig {
            base_url: server.base_url(),
            embedding_url: None,
            generation_url: None,
            embedding_model: "test-model".to_string(),
            context_window: 8192,
            timeout_secs: 30,
            ..LocalModelConfig::default()
        };

        let result = embed_long_text(&config, "hello world").unwrap();
        assert_eq!(result.len(), 3);
        assert!((result[0] - 0.1).abs() < 1e-6);
        assert!((result[1] - 0.2).abs() < 1e-6);
        assert!((result[2] - 0.3).abs() < 1e-6);
    }

    #[test]
    fn test_embed_long_text_long_text_chunked_and_pooled() {
        let server = MockServer::start();

        let mock = server.mock(|when, then| {
            when.method(httpmock::Method::POST).path("/v1/embeddings");
            then.status(200)
                .header("Content-Type", "application/json")
                .json_body(serde_json::json!({
                    "data": [
                        {"embedding": [1.0, 0.0]},
                        {"embedding": [0.0, 1.0]}
                    ]
                }));
        });

        // context_window=4 => max_chars=16. Text of 17 chars produces 2 chunks
        // (step=1 when overlap > max_chars). Both chunks fit in one batch.
        let config = LocalModelConfig {
            base_url: server.base_url(),
            embedding_url: None,
            generation_url: None,
            embedding_model: "test-model".to_string(),
            context_window: 4,
            timeout_secs: 30,
            ..LocalModelConfig::default()
        };

        let result = embed_long_text(&config, &"a".repeat(17)).unwrap();
        assert_eq!(result.len(), 2);
        // Mean-pool of [1.0, 0.0] and [0.0, 1.0] = [0.5, 0.5]
        assert!((result[0] - 0.5).abs() < 1e-6);
        assert!((result[1] - 0.5).abs() < 1e-6);
        mock.assert();
    }

    #[test]
    fn test_embed_batch_empty_url_returns_empty() {
        let result = embed_batch("", "model", &["hello"], 30).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_embed_batch_empty_texts_returns_empty() {
        let result = embed_batch("http://localhost:1234", "model", &[], 30).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_embed_batch_exceeds_max_batch() {
        let texts: Vec<String> = (0..=MAX_BATCH_SIZE).map(|i| format!("text{i}")).collect();
        let refs: Vec<&str> = texts.iter().map(|s| s.as_str()).collect();
        let result = embed_batch("http://localhost:1234", "model", &refs, 30);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("exceeds maximum"));
    }

    #[test]
    fn test_embed_batch_success() {
        let server = MockServer::start();

        let mock = server.mock(|when, then| {
            when.method(httpmock::Method::POST).path("/v1/embeddings");
            then.status(200)
                .header("Content-Type", "application/json")
                .json_body(serde_json::json!({
                    "data": [
                        {"embedding": [0.1, 0.2, 0.3]},
                        {"embedding": [0.4, 0.5, 0.6]}
                    ]
                }));
        });

        let result =
            embed_batch(&server.base_url(), "test-model", &["hello", "world"], 30).unwrap();
        mock.assert();

        assert_eq!(result.len(), 2);
        assert_eq!(result[0], vec![0.1_f32, 0.2, 0.3]);
        assert_eq!(result[1], vec![0.4_f32, 0.5, 0.6]);
    }

    #[test]
    fn test_embed_batch_server_error() {
        let server = MockServer::start();

        server.mock(|when, then| {
            when.method(httpmock::Method::POST).path("/v1/embeddings");
            then.status(503).body("Service Unavailable");
        });

        let result = embed_batch(&server.base_url(), "test-model", &["hello"], 30);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("503"));
    }

    #[test]
    fn test_embed_batch_connection_refused() {
        let result = embed_batch("http://127.0.0.1:1", "model", &["hello"], 1);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("unreachable"));
    }

    #[test]
    fn test_embed_batch_pooling_none_error() {
        let server = MockServer::start();

        server.mock(|when, then| {
            when.method(httpmock::Method::POST).path("/v1/embeddings");
            then.status(400).body("the served model is using pooling type none, which is not compatible with OpenAI embeddings.");
        });

        let result = embed_batch(&server.base_url(), "test-model", &["hello"], 30);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.contains("pooling type none"));
        assert!(err.contains("--embedding or --pooling"));
    }
}
