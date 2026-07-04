use crate::config::model::Config;
use crate::semantic::SemanticDiscovery;
use crate::semantic::concurrency::EmbedSemaphore;
use crate::state::layout::Layout;
use crate::state::storage::StorageManager;
use blake3;
use indicatif::{ProgressBar, ProgressStyle};
use miette::Result;
use rayon::prelude::*;
use std::sync::Arc;
use tracing::{info, warn};

type ParsedSemanticFile = (
    std::path::PathBuf,
    String,
    Vec<crate::semantic::chunker::AstChunk>,
);
type ParsedSemanticFileResult = std::result::Result<ParsedSemanticFile, String>;
const SEMANTIC_EMBEDDING_BATCH_SIZE: usize = 8;

fn semantic_embedding_batches(
    chunks: &[crate::semantic::chunker::AstChunk],
    batch_size: usize,
) -> Vec<Vec<crate::semantic::chunker::AstChunk>> {
    debug_assert!(batch_size > 0);
    chunks
        .chunks(batch_size)
        .map(|batch| batch.to_vec())
        .collect()
}

/// Resolve parse and embed concurrency from CLI override, semantic config,
/// and local-model defaults. Used by both semantic index and dry-run.
pub(crate) fn resolve_semantic_concurrency(
    concurrency_override: Option<usize>,
    config: &Config,
) -> crate::semantic::concurrency::ResolvedConcurrency {
    use crate::semantic::concurrency::{ResolveOptions, resolve_split_semantic_concurrency};
    let available_parallelism = std::thread::available_parallelism()
        .ok()
        .map(|n| std::num::NonZeroUsize::new(n.get()).expect("available_parallelism is non-zero"));
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
pub(crate) fn walk_repo_for_semantic_files(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    fn walk_dir(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if matches!(
                    name,
                    ".git"
                        | ".ledgerful"
                        | "target"
                        | "node_modules"
                        | ".agents"
                        | ".claude"
                        | ".codex"
                        | ".opencode"
                ) {
                    continue;
                }
                walk_dir(&path, out);
            } else {
                let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
                if matches!(ext, "rs" | "ts" | "tsx" | "js" | "jsx" | "py" | "go") {
                    out.push(path);
                }
            }
        }
    }
    let mut out = Vec::new();
    walk_dir(root, &mut out);
    out
}

