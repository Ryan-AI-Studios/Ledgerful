mod progress;
mod stages;

use crate::config::model::Config;
use crate::semantic::SemanticDiscovery;
use crate::semantic::concurrency::EmbedSemaphore;
use crate::state::layout::Layout;
use crate::state::storage::StorageManager;
use crate::util::path::{path_is_under_work_root, resolve_under_work_root, semantic_path_key};
use miette::Result;
use serde::Serialize;
use std::sync::Arc;
use tracing::{debug, info, warn};

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

/// Walk the repository for candidate semantic-index files.
///
/// Uses `ignore::WalkBuilder` with `git_ignore(true)` (same class as
/// `RepoWalker`) so gitignored deps/headers do not flood the embed set (D3).
/// Fixed directory name skips remain for non-git noise.
pub(crate) fn walk_repo_for_semantic_files(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    const SKIP_DIRS: &[&str] = &[
        ".git",
        ".ledgerful",
        "target",
        "node_modules",
        ".agents",
        ".claude",
        ".codex",
        ".opencode",
    ];
    const SEMANTIC_EXTS: &[&str] = &[
        "rs", "ts", "tsx", "js", "jsx", "py", "go", //
        "c", "h", "cpp", "cc", "cxx", "hpp", "hh", "hxx", "h++",
    ];

    let mut out = Vec::new();
    let walker = ignore::WalkBuilder::new(root)
        .hidden(true)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .filter_entry(|entry| {
            if entry.file_type().is_some_and(|ft| ft.is_dir()) {
                let name = entry.file_name().to_string_lossy();
                return !SKIP_DIRS.iter().any(|s| *s == name);
            }
            true
        })
        .build();

    for entry in walker {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!("Error walking directory for semantic files: {e}");
                continue;
            }
        };
        if !entry.file_type().is_some_and(|ft| ft.is_file()) {
            continue;
        }
        let path = entry.path();
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        if SEMANTIC_EXTS.contains(&ext) {
            out.push(path.to_path_buf());
        }
    }
    out.sort();
    out
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
        .cozo
        .as_ref()
        .ok_or_else(|| miette::miette!("CozoDB storage not initialized"))?;

    let semantic = SemanticDiscovery::new_with_semantic_config(
        config.local_model.clone(),
        config.semantic.clone(),
        cozo,
    )?;

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
                let hash = blake3::hash(content.as_bytes()).to_hex().to_string();
                let Ok(key) = semantic_path_key(repo_root, path) else {
                    return true;
                };
                // Skip only if hash-current AND has ≥1 snippet row.
                !(semantic.is_file_hash_current(&key, &hash) && semantic.file_has_snippets(&key))
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
        let hash = blake3::hash(content.as_bytes()).to_hex().to_string();
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

    let current_vector_count = cozo
        .as_ref()
        .map(|db| crate::semantic::vector_store::count_snippet_embedding_rows(db).unwrap_or(0))
        .unwrap_or(0);

    let current_file_count = cozo
        .as_ref()
        .map(|db| {
            let relations = db.get_relations().unwrap_or_default();
            if !relations.contains(&"semantic_file_hash".to_string()) {
                return 0;
            }
            let script = "?[file_path] := *semantic_file_hash{file_path}";
            db.run_script(script).map(|res| res.rows.len()).unwrap_or(0)
        })
        .unwrap_or(0);

    let hnsw_rebuild_threshold = config.semantic.hnsw_rebuild_threshold();
    let would_trigger_hnsw_rebuild = estimated_chunk_count > hnsw_rebuild_threshold;

    let embedding_dimensions = config.local_model.dimensions;

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
        embedding_dimensions,
        hnsw_rebuild_threshold,
        would_trigger_hnsw_rebuild,
        current_vector_count,
        current_file_count,
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
            "File walk of repository",
        ]);
        table.add_row(vec![
            "Estimated Chunks",
            &report.estimated_chunk_count.to_string(),
            "Lines count / 30 approximation",
        ]);
        table.add_row(vec![
            "Embedding Model",
            &report.embedding_model,
            "config.local_model.embedding_model",
        ]);
        let dims_str = if report.embedding_dimensions == 0 {
            "0 (probed at runtime)".to_string()
        } else {
            report.embedding_dimensions.to_string()
        };
        table.add_row(vec![
            "Embedding Dimensions",
            &dims_str,
            "config.local_model.dimensions",
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
            "CozoDB vector store",
        ]);
        table.add_row(vec![
            "Current Files in DB",
            &report.current_file_count.to_string(),
            "CozoDB vector store",
        ]);
        println!("{table}");
    }

    Ok(())
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
    pub embedding_dimensions: usize,
    pub hnsw_rebuild_threshold: usize,
    pub would_trigger_hnsw_rebuild: bool,
    pub current_vector_count: usize,
    pub current_file_count: usize,
}

#[cfg(test)]
mod tests;
