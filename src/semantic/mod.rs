pub mod chunker;
pub mod concurrency;
pub mod embedder;
pub mod hotspots;
pub mod vector_store;

use crate::config::model::{LocalModelConfig, SemanticConfig};
use crate::embed::client::is_embedding_backend_configured;
use crate::semantic::chunker::AstChunker;
use crate::semantic::embedder::SemanticEmbedder;
use crate::semantic::vector_store::VectorStore;
use crate::state::storage_cozo::CozoStorage;
use miette::Result;
use std::path::Path;

use serde::Serialize;

/// Orthogonal backend axis for semantic readiness (DoD-3).
/// Kept separate from `vector_count` so Ready+empty is expressible.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BackendStatus {
    NotConfigured,
    Unreachable,
    Ready,
}

#[derive(Debug, Serialize)]
pub struct SemanticReadiness {
    /// Backend health axis (replaces collapsed `endpoint_available: bool`).
    pub backend_status: BackendStatus,
    pub model_name: String,
    pub dimensions: usize,
    pub vector_count: usize,
    /// Pre-existing all-zero / zero-length embedding rows (detect, never delete).
    pub zero_vector_count: usize,
    pub is_stale: bool,
    pub dimension_mismatch: bool,
}

/// Map a probe result + config into the backend status axis.
/// Shared so search and doctor cannot re-derive a third gate (spec §2.3).
pub fn backend_status_from_probe(
    config: &LocalModelConfig,
    probe: &std::result::Result<crate::embed::client::Dimensions, String>,
) -> BackendStatus {
    if !is_embedding_backend_configured(config) {
        return BackendStatus::NotConfigured;
    }
    match probe {
        Ok(dims) if dims.active => BackendStatus::Ready,
        Ok(_) => BackendStatus::NotConfigured,
        Err(_) => BackendStatus::Unreachable,
    }
}

/// User-facing readiness messages for `search --semantic` (DoD-4).
/// Distinct wording per state; never recommends `index --semantic` when
/// the backend is not configured.
pub fn semantic_readiness_messages(readiness: &SemanticReadiness) -> Vec<String> {
    let mut msgs = Vec::new();
    match readiness.backend_status {
        BackendStatus::NotConfigured => {
            msgs.push(
                "Embedding backend not configured. Set `local_model.base_url` (or \
                 `local_model.embedding_url`) to enable semantic search. Inspect with \
                 `ledgerful index --semantic-dry-run`."
                    .to_string(),
            );
        }
        BackendStatus::Unreachable => {
            msgs.push("Local embedding endpoint unreachable. Check your model server.".to_string());
        }
        BackendStatus::Ready if readiness.vector_count == 0 => {
            msgs.push(
                "Semantic index is empty. Run `ledgerful index --semantic` to populate."
                    .to_string(),
            );
        }
        BackendStatus::Ready => {}
    }
    if readiness.dimension_mismatch {
        msgs.push(format!(
            "Model/Index dimension mismatch ({} vs {}). Run `ledgerful update --migrate` to fix.",
            readiness.model_name, readiness.dimensions
        ));
    }
    if readiness.zero_vector_count > 0 {
        // Gate index --semantic remediation on Ready only. Under
        // NotConfigured / Unreachable, naming index alone reintroduces the
        // forbidden remedy (backend must be healthy before re-index helps).
        match readiness.backend_status {
            BackendStatus::Ready => {
                msgs.push(format!(
                    "{} zero-magnitude embedding row(s) detected in the vector store. \
                     Re-run `ledgerful index --semantic` to replace them \
                     (rows are not auto-deleted).",
                    readiness.zero_vector_count
                ));
            }
            BackendStatus::NotConfigured => {
                msgs.push(format!(
                    "{} zero-magnitude embedding row(s) detected in the vector store. \
                     Configure an embedding backend first (inspect with \
                     `ledgerful index --semantic-dry-run`); re-index only after the \
                     backend is ready (rows are not auto-deleted).",
                    readiness.zero_vector_count
                ));
            }
            BackendStatus::Unreachable => {
                msgs.push(format!(
                    "{} zero-magnitude embedding row(s) detected in the vector store. \
                     Restore the embedding endpoint, then re-index once it is reachable \
                     (rows are not auto-deleted).",
                    readiness.zero_vector_count
                ));
            }
        }
    }
    msgs
}