pub(crate) fn execute_semantic_index(
    layout: &Layout,
    storage: StorageManager,
    config: &Config,
    incremental: bool,
    concurrency_override: Option<usize>,
) -> Result<()> {
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

    info!(
        "Semantic indexing started: incremental={incremental}, cli_concurrency={:?}",
        concurrency_override
    );
    info!("Semantic indexing threads: parse={parse_threads}, embed_concurrency={embed_cap}");

    info!("Indexing repository for semantic search...");

    // ── Phase 1: Collect candidate files ───────────────────────────────────
    let repo_root = layout.root.as_std_path();
    let candidate_paths = walk_repo_for_semantic_files(repo_root);

    // HP3: On incremental runs filter to only files whose hash has changed.
    let files_to_process: Vec<std::path::PathBuf> = if incremental {
        let tracked_files = semantic.get_tracked_files()?;
        for tracked in tracked_files {
            let path = std::path::Path::new(&tracked);
            if !path.exists() {
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
                    return true; // re-try unreadable files
                };
                let hash = blake3::hash(content.as_bytes()).to_hex().to_string();
                !semantic.is_file_hash_current(path, &hash)
            })
            .collect()
    } else {
        // Full index: prune snippets for files that no longer exist
        semantic.prune_deleted_snippets(repo_root)?;
        candidate_paths
    };

    if files_to_process.is_empty() {
        info!("Semantic index is up to date: no files changed since last index");
        return Ok(());
    }

    info!(
        "Semantic indexing will process {} files",
        files_to_process.len()
    );

    // ── Phase 2: Configure Rayon thread pool (U13/U14) ──────────────────────

    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(parse_threads)
        .build()
        .map_err(|e| miette::miette!("Failed to build Rayon thread pool: {}", e))?;

    let embed_semaphore = Arc::new(EmbedSemaphore::new(embed_cap));

    // ── Phase 3: Parallel parse + embed with progress bar (HP2 + HP4) ──────
    let total = files_to_process.len();

    let pb_parse = ProgressBar::new(total as u64);
    if !crate::util::term::is_interactive() {
        pb_parse.set_draw_target(indicatif::ProgressDrawTarget::hidden());
    }
    pb_parse.set_style(
        ProgressStyle::with_template(
            "  {spinner:.cyan} Parsing [{bar:40.cyan/dim}] {pos}/{len} files  {elapsed_precise}",
        )
        .unwrap_or_else(|_| ProgressStyle::with_template("{pos}/{len}").unwrap())
        .progress_chars("█▓░"),
    );
    pb_parse.enable_steady_tick(std::time::Duration::from_millis(80));

    let parsed_files_res: Vec<ParsedSemanticFileResult> = pool.install(|| {
        files_to_process
            .into_par_iter()
            .map(|path| {
                let res = match crate::util::fs::read_to_string_with_encoding(&path) {
                    Ok(content) => {
                        match crate::semantic::chunker::AstChunker::chunk_file(&path, &content) {
                            Ok(chunks) => Ok((path, content, chunks)),
                            Err(e) => Err(format!("{}: {}", path.display(), e)),
                        }
                    }
                    Err(e) => Err(format!("{}: {}", path.display(), e)),
                };
                pb_parse.inc(1);
                res
            })
            .collect()
    });
    pb_parse.finish_and_clear();

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

    // Flatten chunks
    let mut flat_chunks = Vec::new();
    let mut successful_files = Vec::new();
    for (path, content, chunks) in parsed_files {
        let hash = blake3::hash(content.as_bytes()).to_hex().to_string();
        successful_files.push((path.clone(), hash));
        for chunk in chunks {
            flat_chunks.push(chunk);
        }
    }

    let files_indexed_count = successful_files.len();

    // Batch embedding generation
    let mut all_embeddings = Vec::new();
    if !flat_chunks.is_empty() {
        let pb_embed = ProgressBar::new(flat_chunks.len() as u64);
        if !crate::util::term::is_interactive() {
            pb_embed.set_draw_target(indicatif::ProgressDrawTarget::hidden());
        }
        pb_embed.set_style(
            ProgressStyle::with_template(
                "  {spinner:.cyan} Embedding [{bar:40.green/dim}] {pos}/{len} chunks  {elapsed_precise}",
            )
            .unwrap_or_else(|_| ProgressStyle::with_template("{pos}/{len}").unwrap())
            .progress_chars("█▓░"),
        );
        pb_embed.enable_steady_tick(std::time::Duration::from_millis(80));

        let chunk_batches: Vec<Vec<crate::semantic::chunker::AstChunk>> =
            semantic_embedding_batches(&flat_chunks, SEMANTIC_EMBEDDING_BATCH_SIZE);

        let pb_embed_ref = pb_embed.clone();
        let embed_sem_ref = embed_semaphore.clone();
        let embedding_results: Result<Vec<Vec<Vec<f32>>>, String> = pool.install(|| {
            chunk_batches
                .into_par_iter()
                .map(|batch| {
                    let _permit = embed_sem_ref.acquire();
                    let texts: Vec<String> = batch.iter().map(|c| c.to_embedding_text()).collect();
                    let text_refs: Vec<&str> = texts.iter().map(|s| s.as_str()).collect();
                    let embedder_res = semantic
                        .embedder
                        .embed_batch(&text_refs)
                        .map_err(|e| e.to_string());
                    pb_embed_ref.inc(batch.len() as u64);
                    embedder_res
                })
                .collect()
        });

        pb_embed.finish_and_clear();

        match embedding_results {
            Ok(batches) => {
                for batch in batches {
                    all_embeddings.extend(batch);
                }
            }
            Err(e) => {
                return Err(miette::miette!("Embedding generation failed: {}", e));
            }
        }
    }

    // ── Phase 4: Batch ingest into CozoDB (single-threaded for safety) ─────
    if !successful_files.is_empty() {
        info!("Pruning stale semantic database rows...");
        for (path, _) in &successful_files {
            let path_str = path.to_string_lossy();
            if let Err(e) = semantic.remove_file_snippets(&path_str) {
                warn!(
                    "Failed to prune stale snippets for {}: {}",
                    path.display(),
                    e
                );
            }
        }
    }

    if !flat_chunks.is_empty() {
        let spinner = ProgressBar::new_spinner();
        if !crate::util::term::is_interactive() {
            spinner.set_draw_target(indicatif::ProgressDrawTarget::hidden());
        }
        spinner.set_style(
            ProgressStyle::with_template(
                "  {spinner:.yellow} Building HNSW index… {elapsed_precise}",
            )
            .unwrap_or_else(|_| ProgressStyle::default_spinner()),
        );
        spinner.enable_steady_tick(std::time::Duration::from_millis(100));

        info!(
            "Ingesting {} snippets into vector store...",
            flat_chunks.len()
        );
        semantic.index_chunks_batched(flat_chunks, all_embeddings)?;
        spinner.finish_and_clear();
    }

    // Record new hashes only for successfully processed files
    for (path, hash) in successful_files {
        if let Err(e) = semantic.record_file_hash(&path, &hash) {
            warn!("Failed to record file hash for {}: {}", path.display(), e);
        }
    }

    println!(
        "Semantic indexing complete: {files_indexed_count}/{total} files produced embeddings{}.",
        if incremental { " (incremental)" } else { "" }
    );
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
        .map(|db| {
            let relations = db.get_relations().unwrap_or_default();
            if !relations.contains(&"snippet_embedding".to_string()) {
                return 0;
            }
            let script = "?[count(file_path)] := *snippet_embedding{file_path}";
            if let Ok(res) = db.run_script(script)
                && let Some(row) = res.rows.first()
                && let Some(cozo::DataValue::Num(cozo::Num::Int(count))) = row.first()
            {
                *count as usize
            } else {
                0
            }
        })
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
mod tests {
    use super::*;
    use crate::index::symbols::SymbolKind;
    use crate::semantic::chunker::AstChunk;

    fn chunk(name: &str) -> AstChunk {
        AstChunk {
            file_path: "src/lib.rs".to_string(),
            name: name.to_string(),
            kind: SymbolKind::Function,
            content: format!("fn {name}() {{}}"),
            docstring: None,
            range: (0, 0),
            lines: (1, 1),
            offset: 0,
        }
    }

    #[test]
    fn semantic_embedding_batches_preserve_order() {
        let chunks: Vec<AstChunk> = (0..10).map(|i| chunk(&format!("chunk_{i}"))).collect();

        let batches = semantic_embedding_batches(&chunks, 4);
        let flattened_names: Vec<&str> = batches
            .iter()
            .flat_map(|batch| batch.iter().map(|chunk| chunk.name.as_str()))
            .collect();

        assert_eq!(batches.len(), 3);
        assert_eq!(batches[0].len(), 4);
        assert_eq!(batches[1].len(), 4);
        assert_eq!(batches[2].len(), 2);
        assert_eq!(
            flattened_names,
            chunks
                .iter()
                .map(|chunk| chunk.name.as_str())
                .collect::<Vec<_>>()
        );
    }
}
