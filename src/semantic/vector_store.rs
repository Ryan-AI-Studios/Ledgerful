use crate::embed::similarity::cosine_sim;
use crate::semantic::chunker::AstChunk;
use crate::state::storage_cozo::CozoStorage;
use cozo::{DataValue, Num};
use miette::{Result, miette};
use tracing::{info, warn};

pub struct VectorStore<'a> {
    storage: &'a CozoStorage,
    dim: usize,
    skip_hnsw: bool,
    hnsw_rebuild_threshold: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct HnswRefreshPlan {
    drop_before_put: bool,
    rebuild_after_put: bool,
}

impl HnswRefreshPlan {
    const DEFAULT_REBUILD_BATCH_THRESHOLD: usize =
        crate::config::model::DEFAULT_HNSW_REBUILD_THRESHOLD;

    #[cfg(test)]
    fn for_batch(batch_len: usize, skip_hnsw: bool) -> Self {
        Self::for_batch_with_threshold(batch_len, skip_hnsw, Self::DEFAULT_REBUILD_BATCH_THRESHOLD)
    }

    fn for_batch_with_threshold(
        batch_len: usize,
        skip_hnsw: bool,
        rebuild_threshold: usize,
    ) -> Self {
        if skip_hnsw {
            return Self {
                drop_before_put: false,
                rebuild_after_put: false,
            };
        }

        let rebuild = batch_len >= rebuild_threshold;
        Self {
            drop_before_put: rebuild,
            rebuild_after_put: rebuild,
        }
    }
}

impl<'a> VectorStore<'a> {
    pub fn new(storage: &'a CozoStorage, dim: usize, skip_hnsw: bool) -> Result<Self> {
        Self::new_with_hnsw_threshold(
            storage,
            dim,
            skip_hnsw,
            HnswRefreshPlan::DEFAULT_REBUILD_BATCH_THRESHOLD,
        )
    }

    pub fn new_with_hnsw_threshold(
        storage: &'a CozoStorage,
        dim: usize,
        skip_hnsw: bool,
        hnsw_rebuild_threshold: usize,
    ) -> Result<Self> {
        if hnsw_rebuild_threshold == 0 {
            return Err(miette!("HNSW rebuild threshold must be > 0"));
        }
        let store = Self {
            storage,
            dim,
            skip_hnsw,
            hnsw_rebuild_threshold,
        };
        store.setup_schema()?;
        Ok(store)
    }

    /// Creates a VectorStore without building the HNSW index.
    /// Intended for testing the cos_dist fallback path and for environments
    /// where the index will be created separately (e.g., after migration).
    #[doc(hidden)]
    pub fn new_without_hnsw(storage: &'a CozoStorage, dim: usize) -> Result<Self> {
        let store = Self {
            storage,
            dim,
            skip_hnsw: true,
            hnsw_rebuild_threshold: HnswRefreshPlan::DEFAULT_REBUILD_BATCH_THRESHOLD,
        };
        store.setup_schema()?;
        Ok(store)
    }

    fn setup_schema(&self) -> Result<()> {
        let relations = self.storage.get_relations()?;
        if !relations.contains(&"snippet_embedding".to_string()) {
            // RO Cozo (soft-open / search read path): do not :create schema.
            // Treat missing relation as empty store; callers surface honesty.
            if self.storage.is_read_only() {
                tracing::debug!("snippet_embedding missing and Cozo is read-only; skip :create");
                return Ok(());
            }
            let script = format!(
                ":create snippet_embedding {{file_path,name,line_offset=>embedding:<F32; {}>}}",
                self.dim
            );
            self.storage.run_script(&script)?;
            info!(
                "Relation snippet_embedding created with {} dimensions",
                self.dim
            );

            if !self.skip_hnsw {
                self.rebuild_hnsw_index()?;
                // --- Track 54-1: FTS Index for Snippets ---
                self.storage.run_script(
                    "::fts create snippet_embedding:fts_idx {extractor: name, tokenizer: Simple}",
                )?;
            }
        } else {
            // Verify existing dimension
            if let Err(e) = self
                .storage
                .verify_embedding_dimension("snippet_embedding", self.dim)
            {
                if self.storage.is_read_only() {
                    warn!("Dimension mismatch under read-only Cozo (cannot recreate schema): {e}");
                    return Ok(());
                }
                warn!(
                    "Dimension mismatch or verification failed: {}. Clearing stale snippet embeddings.",
                    e
                );
                // HP3: must drop FTS + HNSW indices before dropping the relation
                for script in [
                    "::fts drop snippet_embedding:fts_idx",
                    "::hnsw drop snippet_embedding:snippet_idx",
                    ":drop snippet_embedding",
                ] {
                    if let Err(e) = self.storage.run_script(script) {
                        warn!("Failed to run migration cleanup script: {script} — {e}");
                    }
                }
                return self.setup_schema();
            }

            if !self.skip_hnsw {
                let indices = self.storage.get_indices("snippet_embedding")?;
                if !indices.contains(&"snippet_idx".to_string()) {
                    self.rebuild_hnsw_index()?;
                }
            }
        }
        Ok(())
    }

