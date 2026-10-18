mod chunk_failures;
mod progress;
mod stages;

use crate::config::model::Config;
use crate::semantic::SemanticDiscovery;
use crate::semantic::chunker::{AstChunker, semantic_file_content_hash};
use crate::semantic::concurrency::EmbedSemaphore;
use crate::semantic::walk_repo_for_semantic_files;
use crate::state::layout::Layout;
use crate::state::storage::StorageManager;
use crate::util::path::{path_is_under_work_root, resolve_under_work_root, semantic_path_key};
use miette::Result;
use serde::Serialize;
use std::sync::Arc;
use tracing::{debug, info, warn};

use chunk_failures::{chunk_failure_progress_line, drop_failed_chunk_rows, failed_chunk_files};
use progress::{embedding_done_progress_line, emit_semantic_progress};
use stages::{embed_semantic_chunks, parse_semantic_files, persist_semantic_chunks};

/// Resolve effective semantic index mode from flags + post-purge vector count.
/// Warm SoT = `vector_count > 0` (not hash presence). Pure / unit-testable (0161).
pub(crate) fn resolve_semantic_index_mode(
    force_full: bool,
    force_incremental: bool,
    vector_count: usize,
) -> (bool /* incremental */, &'static str /* reason */) {
    if force_full {
        return (false, "--full");
    }
    if force_incremental {
        return (true, "explicit-incremental");
    }
    if vector_count > 0 {
        (true, "auto-incremental")
    } else {
        (false, "cold-store")
    }
}

/// Warm incremental skip: hash-current and (has snippets or chunker produced none).
/// `chunk_file` errors keep the file in `to_process`.
pub(crate) fn skip_warm_incremental_file(
    semantic: &SemanticDiscovery<'_>,
    path: &std::path::Path,
    content: &str,
    key: &str,
    hash: &str,
) -> bool {
    if !semantic.is_file_hash_current(key, hash) {
        return false;
    }
    if semantic.file_has_snippets(key) {
        return true;
    }
    matches!(AstChunker::chunk_file(path, content), Ok(chunks) if chunks.is_empty())
}

/// Final machine summary for `index --semantic --json` (schemaVersion 1).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SemanticIndexJsonResult {
    schema_version: u32,
    mode: &'static str,
    reason: &'static str,
    files_processed: usize,
    files_candidates: usize,
    chunks_embedded: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    purged_foreign: Option<u64>,
    up_to_date: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    hnsw_rebuilt: Option<bool>,
    /// 0545: files that read but failed to chunk this run. Omitted at zero.
    #[serde(skip_serializing_if = "Option::is_none")]
    chunk_failed_files: Option<usize>,
}

fn emit_semantic_json(result: &SemanticIndexJsonResult) -> Result<()> {
    let json = serde_json::to_string(result)
        .map_err(|e| miette::miette!("Failed to serialize semantic index JSON: {e}"))?;
    println!("{json}");
    Ok(())
}

/// Resolve parse and embed concurrency from CLI override, semantic config,
/// and local-model defaults. Used by both semantic index and dry-run.
pub(crate) fn resolve_semantic_concurrency(
    concurrency_override: Option<usize>,
    config: &Config,
) -> crate::semantic::concurrency::ResolvedConcurrency {
    use crate::semantic::concurrency::{ResolveOptions, resolve_split_semantic_concurrency};
    let available_parallelism = std::thread::available_parallelism().ok().map(|n| {
        std::num::NonZeroUsize::new(n.get()).unwrap_or(std::num::NonZeroUsize::new(1).unwrap())
    });
    let resolve_opts = ResolveOptions {
        available_parallelism,
        ..Default::default()
    };
    resolve_split_semantic_concurrency(
        concurrency_override,
        &config.semantic,
        config.local_model.concurrency,
        resolve_opts,
    )
}

