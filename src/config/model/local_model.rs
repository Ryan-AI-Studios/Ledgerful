use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct LocalModelConfig {
    #[serde(default)]
    pub base_url: String,
    #[serde(default)]
    pub embedding_url: Option<String>,
    #[serde(default)]
    pub generation_url: Option<String>,
    #[serde(default)]
    pub ollama_cloud_url: Option<String>,
    /// Backward-compatible alias: `ollama_key`, `ollama_cloud_api_key`.
    #[serde(default, alias = "ollama_key")]
    pub ollama_cloud_api_key: Option<String>,
    #[serde(default)]
    pub ollama_cloud_model: Option<String>,
    #[serde(default)]
    pub embedding_model: String,
    #[serde(default)]
    pub generation_model: String,
    #[serde(default)]
    pub rerank_model: String,
    #[serde(default)]
    pub dimensions: usize,
    #[serde(default = "default_context_window_local")]
    pub context_window: usize,
    #[serde(default = "default_local_timeout")]
    pub timeout_secs: u64,
    #[serde(default)]
    pub prefer_local: bool,
    #[serde(default = "default_chunk_top_k")]
    pub chunk_top_k: usize,
    #[serde(default = "default_chunk_min_similarity")]
    pub chunk_min_similarity: f32,
    #[serde(default = "default_chunk_dedup_threshold")]
    pub chunk_dedup_threshold: f32,
    #[serde(default)]
    pub disable_hnsw: bool,
    /// Maximum number of threads used for parallel AST parsing + embedding (HP2).
    /// `None` means rayon's default (one thread per logical CPU).
    #[serde(default)]
    pub concurrency: Option<usize>,
}

fn default_context_window_local() -> usize {
    38000
}
fn default_local_timeout() -> u64 {
    300
}

fn default_chunk_top_k() -> usize {
    10
}
fn default_chunk_min_similarity() -> f32 {
    0.3
}
fn default_chunk_dedup_threshold() -> f32 {
    0.95
}

impl Default for LocalModelConfig {
    fn default() -> Self {
        Self {
            base_url: String::new(),
            embedding_url: None,
            generation_url: None,
            ollama_cloud_url: None,
            ollama_cloud_api_key: None,
            ollama_cloud_model: None,
            embedding_model: String::new(),
            generation_model: String::new(),
            rerank_model: String::new(),
            dimensions: 0,
            context_window: default_context_window_local(),
            timeout_secs: default_local_timeout(),
            prefer_local: false,
            chunk_top_k: default_chunk_top_k(),
            chunk_min_similarity: default_chunk_min_similarity(),
            chunk_dedup_threshold: default_chunk_dedup_threshold(),
            disable_hnsw: false,
            concurrency: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_local_model_config_defaults() {
        let config = LocalModelConfig::default();
        assert_eq!(config.base_url, "");
        assert_eq!(config.embedding_model, "");
        assert_eq!(config.generation_model, "");
        assert_eq!(config.rerank_model, "");
        assert_eq!(config.dimensions, 0);
        assert_eq!(config.context_window, 38000);
        assert_eq!(config.timeout_secs, 300);
        assert!(!config.prefer_local);
    }
}
