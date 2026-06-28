use serde::{Deserialize, Serialize};

pub const DEFAULT_HNSW_REBUILD_THRESHOLD: usize = 500;

fn default_hnsw_rebuild_threshold() -> Option<usize> {
    Some(DEFAULT_HNSW_REBUILD_THRESHOLD)
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SemanticConfig {
    /// Minimum ingestion batch size that triggers drop/rebuild of the HNSW index.
    /// None means use the built-in default.
    #[serde(default = "default_hnsw_rebuild_threshold")]
    pub hnsw_rebuild_threshold: Option<usize>,
    /// Legacy combined field. If set and the split fields are not, populates both.
    #[serde(default)]
    pub concurrency: Option<usize>,
    /// Threads for CPU-bound AST parsing (rayon pool size).
    #[serde(default)]
    pub parse_concurrency: Option<usize>,
    /// Cap on concurrent embed requests in flight.
    #[serde(default)]
    pub embed_concurrency: Option<usize>,
    /// Safety ceiling on concurrent embed requests.
    #[serde(default)]
    pub embed_concurrency_cap: Option<usize>,
}

impl Default for SemanticConfig {
    fn default() -> Self {
        Self {
            hnsw_rebuild_threshold: default_hnsw_rebuild_threshold(),
            concurrency: None,
            parse_concurrency: None,
            embed_concurrency: None,
            embed_concurrency_cap: None,
        }
    }
}

impl SemanticConfig {
    pub fn hnsw_rebuild_threshold(&self) -> usize {
        self.hnsw_rebuild_threshold
            .unwrap_or(DEFAULT_HNSW_REBUILD_THRESHOLD)
    }

    /// Resolved concurrency for semantic indexing. Returns `Some(n)` only when
    /// the user has explicitly set a non-zero value in TOML; `None` triggers
    /// auto-tuning in the call site.
    pub fn semantic_concurrency(&self) -> Option<usize> {
        self.concurrency.filter(|n| *n > 0)
    }

    pub fn semantic_parse_concurrency(&self) -> Option<usize> {
        self.parse_concurrency.filter(|n| *n > 0)
    }

    pub fn semantic_embed_concurrency(&self) -> Option<usize> {
        self.embed_concurrency.filter(|n| *n > 0)
    }

    pub fn semantic_embed_concurrency_cap(&self) -> Option<usize> {
        self.embed_concurrency_cap.filter(|n| *n > 0)
    }
}