pub(crate) fn execute_semantic_index(
    layout: &Layout,
    storage: StorageManager,
    config: &Config,
    force_full: bool,
    force_incremental: bool,
    concurrency_override: Option<usize>,
    json: bool,
) -> Result<()> {
    // DoD-1: refuse before any write when no embedding backend is configured.
    // Non-zero exit is intentional — the command cannot succeed meaningfully.
    if !crate::embed::client::is_embedding_backend_configured(&config.local_model) {
        return Err(miette::miette!(
            "Semantic indexing requires an embedding backend. \
             Set `local_model.base_url` (or `local_model.embedding_url`) in config, then re-run. \
             Inspect with `ledgerful index --semantic-dry-run`. \
             (Unconfigured is a valid install state — configure only if you want semantic search.)"
        ));
    }

    let cozo = storage
        .cozo()
        .ok_or_else(|| miette::miette!("CozoDB storage not initialized"))?;

    let (semantic, recreated) = SemanticDiscovery::new_for_index(
        config.local_model.clone(),
        config.semantic.clone(),
        cozo,
    )?;
    if recreated {
        let dim = preferred_embedding_dimensions(&config.local_model);
        emit_semantic_progress(
            json,
            &format!(
                "WARN: Recreated snippet_embedding at {dim} dimensions (wiped stored vectors)."
            ),
        );
    }

    // HP3: ensure the semantic file-hash tracking schema exists
    semantic.ensure_file_hash_schema()?;

    let resolved = resolve_semantic_concurrency(concurrency_override, config);
    let parse_threads = resolved.parse_threads.get();
    let embed_cap = resolved.embed_threads.get();

    let repo_root = layout.root.as_std_path();

    // 0152: dual-relation foreign purge at start of BOTH modes BEFORE warm detection.
    let purged = semantic.purge_foreign_semantic_keys(repo_root)?;
    if purged > 0 {
        debug!("Purged {purged} semantic path key(s) outside work root");
    }

    // Warm SoT = vector_count > 0 after purge (safe 0 on error — never panic).
    let vector_count = semantic.get_vector_count().unwrap_or(0);
    let (incremental, reason) =
        resolve_semantic_index_mode(force_full, force_incremental, vector_count);
    let mode_label: &'static str = if incremental { "incremental" } else { "full" };

    // C5: mode line as soon as resolved (before walk/hash long work).
    emit_semantic_progress(
        json,
        &format!("Semantic indexing: mode={mode_label} (reason={reason})"),
    );
    // Purge human line after mode when purged > 0 (0154 quiet default).
    if purged > 0 {
        emit_semantic_progress(
            json,
            &format!("Purged {purged} semantic path key(s) outside work root."),
        );
    }

    info!(
        "Semantic indexing started: mode={mode_label}, reason={reason}, cli_concurrency={:?}",
        concurrency_override
    );
    info!("Semantic indexing threads: parse={parse_threads}, embed_concurrency={embed_cap}");

    // ── Phase 1: Collect candidate files ───────────────────────────────────
    let candidate_paths = walk_repo_for_semantic_files(repo_root);
    let files_candidates = candidate_paths.len();

    // HP3: On incremental runs filter to changed hashes OR missing snippets (C1).
    let files_to_process: Vec<std::path::PathBuf> = if incremental {
        let tracked_files = semantic.get_tracked_files()?;
        for tracked in tracked_files {
            // Relative keys must be root-joined for exists() (CWD-relative after B1 is a mass-prune bug).
            // Foreign keys should already be gone via dual purge; re-check for defense in depth.
            let gone = if !path_is_under_work_root(repo_root, &tracked) {
                true
            } else {
                !resolve_under_work_root(repo_root, &tracked).exists()
            };
            if gone {
                info!("Pruning deleted file from semantic index: {}", tracked);
                if let Err(e) = semantic.remove_file_snippets(&tracked) {
                    warn!(
                        "Failed to prune snippets for deleted file {}: {}",
                        tracked, e
                    );
                }
                if let Err(e) = semantic.remove_file_hash(&tracked) {
                    warn!(
                        "Failed to remove file hash for deleted file {}: {}",
                        tracked, e
                    );
                }
            }
        }

        candidate_paths
            .into_iter()
            .filter(|path| {
                let Ok(content) = crate::util::fs::read_to_string_with_encoding(path) else {
                    return true; // re-try unreadable files (C10 soft)
                };
                let hash = semantic_file_content_hash(&content);
                let Ok(key) = semantic_path_key(repo_root, path) else {
                    return true;
                };
                !skip_warm_incremental_file(&semantic, path, &content, &key, &hash)
            })
            .collect()
    } else {
        // Full index: prune snippets for files that no longer exist (also purges foreign leftovers)
        semantic.prune_deleted_snippets(repo_root)?;
        candidate_paths
    };

    let to_process = files_to_process.len();
    emit_semantic_progress(
        json,
        &format!("Semantic indexing: candidates={files_candidates} to_process={to_process}"),
    );

    // B4: zero-delta up-to-date — always print (mode-aware); exit 0.
    if files_to_process.is_empty() {
        if incremental {
            emit_semantic_progress(
                json,
                "Semantic index up to date: 0 files changed (incremental).",
            );
        } else if files_candidates == 0 {
            emit_semantic_progress(json, "Semantic index up to date: 0 candidate files.");
        } else {
            emit_semantic_progress(
                json,
                "Semantic index complete: 0/0 files (full, nothing to process).",
            );
        }
        if json {
            emit_semantic_json(&SemanticIndexJsonResult {
                schema_version: 1,
                mode: mode_label,
                reason,
                files_processed: 0,
                files_candidates,
                chunks_embedded: 0,
                purged_foreign: if purged > 0 { Some(purged) } else { None },
                up_to_date: true,
                hnsw_rebuilt: None,
                chunk_failed_files: None,
            })?;
        }
        return Ok(());
    }

    // ── Phase 2: Configure Rayon thread pool (U13/U14) ──────────────────────

    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(parse_threads)
        .build()
        .map_err(|e| miette::miette!("Failed to build Rayon thread pool: {}", e))?;

    let embed_semaphore = Arc::new(EmbedSemaphore::new(embed_cap));

    // ── Phase 3: Parallel parse + embed with progress (HP2 + HP4 + 0161 B1) ─
    let total = files_to_process.len();

    let parsed_files_res = parse_semantic_files(&pool, files_to_process, json);

    let mut parsed_files = Vec::new();
    let mut parse_errors = Vec::new();
    for res in parsed_files_res {
        match res {
            Ok(val) => parsed_files.push(val),
            Err(e) => parse_errors.push(e),
        }
    }

    for err in &parse_errors {
        warn!("Semantic indexing skipped due to parse error: {}", err);
    }

    // Flatten chunks — rewrite absolute chunk.file_path to work-root-relative keys (0152 B1).
    let mut flat_chunks = Vec::new();
    let mut successful_files = Vec::new();
    for (path, content, chunks) in parsed_files {
        let hash = semantic_file_content_hash(&content);
        let Ok(path_key) = semantic_path_key(repo_root, &path) else {
            warn!(
                "Skipping semantic ingest for path outside work root: {}",
                path.display()
            );
            continue;
        };
        successful_files.push((path_key.clone(), hash));
        for mut chunk in chunks {
            chunk.file_path = path_key.clone();
            flat_chunks.push(chunk);
        }
    }

    let files_indexed_count = successful_files.len();
    let chunks_to_embed = flat_chunks.len();

    // Batch embedding generation
    let mut all_embeddings = Vec::new();
    if !flat_chunks.is_empty() {
        match embed_semantic_chunks(&pool, &semantic, embed_semaphore, &flat_chunks, json) {
            Ok(batches) => {
                if let Some(line) = embedding_done_progress_line(chunks_to_embed, true) {
                    emit_semantic_progress(json, &line);
                }
                for batch in batches {
                    all_embeddings.extend(batch);
                }
            }
            Err(e) => {
                // Soft C: never emit "embedding done" on failure (None when succeeded=false).
                debug_assert!(embedding_done_progress_line(chunks_to_embed, false).is_none());
                return Err(miette::miette!("Embedding generation failed: {}", e));
            }
        }
    }

    // ── Phase 4: Batch ingest into CozoDB (single-threaded for safety) ─────
    let hnsw_rebuilt = persist_semantic_chunks(
        &semantic,
        config,
        &successful_files,
        flat_chunks,
        all_embeddings,
        json,
    )?;

    // 0545: clear rows of changed files that failed to chunk (never `Read` errors),
    // after the successful files are persisted and before their hashes are recorded.
    let chunk_failures = failed_chunk_files(&parse_errors, repo_root);
    let chunk_failed = chunk_failures.len();
    if chunk_failed > 0 {
        let dropped = drop_failed_chunk_rows(&semantic, &chunk_failures);
        debug!(
            "Chunk-failure rows: cleared={}, kept_current={}, failed={}",
            dropped.cleared, dropped.kept_current, dropped.failed
        );
    }
    if let Some(line) = chunk_failure_progress_line(chunk_failed) {
        emit_semantic_progress(json, &line);
    }

    // Record new hashes only for successfully processed files (relative keys).
    for (path_key, hash) in successful_files {
        if let Err(e) = semantic.record_file_hash(&path_key, &hash) {
            warn!("Failed to record file hash for {path_key}: {e}");
        }
    }

    let complete_msg = format!(
        "Semantic indexing complete: {files_indexed_count}/{total} files produced embeddings{}.",
        if incremental { " (incremental)" } else { "" }
    );
    emit_semantic_progress(json, &complete_msg);

    if json {
        emit_semantic_json(&SemanticIndexJsonResult {
            schema_version: 1,
            mode: mode_label,
            reason,
            files_processed: files_indexed_count,
            files_candidates,
            chunks_embedded: chunks_to_embed,
            purged_foreign: if purged > 0 { Some(purged) } else { None },
            up_to_date: false,
            hnsw_rebuilt: if hnsw_rebuilt { Some(true) } else { None },
            chunk_failed_files: if chunk_failed > 0 {
                Some(chunk_failed)
            } else {
                None
            },
        })?;
    }
    Ok(())
}