    pub fn get_vector_count(&self) -> Result<usize> {
        let relations = self.storage.get_relations()?;
        if !relations.contains(&"snippet_embedding".to_string()) {
            return Ok(0);
        }
        let script = "?[count(file_path)] := *snippet_embedding{file_path}";
        let res = self.storage.run_script(script)?;
        if let Some(row) = res.rows.first()
            && let Some(DataValue::Num(Num::Int(count))) = row.first()
        {
            return Ok(*count as usize);
        }
        Ok(0)
    }

    /// Count stored embeddings that are zero-length or all-zero (legacy junk).
    /// Read-only — never deletes (DoD-7).
    pub fn count_zero_vectors(&self) -> Result<usize> {
        let relations = self.storage.get_relations()?;
        if !relations.contains(&"snippet_embedding".to_string()) {
            return Ok(0);
        }
        let script = "?[file_path, name, line_offset, embedding] := *snippet_embedding{file_path, name, line_offset, embedding}";
        let res = self.storage.run_script(script)?;
        let mut count = 0usize;
        for row in res.rows {
            if let Some(DataValue::Vec(v)) = row.get(3) {
                let candidate: Vec<f32> = match &**v {
                    cozo::Vector::F32(vec) => vec.to_vec(),
                    cozo::Vector::F64(vec) => vec.iter().map(|&x| x as f32).collect(),
                };
                if candidate.is_empty() || is_all_zero(&candidate) {
                    count += 1;
                }
            }
        }
        Ok(count)
    }

    /// Expose the underlying storage reference for HP3 hash-tracking helpers.
    pub fn storage_ref(&self) -> &CozoStorage {
        self.storage
    }

    /// Remove all `snippet_embedding` rows for `file_path` (HP3 pruning).
    pub fn remove_file_snippets(&self, file_path: &str) -> Result<()> {
        let path_str = file_path.replace('\\', "/");
        let escaped = path_str.replace('\'', "\\'");
        let script = format!(
            "paths[file_path] <- [['{}']]\n\
             ?[file_path, name, line_offset] := paths[file_path], *snippet_embedding{{file_path, name, line_offset}}\n\
             :rm snippet_embedding {{file_path, name, line_offset}}",
            escaped
        );
        self.storage.run_script(&script)?;
        tracing::debug!("Pruned snippets for deleted file: {}", file_path);
        Ok(())
    }