/// Fallback copy when semantic query **succeeded** with zero hits (DoD-4 / DoD-8).
/// Distinguishes "did not run usefully" from "ran, no matches".
///
/// Never use this for `query` `Err` under `BackendStatus::Ready` — that path
/// must use [`semantic_query_failure_message`] so a transient embed/store
/// failure is not claimed as "no matches".
pub fn semantic_empty_result_message(readiness: &SemanticReadiness) -> String {
    match readiness.backend_status {
        BackendStatus::NotConfigured => {
            "Semantic search did not run: embedding backend not configured. \
             Set `local_model.base_url` (or `local_model.embedding_url`). \
             Showing BM25 results."
                .to_string()
        }
        BackendStatus::Unreachable => {
            "Semantic search did not run: embedding endpoint unreachable. \
             Check your model server. Showing BM25 results."
                .to_string()
        }
        BackendStatus::Ready if readiness.vector_count == 0 => {
            "Semantic index empty. Showing BM25 results. \
             Run `ledgerful index --semantic` to populate."
                .to_string()
        }
        BackendStatus::Ready => {
            "No relevant code snippets found semantically. Showing BM25 results.".to_string()
        }
    }
}

/// User-facing copy when semantic `query` returned `Err` (DoD-4 / DoD-8).
///
/// - **NotConfigured / Unreachable**: reuse "did not run" config wording (backend
///   was never usable; error is secondary).
/// - **Ready**: must say semantic search **failed** (include a short error),
///   never "no matches" / "no relevant". Mentions BM25 fallback when falling through.
pub fn semantic_query_failure_message(
    readiness: &SemanticReadiness,
    error: &dyn std::fmt::Display,
) -> String {
    match readiness.backend_status {
        BackendStatus::NotConfigured => {
            "Semantic search did not run: embedding backend not configured. \
             Set `local_model.base_url` (or `local_model.embedding_url`). \
             Showing BM25 results."
                .to_string()
        }
        BackendStatus::Unreachable => {
            "Semantic search did not run: embedding endpoint unreachable. \
             Check your model server. Showing BM25 results."
                .to_string()
        }
        BackendStatus::Ready => {
            let detail = error.to_string();
            let first_line = detail
                .lines()
                .next()
                .filter(|l| !l.is_empty())
                .unwrap_or("unknown error");
            format!("Semantic search failed: {first_line}. Falling back to BM25 results.")
        }
    }
}