pub(crate) fn execute_semantic_dry_run(
    layout: &Layout,
    config: &Config,
    concurrency_override: Option<usize>,
    output_path: Option<std::path::PathBuf>,
) -> Result<()> {
    use comfy_table::Table;

    let cozo_path = layout.state_subdir().join("ledger.cozo");
    let cozo = if cozo_path.exists() {
        crate::state::storage_cozo::CozoStorage::new_read_only(cozo_path.as_std_path()).ok()
    } else {
        None
    };

    let resolved = resolve_semantic_concurrency(concurrency_override, config);

    let candidate_paths = walk_repo_for_semantic_files(layout.root.as_std_path());

    let mut total_lines = 0;
    for path in &candidate_paths {
        if let Ok(content) = std::fs::read_to_string(path) {
            total_lines += content.lines().count();
        }
    }
    let estimated_chunk_count = total_lines / 30;

    let provenance = match crate::semantic::provenance::collect_semantic_provenance(
        layout.root.as_std_path(),
        cozo.as_ref(),
        crate::semantic::provenance::ProvenanceWalk::Always,
    ) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!("semantic dry-run provenance query failed: {e}");
            crate::semantic::provenance::SemanticProvenance {
                candidate_file_count: candidate_paths.len(),
                stored_hash_file_count: 0,
                stored_vector_row_count: 0,
                stored_vector_file_count: 0,
                orphaned_hash_count: 0,
                orphaned_vector_file_count: 0,
                missing_hash_count: 0,
                drifted_hash_count: 0,
                stored_dimensions: None,
                walked: true,
            }
        }
    };

    let hnsw_rebuild_threshold = config.semantic.hnsw_rebuild_threshold();
    let would_trigger_hnsw_rebuild = estimated_chunk_count > hnsw_rebuild_threshold;

    let embedding_dimensions = preferred_embedding_dimensions(&config.local_model);
    let stored_dimensions = provenance.stored_dimensions;
    let dimension_mismatch = match (embedding_dimensions, stored_dimensions) {
        (preferred, Some(stored)) if preferred > 0 => preferred != stored,
        _ => false,
    };

    let report = SemanticDryRunReport {
        parse_threads: resolved.parse_threads.get(),
        parse_source: resolved.parse_source.to_string(),
        embed_concurrency: resolved.embed_threads.get(),
        requested_embed_concurrency: resolved.requested_embed_threads.get(),
        embed_source: resolved.embed_source.to_string(),
        embed_concurrency_cap: resolved.embed_cap.get(),
        cap_source: resolved.cap_source.to_string(),
        candidate_file_count: candidate_paths.len(),
        estimated_chunk_count,
        embedding_model: config.local_model.embedding_model.clone(),
        embedding_model_source: "config".to_string(),
        embedding_dimensions,
        stored_dimensions,
        dimension_mismatch,
        hnsw_rebuild_threshold,
        would_trigger_hnsw_rebuild,
        current_vector_count: provenance.stored_vector_row_count,
        stored_vector_file_count: provenance.stored_vector_file_count,
        current_file_count: provenance.stored_hash_file_count,
        orphaned_hash_count: provenance.orphaned_hash_count,
        orphaned_vector_file_count: provenance.orphaned_vector_file_count,
        missing_hash_count: provenance.missing_hash_count,
    };

    if let Some(path) = output_path {
        let json_str = serde_json::to_string_pretty(&report)
            .map_err(|e| miette::miette!("Failed to serialize dry-run report to JSON: {}", e))?;
        std::fs::write(&path, json_str).map_err(|e| {
            miette::miette!(
                "Failed to write dry-run report to {}: {}",
                path.display(),
                e
            )
        })?;
        println!("Dry-run report written to {}", path.display());
    } else {
        println!("Semantic Indexing Dry-Run Report");
        println!("=================================");
        let mut table = Table::new();
        table.set_header(vec!["Metric", "Value", "Source / Reason"]);
        table.add_row(vec![
            "Parse Threads",
            &report.parse_threads.to_string(),
            &report.parse_source,
        ]);
        table.add_row(vec![
            "Requested Embed Concurrency",
            &report.requested_embed_concurrency.to_string(),
            &report.embed_source,
        ]);
        table.add_row(vec![
            "Effective Embed Concurrency",
            &report.embed_concurrency.to_string(),
            "min(Requested Embed Concurrency, Embed Concurrency Cap)",
        ]);
        table.add_row(vec![
            "Embed Concurrency Cap",
            &report.embed_concurrency_cap.to_string(),
            &report.cap_source,
        ]);
        table.add_row(vec![
            "Candidate Files",
            &report.candidate_file_count.to_string(),
            "File walk of repository (semantic extensions; not the lexical Tantivy set)",
        ]);
        table.add_row(vec![
            "Estimated Chunks",
            &report.estimated_chunk_count.to_string(),
            "Lines count / 30 approximation (not AST function grain)",
        ]);
        table.add_row(vec![
            "Embedding Model",
            &report.embedding_model,
            "config.local_model.embedding_model (not stored)",
        ]);
        let dims_str = format_dimensions_label(report.embedding_dimensions);
        table.add_row(vec![
            "Embedding Dimensions",
            &dims_str,
            "configured > probed (preferred); stored width is a separate row",
        ]);
        let stored_dims_str = report
            .stored_dimensions
            .map(format_dimensions_label)
            .unwrap_or_else(|| "unset".to_string());
        table.add_row(vec![
            "Stored Dimensions",
            &stored_dims_str,
            "snippet_embedding column; unset when missing",
        ]);
        table.add_row(vec![
            "HNSW Rebuild Threshold",
            &report.hnsw_rebuild_threshold.to_string(),
            "config.semantic.hnsw_rebuild_threshold",
        ]);
        table.add_row(vec![
            "Would Rebuild HNSW",
            &report.would_trigger_hnsw_rebuild.to_string(),
            "Estimated chunks > threshold",
        ]);
        table.add_row(vec![
            "Current Vectors in DB",
            &report.current_vector_count.to_string(),
            "snippet_embedding rows (function-grain chunks, not distinct files)",
        ]);
        table.add_row(vec![
            "Current Vector Files in DB",
            &report.stored_vector_file_count.to_string(),
            "distinct snippet_embedding paths",
        ]);
        table.add_row(vec![
            "Current Files in DB",
            &report.current_file_count.to_string(),
            "semantic_file_hash relation (not vector-store files)",
        ]);
        table.add_row(vec![
            "Orphaned hash files",
            &report.orphaned_hash_count.to_string(),
            "stored hashes whose path is not in the current walk",
        ]);
        table.add_row(vec![
            "Orphaned vector files",
            &report.orphaned_vector_file_count.to_string(),
            "distinct snippet_embedding paths not in the current walk",
        ]);
        table.add_row(vec![
            "Missing hash files",
            &report.missing_hash_count.to_string(),
            "current-walk files with no semantic_file_hash row",
        ]);
        println!("{table}");
    }

    Ok(())
}