    pub fn index_chunks(&self, chunks: Vec<AstChunk>, embeddings: Vec<Vec<f32>>) -> Result<()> {
        if chunks.len() != embeddings.len() {
            return Err(miette!("Mismatch between chunks and embeddings length"));
        }

        // DoD-2: belt-and-braces — reject zero-length and all-zero embeddings
        // (never silently skip). Match only all-zero / empty, never a norm threshold.
        for (i, embedding) in embeddings.iter().enumerate() {
            if embedding.is_empty() {
                return Err(miette!(
                    "Refusing to store zero-length embedding at index {i} (snippet '{}')",
                    chunks
                        .get(i)
                        .map(|c| c.name.as_str())
                        .unwrap_or("<unknown>")
                ));
            }
            if is_all_zero(embedding) {
                return Err(miette!(
                    "Refusing to store all-zero embedding at index {i} (snippet '{}')",
                    chunks
                        .get(i)
                        .map(|c| c.name.as_str())
                        .unwrap_or("<unknown>")
                ));
            }
        }

        use cozo::ScriptMutability;
        use std::collections::BTreeMap;

        let refresh_plan = HnswRefreshPlan::for_batch_with_threshold(
            chunks.len(),
            self.skip_hnsw,
            self.hnsw_rebuild_threshold,
        );
        let mut rebuild_after_put = refresh_plan.rebuild_after_put;
        if refresh_plan.drop_before_put {
            info!(
                "Large semantic batch detected ({} chunks). Temporarily dropping HNSW index for stable ingestion.",
                chunks.len()
            );
            let _ = self
                .storage
                .run_script("::hnsw drop snippet_embedding:snippet_idx");
        }

        let mut data_rows = Vec::new();
        for (chunk, embedding) in chunks.into_iter().zip(embeddings) {
            if embedding.len() != self.dim {
                return Err(miette::miette!(
                    "Embedding dimension mismatch: expected {}, got {}",
                    self.dim,
                    embedding.len()
                ));
            }
            // After DoD-2 rejection above, normalize is expected to succeed for
            // finite non-zero vectors; NaN/Inf-only vectors still fail loudly.
            let normalized_embedding = normalize_vector(embedding).ok_or_else(|| {
                miette!(
                    "Refusing to store invalid embedding for snippet '{}' in '{}' (zero magnitude after sanitization)",
                    chunk.name,
                    chunk.file_path
                )
            })?;
            let row = vec![
                DataValue::from(chunk.file_path.replace('\\', "/")),
                DataValue::from(chunk.name),
                DataValue::from(chunk.offset as i64),
                DataValue::Vec(Box::new(cozo::Vector::F32(normalized_embedding.into()))),
            ];
            data_rows.push(DataValue::List(Box::new(row)));
        }

        if data_rows.is_empty() {
            return Ok(());
        }

        let mut params = BTreeMap::new();
        params.insert("data".to_string(), DataValue::from(data_rows));

        let script = "?[file_path, name, line_offset, embedding] <- $data :put snippet_embedding";

        let mut attempts = 0;
        let max_attempts = 3;
        loop {
            match self.storage.run_script_with_params(
                script,
                params.clone(),
                ScriptMutability::Mutable,
            ) {
                Ok(_) => break,
                Err(e)
                    if attempts < max_attempts
                        && (e.to_string().contains("Invalid neighbor degree")
                            || e.to_string().contains("corruption")) =>
                {
                    attempts += 1;
                    warn!(
                        "HNSW storage issue detected ({}). Attempting self-healing (attempt {}/{})...",
                        e, attempts, max_attempts
                    );
                    let _ = self
                        .storage
                        .run_script("::hnsw drop snippet_embedding:snippet_idx");
                    rebuild_after_put = !self.skip_hnsw;
                }
                Err(e)
                    if attempts < max_attempts
                        && (e.to_string().contains("locked") || e.to_string().contains("busy")) =>
                {
                    attempts += 1;
                    let delay = std::time::Duration::from_millis(200 * attempts as u64);
                    warn!(
                        "Database busy, retrying in {:?} (attempt {}/{})...",
                        delay, attempts, max_attempts
                    );
                    std::thread::sleep(delay);
                }
                Err(e) => return Err(e),
            }
        }

        // Rebuild/refresh index after batch put
        if rebuild_after_put {
            self.rebuild_hnsw_index()?;
        }

        Ok(())
    }