/// Pure decision for post-query messaging when there are no hits to display.
///
/// - `query_succeeded == true` → successful empty (use empty-result copy)
/// - `query_succeeded == false` → query/embed failed (use failure copy; never
///   Ready "no matches")
///
/// Extracted so command-level honesty is unit-testable without driving full CLI I/O.
pub fn semantic_no_results_message(
    readiness: &SemanticReadiness,
    query_succeeded: bool,
    error: Option<&str>,
) -> String {
    if query_succeeded {
        semantic_empty_result_message(readiness)
    } else {
        semantic_query_failure_message(readiness, &error.unwrap_or("unknown error"))
    }
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
        let backend_status = backend_status_from_probe(&self.config, &probe);
        let model_name = probe
            .as_ref()
            .map(|d| {
                if d.model_name.is_empty() {
                    self.config.embedding_model.clone()
                } else {
                    d.model_name.clone()
                }
            })
            .unwrap_or_else(|_| self.config.embedding_model.clone());
        let model_dims = probe.as_ref().map(|d| d.dimensions).unwrap_or(0);

        let vector_count = self.vector_store.get_vector_count().unwrap_or(0);
        let zero_vector_count = self.vector_store.count_zero_vectors().unwrap_or(0);

        // Check for dimension mismatch between model and store
        let dimension_mismatch = if model_dims > 0 && self.config.dimensions > 0 {
            model_dims != self.config.dimensions
        } else {
            false
        };

        Ok(SemanticReadiness {
            backend_status,
            model_name,
            dimensions: self.config.dimensions,
            vector_count,
            zero_vector_count,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::model::LocalModelConfig;

    #[test]
    fn backend_status_not_configured_when_url_empty() {
        let config = LocalModelConfig::default();
        let probe = Ok(crate::embed::client::Dimensions {
            dimensions: 0,
            model_name: String::new(),
            active: false,
        });
        assert_eq!(
            backend_status_from_probe(&config, &probe),
            BackendStatus::NotConfigured
        );
    }

    #[test]
    fn backend_status_unreachable_on_probe_err() {
        let config = LocalModelConfig {
            base_url: "http://127.0.0.1:9".to_string(),
            embedding_model: "test".to_string(),
            ..Default::default()
        };
        let probe = Err("unreachable".to_string());
        assert_eq!(
            backend_status_from_probe(&config, &probe),
            BackendStatus::Unreachable
        );
    }

    #[test]
    fn backend_status_ready_when_active() {
        let config = LocalModelConfig {
            base_url: "http://127.0.0.1:8083".to_string(),
            embedding_model: "nomic".to_string(),
            dimensions: 768,
            ..Default::default()
        };
        let probe = Ok(crate::embed::client::Dimensions {
            dimensions: 768,
            model_name: "nomic".to_string(),
            active: true,
        });
        assert_eq!(
            backend_status_from_probe(&config, &probe),
            BackendStatus::Ready
        );
    }

    /// DoD-3: Ready + empty index is expressible (orthogonal axes).
    #[test]
    fn readiness_ready_and_empty_is_expressible() {
        let storage = CozoStorage::new_in_memory().expect("cozo");
        let config = LocalModelConfig {
            // No URL → NotConfigured for real probe; we assert the struct shape
            // by constructing SemanticReadiness directly for the Ready+empty case.
            dimensions: 3,
            disable_hnsw: true,
            ..Default::default()
        };
        let semantic = SemanticDiscovery::new(config, &storage).expect("semantic");
        let readiness = semantic.check_readiness().expect("readiness");
        // Default install: NotConfigured + vector_count == 0
        assert_eq!(readiness.backend_status, BackendStatus::NotConfigured);
        assert_eq!(readiness.vector_count, 0);

        // The combination the flat enum could not express:
        let ready_empty = SemanticReadiness {
            backend_status: BackendStatus::Ready,
            model_name: "nomic".to_string(),
            dimensions: 768,
            vector_count: 0,
            zero_vector_count: 0,
            is_stale: false,
            dimension_mismatch: false,
        };
        assert_eq!(ready_empty.backend_status, BackendStatus::Ready);
        assert_eq!(ready_empty.vector_count, 0);
        let msgs = semantic_readiness_messages(&ready_empty);
        assert!(
            msgs.iter().any(|m| m.contains("index --semantic")),
            "Ready+empty must recommend index --semantic: {msgs:?}"
        );
    }

    /// DoD-4: NotConfigured message never suggests index --semantic.
    #[test]
    fn readiness_messages_not_configured_never_suggests_index_semantic() {
        let readiness = SemanticReadiness {
            backend_status: BackendStatus::NotConfigured,
            model_name: String::new(),
            dimensions: 0,
            vector_count: 0,
            zero_vector_count: 0,
            is_stale: false,
            dimension_mismatch: false,
        };
        let msgs = semantic_readiness_messages(&readiness);
        assert!(!msgs.is_empty(), "NotConfigured must emit a message");
        for msg in &msgs {
            assert!(
                !recommends_semantic_index(msg),
                "must never recommend index --semantic when unconfigured: {msg}"
            );
            assert!(
                msg.contains("semantic-dry-run") || msg.contains("base_url"),
                "must name config key or dry-run: {msg}"
            );
        }
        let empty = semantic_empty_result_message(&readiness);
        assert!(
            !recommends_semantic_index(&empty),
            "empty-result path must not recommend index --semantic: {empty}"
        );
        assert!(
            empty.contains("did not run"),
            "must distinguish absence of run: {empty}"
        );
    }

    /// DoD-4: Unreachable message.
    #[test]
    fn readiness_messages_unreachable() {
        let readiness = SemanticReadiness {
            backend_status: BackendStatus::Unreachable,
            model_name: "nomic".to_string(),
            dimensions: 768,
            vector_count: 0,
            zero_vector_count: 0,
            is_stale: false,
            dimension_mismatch: false,
        };
        let msgs = semantic_readiness_messages(&readiness);
        assert!(
            msgs.iter()
                .any(|m| m.contains("unreachable") || m.contains("model server")),
            "Unreachable must mention server: {msgs:?}"
        );
        assert!(
            msgs.iter().all(|m| !recommends_semantic_index(m)),
            "Unreachable must not recommend index when endpoint is down: {msgs:?}"
        );
    }

    /// True if message recommends *populating* via `index --semantic`.
    /// Mentions of `--semantic-dry-run` alone do not count.
    fn recommends_semantic_index(msg: &str) -> bool {
        // Strip dry-run mentions so substring checks don't false-positive.
        let scrubbed = msg
            .replace("semantic-dry-run", "")
            .replace("semantic_dry_run", "");
        scrubbed.contains("index --semantic")
    }

    /// DoD-4: dimension_mismatch message.
    #[test]
    fn readiness_messages_dimension_mismatch() {
        let readiness = SemanticReadiness {
            backend_status: BackendStatus::Ready,
            model_name: "nomic".to_string(),
            dimensions: 384,
            vector_count: 10,
            zero_vector_count: 0,
            is_stale: false,
            dimension_mismatch: true,
        };
        let msgs = semantic_readiness_messages(&readiness);
        assert!(
            msgs.iter()
                .any(|m| m.contains("dimension mismatch") || m.contains("Dimension")),
            "must report dimension mismatch: {msgs:?}"
        );
        assert!(
            msgs.iter().any(|m| m.contains("update --migrate")),
            "must name migrate remediation: {msgs:?}"
        );
    }

    /// DoD-4: Ready + non-empty has no "empty index" warning.
    #[test]
    fn readiness_messages_ready_populated_is_quiet() {
        let readiness = SemanticReadiness {
            backend_status: BackendStatus::Ready,
            model_name: "nomic".to_string(),
            dimensions: 768,
            vector_count: 42,
            zero_vector_count: 0,
            is_stale: false,
            dimension_mismatch: false,
        };
        let msgs = semantic_readiness_messages(&readiness);
        assert!(
            msgs.is_empty(),
            "healthy ready+populated must be silent: {msgs:?}"
        );
        let empty = semantic_empty_result_message(&readiness);
        assert!(
            empty.contains("No relevant") || empty.contains("no relevant"),
            "must distinguish ran-but-no-matches: {empty}"
        );
        assert!(
            !empty.contains("did not run"),
            "populated ready must not claim did-not-run: {empty}"
        );
    }

    /// DoD-7: zero_vector_count surfaces a named remediation without auto-delete.
    #[test]
    fn readiness_messages_zero_vector_report() {
        let readiness = SemanticReadiness {
            backend_status: BackendStatus::Ready,
            model_name: "nomic".to_string(),
            dimensions: 768,
            vector_count: 10,
            zero_vector_count: 3,
            is_stale: false,
            dimension_mismatch: false,
        };
        let msgs = semantic_readiness_messages(&readiness);
        assert!(
            msgs.iter()
                .any(|m| m.contains("3") && m.contains("zero-magnitude")),
            "must report zero-vector count: {msgs:?}"
        );
        assert!(
            msgs.iter().any(|m| m.contains("not auto-deleted")),
            "must state non-deletion: {msgs:?}"
        );
        assert!(
            msgs.iter().any(|m| recommends_semantic_index(m)),
            "Ready + zeros may recommend index --semantic: {msgs:?}"
        );
    }

    /// NotConfigured + zero rows must not recommend bare `index --semantic`.
    #[test]
    fn readiness_messages_zero_vector_not_configured_no_index_remedy() {
        let readiness = SemanticReadiness {
            backend_status: BackendStatus::NotConfigured,
            model_name: String::new(),
            dimensions: 0,
            vector_count: 5,
            zero_vector_count: 2,
            is_stale: false,
            dimension_mismatch: false,
        };
        let msgs = semantic_readiness_messages(&readiness);
        assert!(
            msgs.iter()
                .any(|m| m.contains("2") && m.contains("zero-magnitude")),
            "must report zero-vector count: {msgs:?}"
        );
        assert!(
            msgs.iter().all(|m| !recommends_semantic_index(m)),
            "NotConfigured + zeros must not recommend index --semantic: {msgs:?}"
        );
        assert!(
            msgs.iter()
                .any(|m| m.contains("semantic-dry-run") || m.contains("base_url")),
            "must point at config/dry-run first: {msgs:?}"
        );
    }

    /// Unreachable + zero rows must not recommend bare `index --semantic`.
    #[test]
    fn readiness_messages_zero_vector_unreachable_no_index_remedy() {
        let readiness = SemanticReadiness {
            backend_status: BackendStatus::Unreachable,
            model_name: "nomic".to_string(),
            dimensions: 768,
            vector_count: 5,
            zero_vector_count: 1,
            is_stale: false,
            dimension_mismatch: false,
        };
        let msgs = semantic_readiness_messages(&readiness);
        assert!(
            msgs.iter().any(|m| m.contains("zero-magnitude")),
            "must report zero-vector count: {msgs:?}"
        );
        assert!(
            msgs.iter().all(|m| !recommends_semantic_index(m)),
            "Unreachable + zeros must not recommend index --semantic: {msgs:?}"
        );
    }

    fn ready_populated() -> SemanticReadiness {
        SemanticReadiness {
            backend_status: BackendStatus::Ready,
            model_name: "nomic".to_string(),
            dimensions: 768,
            vector_count: 42,
            zero_vector_count: 0,
            is_stale: false,
            dimension_mismatch: false,
        }
    }

    /// Codex R1 / DoD-4: Ready + query Err must never use "no matches" empty copy.
    #[test]
    fn query_failure_message_ready_says_failed_not_no_matches() {
        let readiness = ready_populated();
        let msg = semantic_query_failure_message(&readiness, &"embed HTTP 503");
        assert!(
            msg.to_lowercase().contains("fail"),
            "Ready failure must say failed: {msg}"
        );
        assert!(
            !msg.to_lowercase().contains("no relevant"),
            "Ready failure must not claim no matches: {msg}"
        );
        assert!(
            !msg.contains("did not run"),
            "Ready failure is not a config skip: {msg}"
        );
        assert!(
            msg.contains("embed HTTP 503"),
            "must include short error detail: {msg}"
        );
        assert!(msg.contains("BM25"), "must mention BM25 fallback: {msg}");
    }

    /// NotConfigured query Err reuses did-not-run wording (config-related).
    #[test]
    fn query_failure_message_not_configured_did_not_run() {
        let readiness = SemanticReadiness {
            backend_status: BackendStatus::NotConfigured,
            model_name: String::new(),
            dimensions: 0,
            vector_count: 0,
            zero_vector_count: 0,
            is_stale: false,
            dimension_mismatch: false,
        };
        let msg = semantic_query_failure_message(&readiness, &"embedding backend not configured");
        assert!(
            msg.contains("did not run"),
            "NotConfigured Err reuses did-not-run: {msg}"
        );
        assert!(
            !msg.to_lowercase().contains("no relevant"),
            "must not claim no matches: {msg}"
        );
        assert!(!recommends_semantic_index(&msg));
    }

    /// Unreachable query Err reuses did-not-run wording.
    #[test]
    fn query_failure_message_unreachable_did_not_run() {
        let readiness = SemanticReadiness {
            backend_status: BackendStatus::Unreachable,
            model_name: "nomic".to_string(),
            dimensions: 768,
            vector_count: 0,
            zero_vector_count: 0,
            is_stale: false,
            dimension_mismatch: false,
        };
        let msg = semantic_query_failure_message(&readiness, &"connection refused");
        assert!(
            msg.contains("did not run") || msg.contains("unreachable"),
            "Unreachable Err reuses config-path wording: {msg}"
        );
        assert!(!msg.to_lowercase().contains("no relevant"));
    }

    /// Pure decision: Ready + Err → failure message; Ready + Ok empty → no-matches.
    #[test]
    fn no_results_message_ready_err_vs_empty_success() {
        let readiness = ready_populated();

        let on_err = semantic_no_results_message(&readiness, false, Some("store timeout"));
        assert!(
            on_err.to_lowercase().contains("fail"),
            "Err path under Ready must use failure copy: {on_err}"
        );
        assert!(
            !on_err.to_lowercase().contains("no relevant"),
            "Err path must not use empty-result Ready wording: {on_err}"
        );
        assert!(on_err.contains("store timeout"), "{on_err}");

        let on_empty_ok = semantic_no_results_message(&readiness, true, None);
        assert!(
            on_empty_ok.contains("No relevant") || on_empty_ok.contains("no relevant"),
            "Ok([]) under Ready uses no-matches copy: {on_empty_ok}"
        );
        assert!(
            !on_empty_ok.to_lowercase().contains("fail"),
            "successful empty must not say failed: {on_empty_ok}"
        );
    }

    /// NotConfigured empty success and Err both avoid "no relevant".
    #[test]
    fn no_results_message_unconfigured_never_no_matches() {
        let readiness = SemanticReadiness {
            backend_status: BackendStatus::NotConfigured,
            model_name: String::new(),
            dimensions: 0,
            vector_count: 0,
            zero_vector_count: 0,
            is_stale: false,
            dimension_mismatch: false,
        };
        for succeeded in [true, false] {
            let msg = semantic_no_results_message(&readiness, succeeded, Some("no backend"));
            assert!(
                msg.contains("did not run"),
                "unconfigured must say did not run (succeeded={succeeded}): {msg}"
            );
            assert!(
                !msg.to_lowercase().contains("no relevant"),
                "unconfigured must not claim no matches (succeeded={succeeded}): {msg}"
            );
        }
    }
}