/// Preferred index width: configured > probed. `0` when neither is available.
/// Does not probe when the embedding backend is unconfigured.
pub(crate) fn preferred_embedding_dimensions(
    config: &crate::config::model::LocalModelConfig,
) -> usize {
    if config.dimensions > 0 {
        return config.dimensions;
    }
    if !crate::embed::client::is_embedding_backend_configured(config) {
        return 0;
    }
    match crate::embed::client::check_local_model(config) {
        Ok(d) if d.dimensions > 0 => d.dimensions,
        _ => 0,
    }
}

/// Human label for preferred embedding width. `0` is unset, not a probe.
pub(crate) fn format_dimensions_label(configured: usize) -> String {
    if configured == 0 {
        "unset".to_string()
    } else {
        configured.to_string()
    }
}

#[derive(serde::Serialize)]
struct SemanticDryRunReport {
    pub parse_threads: usize,
    pub parse_source: String,
    pub embed_concurrency: usize,
    pub requested_embed_concurrency: usize,
    pub embed_source: String,
    pub embed_concurrency_cap: usize,
    pub cap_source: String,
    pub candidate_file_count: usize,
    pub estimated_chunk_count: usize,
    pub embedding_model: String,
    pub embedding_model_source: String,
    pub embedding_dimensions: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stored_dimensions: Option<usize>,
    pub dimension_mismatch: bool,
    pub hnsw_rebuild_threshold: usize,
    pub would_trigger_hnsw_rebuild: bool,
    pub current_vector_count: usize,
    pub stored_vector_file_count: usize,
    pub current_file_count: usize,
    pub orphaned_hash_count: usize,
    pub orphaned_vector_file_count: usize,
    pub missing_hash_count: usize,
}

#[cfg(test)]
mod chunk_failures_tests;
#[cfg(test)]
mod tests;