    pub fn rebuild_hnsw_index(&self) -> Result<()> {
        info!("Building HNSW index for snippet_embedding...");

        // 1. Ensure any stale index is gone
        let _ = self
            .storage
            .run_script("::hnsw drop snippet_embedding:snippet_idx");

        // 2. Create the index
        let hnsw_script = format!(
            "::hnsw create snippet_embedding:snippet_idx {{dim:{},dtype:F32,fields:[embedding],distance:L2,m:16,ef_construction:100}}",
            self.dim
        );

        // Wrap in retry for Windows filesystem sync
        let mut attempts = 0;
        loop {
            match self.storage.run_script(&hnsw_script) {
                Ok(_) => break,
                Err(e)
                    if attempts < 3
                        && (e.to_string().contains("locked") || e.to_string().contains("busy")) =>
                {
                    attempts += 1;
                    std::thread::sleep(std::time::Duration::from_millis(500));
                }
                Err(e) => return Err(e),
            }
        }

        info!("HNSW index built successfully");
        Ok(())
    }

    pub fn query(
        &self,
        query_vector: Vec<f32>,
        k: usize,
    ) -> Result<Vec<(String, String, usize, f32)>> {
        use cozo::ScriptMutability;
        use std::collections::BTreeMap;

        let query_vector = match normalize_vector(query_vector) {
            Some(v) => v,
            None => {
                tracing::warn!("Query vector is invalid (zero magnitude). Aborting search.");
                return Ok(Vec::new());
            }
        };

        let mut params = BTreeMap::new();
        params.insert(
            "query_vec".to_string(),
            DataValue::Vec(Box::new(cozo::Vector::F32(query_vector.clone().into()))),
        );

        // Tier 1: HNSW candidate generation with exact Cozo-side cosine reranking.
        // Over-fetch candidates so zero-magnitude legacy rows excluded at parse
        // time (DoD-7b) do not starve the top-k.
        let candidate_k = k.saturating_mul(10).max(50);
        let hnsw_script = format!(
            "?[file_path, name, line_offset, dist] := ~snippet_embedding:snippet_idx{{file_path, name, line_offset | query: $query_vec, k: {candidate_k}, ef: 100}}, *snippet_embedding{{file_path, name, line_offset, embedding}}, dist = cos_dist(embedding, $query_vec) :order +dist :limit {candidate_k}"
        );
        let res = self.storage.run_script_with_params(
            &hnsw_script,
            params.clone(),
            ScriptMutability::Immutable,
        );

        match res {
            Ok(r) => {
                info!("Semantic query served by HNSW index");
                return parse_hnsw_results(r, k);
            }
            Err(e)
                if e.to_string().contains("hnsw_index_not_found")
                    || e.to_string().contains("no_implementation") =>
            {
                warn!("HNSW index unavailable, falling back to Cozo-native cos_dist.");
                // Fall through to Tier 2
            }
            Err(e) => return Err(e),
        }

        // Tier 2: CozoDB-native cos_dist query (over-fetch for DoD-7b filter)
        let fetch_k = k.saturating_mul(10).max(50);
        let cos_dist_script = format!(
            "?[file_path, name, line_offset, dist] := *snippet_embedding{{file_path, name, line_offset, embedding}}, dist = cos_dist(embedding, $query_vec) :order +dist :limit {}",
            fetch_k
        );
        let cos_res = self.storage.run_script_with_params(
            &cos_dist_script,
            params.clone(),
            ScriptMutability::Immutable,
        );

        match cos_res {
            Ok(r) => {
                info!("Semantic query served by Cozo-native cos_dist");
                return parse_hnsw_results(r, k);
            }
            Err(e) if e.to_string().contains("no_implementation") => {
                warn!("Cozo-native cos_dist unavailable, falling back to Rust-side cosine_sim.");
                // Fall through to Tier 3
            }
            Err(e) => return Err(e),
        }

        // Tier 3: Rust-side cosine_sim loop (last-resort safety net)
        warn!(
            "Serving semantic query via Rust-side cosine_sim (slow path) — consider running 'ledgerful update --migrate' and 'ledgerful index --semantic'."
        );
        let all_script = "?[file_path,name,line_offset,embedding] := *snippet_embedding{file_path,name,line_offset,embedding}";
        let all_res = self.storage.run_script(all_script)?;

        let mut scored_results = Vec::new();
        for row in all_res.rows {
            if let (
                Some(DataValue::Str(file_path)),
                Some(DataValue::Str(name)),
                Some(DataValue::Num(Num::Int(offset))),
                Some(DataValue::Vec(v)),
            ) = (row.first(), row.get(1), row.get(2), row.get(3))
            {
                let candidate_vec: Vec<f32> = match &**v {
                    cozo::Vector::F32(vec) => vec.to_vec(),
                    cozo::Vector::F64(vec) => vec.iter().map(|&x| x as f32).collect(),
                };

                // DoD-7b: exclude zero-magnitude stored vectors at query time
                if candidate_vec.is_empty() || is_all_zero(&candidate_vec) {
                    continue;
                }
                if normalize_vector(candidate_vec.clone()).is_none() {
                    continue;
                }

                if let Ok(sim) = cosine_sim(&query_vector, &candidate_vec) {
                    if !sim.is_finite() {
                        continue;
                    }
                    scored_results.push((
                        file_path.to_string(),
                        name.to_string(),
                        *offset as usize,
                        sim,
                    ));
                }
            }
        }

        scored_results.sort_by(|a, b| {
            b.3.partial_cmp(&a.3)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.0.cmp(&b.0))
        });

