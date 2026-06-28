pub mod chunker;
pub mod concurrency;
pub mod embedder;
pub mod hotspots;
pub mod vector_store;

use crate::config::model::{LocalModelConfig, SemanticConfig};
use crate::semantic::chunker::AstChunker;
use crate::semantic::embedder::SemanticEmbedder;
use crate::semantic::vector_store::VectorStore;
use crate::state::storage_cozo::CozoStorage;
use miette::Result;
use std::path::Path;

use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct SemanticReadiness {
    pub endpoint_available: bool,
    pub model_name: String,
    pub dimensions: usize,
    pub vector_count: usize,
    pub is_stale: bool,
    pub dimension_mismatch: bool,
}

pub struct SemanticDiscovery<'a> {
    pub embedder: SemanticEmbedder,
    vector_store: VectorStore<'a>,
    config: LocalModelConfig,
}

impl<'a> SemanticDiscovery<'a> {
    pub fn new(config: LocalModelConfig, storage: &'a CozoStorage) -> Result<Self> {
        Self::new_with_semantic_config(config, SemanticConfig::default(), storage)
    }

    pub fn new_with_semantic_config(
        mut config: LocalModelConfig,
        semantic_config: SemanticConfig,
        storage: &'a CozoStorage,
    ) -> Result<Self> {
        if config.dimensions == 0 && !config.base_url.is_empty() {
            match crate::embed::client::check_local_model(&config) {
                Ok(dims) if dims.dimensions > 0 => {
                    tracing::info!(
                        "Probed local model: {} ({} dimensions)",
                        dims.model_name,
                        dims.dimensions
                    );
                    config.dimensions = dims.dimensions;
                }
                Err(e) => {
                    tracing::warn!(
                        "Failed to probe local model at {}: {}. Defaulting to 384.",
                        config.base_url,
                        e
                    );
                    config.dimensions = 384;
                }
                _ => {
                    tracing::warn!("Probed model returned zero dimensions. Defaulting to 384.");
                    config.dimensions = 384;
                }
            }
        } else if config.dimensions == 0 {
            config.dimensions = 384;
        }

        let dim = config.dimensions;
        let skip_hnsw = config.disable_hnsw;
        tracing::info!("Initializing VectorStore with {} dimensions", dim);
        let embedder = SemanticEmbedder::new(config.clone());
        let vector_store = VectorStore::new_with_hnsw_threshold(
            storage,
            dim,
            skip_hnsw,
            semantic_config.hnsw_rebuild_threshold(),
        )?;
        Ok(Self {
            embedder,
            vector_store,
            config,
        })
    }

    pub fn check_readiness(&self) -> Result<SemanticReadiness> {
        let probe = crate::embed::client::check_local_model(&self.config);
        let endpoint_available = probe.is_ok();
        let model_name = probe
            .as_ref()
            .map(|d| d.model_name.clone())
            .unwrap_or_else(|_| self.config.embedding_model.clone());
        let model_dims = probe.as_ref().map(|d| d.dimensions).unwrap_or(0);

        let vector_count = self.vector_store.get_vector_count().unwrap_or(0);

        // Check for dimension mismatch between model and store
        let dimension_mismatch = if model_dims > 0 && self.config.dimensions > 0 {
            model_dims != self.config.dimensions
        } else {
            false
        };

        Ok(SemanticReadiness {
            endpoint_available,
            model_name,
            dimensions: self.config.dimensions,
            vector_count,
            is_stale: false, // Stale check handled at command level
            dimension_mismatch,
        })
    }

    pub fn index_file(&self, path: &Path, content: &str) -> Result<()> {
        let (chunks, embeddings) = self.process_file(path, content)?;
        if !chunks.is_empty() {
            self.vector_store.index_chunks(chunks, embeddings)?;
        }
        Ok(())
    }

    pub fn process_file(
        &self,
        path: &Path,
        content: &str,
    ) -> Result<(Vec<crate::semantic::chunker::AstChunk>, Vec<Vec<f32>>)> {
        let chunks = AstChunker::chunk_file(path, content)?;
        if chunks.is_empty() {
            return Ok((vec![], vec![]));
        }

        let texts: Vec<String> = chunks.iter().map(|c| c.to_embedding_text()).collect();
        let text_refs: Vec<&str> = texts.iter().map(|s| s.as_str()).collect();

        tracing::debug!("Embedding {} chunks for {}", chunks.len(), path.display());
        let embeddings = self.embedder.embed_batch(&text_refs)?;

        if !embeddings.is_empty() {
            tracing::info!(
                "Received {} embeddings of dimension {}",
                embeddings.len(),
                embeddings[0].len()
            );
        }

        // Verify we got non-zero embeddings
        let zero_count = embeddings
            .iter()
            .filter(|v| v.iter().all(|&x| x == 0.0))
            .count();
        if zero_count > 0 {
            tracing::warn!(
                "Found {} zero-magnitude embeddings for {}",
                zero_count,
                path.display()
            );
        }

        Ok((chunks, embeddings))
    }

    pub fn index_chunks_batched(
        &self,
        chunks: Vec<crate::semantic::chunker::AstChunk>,
        embeddings: Vec<Vec<f32>>,
    ) -> Result<()> {
        self.vector_store.index_chunks(chunks, embeddings)
    }