        if scored_results.len() > k {
            scored_results.truncate(k);
        }

        // Return cos_dist values (1.0 - sim) for consistency with the HNSW/cos_dist paths
        Ok(scored_results
            .into_iter()
            .map(|(f, n, o, s)| (f, n, o, 1.0 - s))
            .collect())
    }
}

/// True when every element is exactly `0.0` (all-zero / zero-length check
/// for DoD-2 / DoD-7). Does **not** use a norm threshold (spec §6).
fn is_all_zero(vector: &[f32]) -> bool {
    !vector.is_empty() && vector.iter().all(|&x| x == 0.0)
}

/// Normalize a vector to unit length.
///
/// Sanitizes any `NaN` or `Inf` values to `0.0` before computing the norm.
/// If the resulting magnitude is zero or near-zero (< 1e-9), returns `None`
/// to indicate the embedding is invalid and should not be stored or queried.
fn normalize_vector(mut vector: Vec<f32>) -> Option<Vec<f32>> {
    // 1. Sanitize: replace NaN/Inf with 0.0
    for value in &mut vector {
        if !value.is_finite() {
            *value = 0.0;
        }
    }

    // 2. Compute magnitude and normalise
    let norm = vector.iter().map(|v| v * v).sum::<f32>().sqrt();
    if norm > 1e-9 {
        for value in &mut vector {
            *value /= norm;
        }
        Some(vector)
    } else {
        None
    }
}

/// Parse Cozo HNSW / cos_dist rows. **Excludes** non-finite distances
/// (DoD-7b: zero-magnitude stored vectors yield NaN from `cos_dist` and
/// must not rank). Truncates to `limit` after filtering.
fn parse_hnsw_results(
    res: cozo::NamedRows,
    limit: usize,
) -> Result<Vec<(String, String, usize, f32)>> {
    let mut results = Vec::new();
    for row in res.rows {
        if let (
            Some(DataValue::Str(file_path)),
            Some(DataValue::Str(name)),
            Some(DataValue::Num(Num::Int(offset))),
            Some(DataValue::Num(Num::Float(dist))),
        ) = (row.first(), row.get(1), row.get(2), row.get(3))
        {
            let dist_f32 = *dist as f32;
            // DoD-7b: drop NaN/Inf rather than clamping into the ranking.
            if !dist_f32.is_finite() {
                tracing::debug!(
                    "Excluding non-finite cos_dist for {}::{} (legacy zero-magnitude row?)",
                    file_path,
                    name
                );
                continue;
            }
            results.push((
                file_path.to_string(),
                name.to_string(),
                *offset as usize,
                dist_f32,
            ));
        }
    }
    if results.len() > limit {
        results.truncate(limit);
    }
    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_regular_vector_has_unit_magnitude() {
        let v = normalize_vector(vec![3.0_f32, 4.0_f32]).unwrap();
        let mag: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!(
            (mag - 1.0).abs() < 1e-5,
            "magnitude should be 1.0, got {mag}"
        );
        assert!(v.iter().all(|x| x.is_finite()));
    }

    #[test]
    fn normalize_zero_vector_returns_none() {
        let v = normalize_vector(vec![0.0_f32, 0.0_f32, 0.0_f32]);
        assert!(v.is_none());
    }

    #[test]
    fn normalize_nan_inputs_are_sanitized_to_valid_or_none() {
        let v = normalize_vector(vec![f32::NAN, 1.0_f32, 0.0_f32]).unwrap();
        assert!(
            v.iter().all(|x| x.is_finite()),
            "all elements must be finite"
        );
        assert_eq!(v[0], 0.0);
        assert!((v[1] - 1.0).abs() < 1e-5);
    }

    #[test]
    fn normalize_inf_inputs_are_sanitized_to_none() {
        let v = normalize_vector(vec![f32::INFINITY, 0.0_f32]);
        assert!(v.is_none());
    }

    #[test]
    fn normalize_empty_vector_does_not_panic() {
        let v = normalize_vector(vec![]);
        assert!(v.is_none());
    }

    #[test]
    fn hnsw_refresh_plan_keeps_existing_index_for_small_batches() {
        let plan = HnswRefreshPlan::for_batch(25, false);
        assert!(!plan.drop_before_put);
        assert!(!plan.rebuild_after_put);
    }

    #[test]
    fn hnsw_refresh_plan_rebuilds_for_large_batches() {
        let plan = HnswRefreshPlan::for_batch(500, false);
        assert!(plan.drop_before_put);
        assert!(plan.rebuild_after_put);
    }

    #[test]
    fn hnsw_refresh_plan_skips_when_disabled() {
        let plan = HnswRefreshPlan::for_batch(1000, true);
        assert!(!plan.drop_before_put);
        assert!(!plan.rebuild_after_put);
    }

    #[test]
    fn hnsw_refresh_plan_respects_configured_threshold() {
        let below = HnswRefreshPlan::for_batch_with_threshold(49, false, 50);
        assert!(!below.drop_before_put);
        assert!(!below.rebuild_after_put);

        let at_threshold = HnswRefreshPlan::for_batch_with_threshold(50, false, 50);
        assert!(at_threshold.drop_before_put);
        assert!(at_threshold.rebuild_after_put);
    }

    fn test_chunk(name: &str) -> AstChunk {
        AstChunk {
            file_path: "t.rs".to_string(),
            name: name.to_string(),
            kind: crate::index::symbols::SymbolKind::Function,
            content: format!("fn {name}() {{}}"),
            docstring: None,
            range: (0, 10),
            lines: (1, 1),
            offset: 0,
        }
    }

    /// DoD-2: index_chunks rejects all-zero embeddings independently of the embedder.
    #[test]
    fn index_chunks_rejects_all_zero_embeddings() {
        let storage = CozoStorage::new_in_memory().expect("in-memory cozo");
        let store = VectorStore::new_without_hnsw(&storage, 3).expect("store");
        let chunks = vec![test_chunk("zero_fn")];
        let embeddings = vec![vec![0.0_f32, 0.0, 0.0]];
        let err = store
            .index_chunks(chunks, embeddings)
            .expect_err("all-zero must be rejected");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("all-zero") || msg.contains("zero"),
            "expected all-zero rejection, got: {msg}"
        );
        assert_eq!(store.get_vector_count().unwrap_or(0), 0);
    }

    /// DoD-2: index_chunks rejects zero-length embeddings.
    #[test]
    fn index_chunks_rejects_zero_length_embeddings() {
        let storage = CozoStorage::new_in_memory().expect("in-memory cozo");
        let store = VectorStore::new_without_hnsw(&storage, 3).expect("store");
        let chunks = vec![test_chunk("empty_fn")];
        let embeddings = vec![vec![]];
        let err = store
            .index_chunks(chunks, embeddings)
            .expect_err("zero-length must be rejected");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("zero-length") || msg.contains("zero"),
            "expected zero-length rejection, got: {msg}"
        );
        assert_eq!(store.get_vector_count().unwrap_or(0), 0);
    }

    /// DoD-2 / sparse: legitimately sparse (not all-zero) vectors are accepted.
    #[test]
    fn index_chunks_accepts_sparse_non_zero_embeddings() {
        let storage = CozoStorage::new_in_memory().expect("in-memory cozo");
        let store = VectorStore::new_without_hnsw(&storage, 3).expect("store");
        let chunks = vec![test_chunk("sparse_fn")];
        // Sparse but not all-zero — must not be rejected by the all-zero gate.
        let embeddings = vec![vec![0.0_f32, 0.0, 1.0]];
        store
            .index_chunks(chunks, embeddings)
            .expect("sparse non-zero must be accepted");
        assert_eq!(store.get_vector_count().unwrap_or(0), 1);
    }

    /// DoD-7: count_zero_vectors reports without deleting.
    #[test]
    fn count_zero_vectors_does_not_delete() {
        use cozo::{DataValue, ScriptMutability};
        use std::collections::BTreeMap;

        let storage = CozoStorage::new_in_memory().expect("in-memory cozo");
        let store = VectorStore::new_without_hnsw(&storage, 3).expect("store");
        // Insert one valid vector via the guarded API.
        store
            .index_chunks(vec![test_chunk("good")], vec![vec![1.0, 0.0, 0.0]])
            .expect("valid insert");
        // Bypass the guard to plant a legacy zero row (simulates pre-0096 junk).
        let mut params = BTreeMap::new();
        params.insert(
            "data".to_string(),
            DataValue::from(vec![DataValue::List(Box::new(vec![
                DataValue::from("t.rs"),
                DataValue::from("junk"),
                DataValue::from(1_i64),
                DataValue::Vec(Box::new(cozo::Vector::F32(vec![0.0_f32, 0.0, 0.0].into()))),
            ]))]),
        );
        storage
            .run_script_with_params(
                "?[file_path, name, line_offset, embedding] <- $data :put snippet_embedding",
                params,
                ScriptMutability::Mutable,
            )
            .expect("plant junk");

        let before_total = store.get_vector_count().expect("count");
        let zeros = store.count_zero_vectors().expect("zero count");
        assert_eq!(zeros, 1, "exactly one planted zero row");
        let after_total = store.get_vector_count().expect("count after");
        assert_eq!(
            before_total, after_total,
            "count_zero_vectors must not delete rows"
        );
        assert_eq!(after_total, 2);
    }

    /// DoD-7b: query with a valid vector excludes all-zero stored rows from ranking.
    #[test]
    fn query_excludes_zero_magnitude_stored_vectors() {
        use cozo::{DataValue, ScriptMutability};
        use std::collections::BTreeMap;

        let storage = CozoStorage::new_in_memory().expect("in-memory cozo");
        let store = VectorStore::new_without_hnsw(&storage, 3).expect("store");
        store
            .index_chunks(
                vec![test_chunk("good_a"), test_chunk("good_b")],
                vec![vec![1.0, 0.0, 0.0], vec![0.0, 1.0, 0.0]],
            )
            .expect("valid inserts");
        // Plant all-zero junk.
        let mut params = BTreeMap::new();
        params.insert(
            "data".to_string(),
            DataValue::from(vec![DataValue::List(Box::new(vec![
                DataValue::from("t.rs"),
                DataValue::from("junk_zero"),
                DataValue::from(99_i64),
                DataValue::Vec(Box::new(cozo::Vector::F32(vec![0.0_f32, 0.0, 0.0].into()))),
            ]))]),
        );
        storage
            .run_script_with_params(
                "?[file_path, name, line_offset, embedding] <- $data :put snippet_embedding",
                params,
                ScriptMutability::Mutable,
            )
            .expect("plant junk");

        let results = store
            .query(vec![1.0, 0.0, 0.0], 10)
            .expect("query must succeed");
        assert!(
            results.iter().all(|(_, name, _, _)| name != "junk_zero"),
            "zero-magnitude stored vector must not appear in results: {results:?}"
        );
        assert!(
            !results.is_empty(),
            "valid rows must still be returned under a valid query vector"
        );
        assert_eq!(results[0].1, "good_a");
    }
}