    pub fn query(&self, query_text: &str, k: usize) -> Result<Vec<(String, String, usize, f32)>> {
        let query_vector = self.embedder.embed(query_text)?;
        self.vector_store.query(query_vector, k)
    }

    pub fn query_raw(
        &self,
        query_vector: Vec<f32>,
        k: usize,
    ) -> Result<Vec<(String, String, usize, f32)>> {
        self.vector_store.query(query_vector, k)
    }

    pub fn get_vector_count(&self) -> Result<usize> {
        self.vector_store.get_vector_count()
    }

    pub fn remove_file_snippets(&self, file_path: &str) -> Result<()> {
        self.vector_store.remove_file_snippets(file_path)
    }

    // ── HP3: File-hash tracking for incremental semantic index ──────────────

    /// Ensure the `semantic_file_hash` relation exists in CozoDB.
    pub fn ensure_file_hash_schema(&self) -> Result<()> {
        let relations = self.vector_store.storage_ref().get_relations()?;
        if !relations.contains(&"semantic_file_hash".to_string()) {
            self.vector_store
                .storage_ref()
                .run_script(":create semantic_file_hash {file_path => content_hash: String}")?;
            tracing::info!("Created semantic_file_hash relation for incremental tracking");
        }
        Ok(())
    }

    /// Returns `true` if the stored hash for `path` matches `hash` (file unchanged).
    pub fn is_file_hash_current(&self, path: &std::path::Path, hash: &str) -> bool {
        let path_str = path.to_string_lossy().replace('\\', "/");
        let script = format!(
            "?[content_hash] := *semantic_file_hash{{file_path: \"{}\", content_hash}}",
            path_str.replace('"', "\\\"")
        );
        match self.vector_store.storage_ref().run_script(&script) {
            Ok(res) => {
                if let Some(row) = res.rows.first()
                    && let Some(cozo::DataValue::Str(stored)) = row.first()
                {
                    return stored.as_str() == hash;
                }
                false
            }
            Err(_) => false,
        }
    }

    /// Upsert the content hash for `path` into `semantic_file_hash`.
    pub fn record_file_hash(&self, path: &std::path::Path, hash: &str) -> Result<()> {
        use cozo::{DataValue, ScriptMutability};
        use std::collections::BTreeMap;

        let path_str = path.to_string_lossy().replace('\\', "/");
        let mut params = BTreeMap::new();
        params.insert(
            "data".to_string(),
            DataValue::from(vec![DataValue::from(vec![
                DataValue::from(path_str.as_str()),
                DataValue::from(hash),
            ])]),
        );
        self.vector_store.storage_ref().run_script_with_params(
            "?[file_path, content_hash] <- $data :put semantic_file_hash",
            params,
            ScriptMutability::Mutable,
        )?;
        Ok(())
    }

    /// Remove snippet embeddings for files that no longer exist under `repo_root`.
    /// Called before a full re-index to keep the vector store clean (HP3 pruning).
    pub fn prune_deleted_snippets(&self, repo_root: &std::path::Path) -> Result<()> {
        // Fetch all indexed file paths
        let script = "?[file_path] := *snippet_embedding{file_path}";
        let res = self.vector_store.storage_ref().run_script(script);
        let res = match res {
            Ok(r) => r,
            Err(_) => return Ok(()), // relation may not exist yet
        };

        let mut pruned = 0usize;
        for row in res.rows {
            if let Some(cozo::DataValue::Str(fp)) = row.first() {
                let full = repo_root.join(fp.as_str().trim_start_matches('/'));
                if !full.exists() {
                    self.vector_store.remove_file_snippets(fp.as_ref())?;
                    pruned += 1;
                }
            }
        }
        if pruned > 0 {
            tracing::info!("Pruned snippets for {} deleted files", pruned);
        }
        Ok(())
    }

    /// Retrieve all file paths currently tracked in `semantic_file_hash`.
    pub fn get_tracked_files(&self) -> Result<Vec<String>> {
        let script = "?[file_path] := *semantic_file_hash{file_path}";
        let res = self.vector_store.storage_ref().run_script(script);
        let res = match res {
            Ok(r) => r,
            Err(_) => return Ok(vec![]), // relation may not exist yet
        };
        let mut files = Vec::new();
        for row in res.rows {
            if let Some(cozo::DataValue::Str(fp)) = row.first() {
                files.push(fp.to_string());
            }
        }
        Ok(files)
    }

    /// Remove the content hash for `file_path` from `semantic_file_hash`.
    pub fn remove_file_hash(&self, file_path: &str) -> Result<()> {
        let path_normalized = file_path.replace('\\', "/");
        // Cozo escapes single quotes by doubling them, not with backslash.
        let escaped = path_normalized.replace('\'', "''");
        let script = format!(
            "paths[file_path] <- [['{}']]\n\
             ?[file_path, content_hash] := paths[file_path], *semantic_file_hash{{file_path, content_hash}}\n\
             :rm semantic_file_hash {{file_path, content_hash}}",
            escaped
        );
        self.vector_store.storage_ref().run_script(&script)?;
        Ok(())
    }
}
