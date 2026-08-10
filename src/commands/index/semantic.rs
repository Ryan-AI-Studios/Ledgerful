use crate::config::model::Config;
use crate::semantic::SemanticDiscovery;
use crate::semantic::concurrency::EmbedSemaphore;
use crate::state::layout::Layout;
use crate::state::storage::StorageManager;
use crate::util::path::{path_is_under_work_root, resolve_under_work_root, semantic_path_key};
use blake3;
use indicatif::{ProgressBar, ProgressStyle};
use miette::Result;
use rayon::prelude::*;
use serde::Serialize;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use tracing::{debug, info, warn};

type ParsedSemanticFile = (
    std::path::PathBuf,
    String,
    Vec<crate::semantic::chunker::AstChunk>,
);
type ParsedSemanticFileResult = std::result::Result<ParsedSemanticFile, String>;
const SEMANTIC_EMBEDDING_BATCH_SIZE: usize = 8;

/// Product progress lines (0148 / 0161): never via filterable tracing INFO.
/// Suppressed under `--json` so machine stdout stays pure (B6).
fn emit_semantic_progress(json: bool, message: &str) {
    if !json {
        println!("{message}");
    }
}

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
struct SemanticIndexJsonResult {
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

/// Non-TTY mid-phase report stride: ~total/20 ticks (no upper clamp).
/// Poller only starts when total > 1 (caller gate).
fn non_tty_progress_step(total: usize) -> usize {
    (total / 20).max(1)
}

/// Non-TTY mid-phase counters: AtomicUsize + background poller (no println in Rayon).
struct NonTtyPhaseProgress {
    counter: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl NonTtyPhaseProgress {
    fn start(label: &'static str, total: usize, unit: &'static str, json: bool) -> Self {
        let counter = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let handle = if !json && !crate::util::term::is_interactive() && total > 1 {
            let c = Arc::clone(&counter);
            let s = Arc::clone(&stop);
            // Throttle: every ~total/20 files (no upper clamp), or ~20s wall.
            let step = non_tty_progress_step(total);
            Some(std::thread::spawn(move || {
                let interval = Duration::from_secs(20);
                let mut last_n = 0usize;
                let mut last_t = Instant::now();
                while !s.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(500));
                    let n = c.load(Ordering::Relaxed);
                    if n == 0 {
                        continue;
                    }
                    if n >= total {
                        break;
                    }
                    if n > last_n
                        && (n.saturating_sub(last_n) >= step || last_t.elapsed() >= interval)
                    {
                        println!("Semantic index: {label} {n}/{total} {unit}…");
                        last_n = n;
                        last_t = Instant::now();
                    }
                }
            }))
        } else {
            None
        };
        Self {
            counter,
            stop,
            handle,
        }
    }

    fn finish(self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle {
            let _ = handle.join();
        }
    }
}

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

    emit_semantic_progress(json, &format!("Semantic index: parsing 0/{total} files…"));

    let hide_bars = json || !crate::util::term::is_interactive();
    let pb_parse = ProgressBar::new(total as u64);
    if hide_bars {
        pb_parse.set_draw_target(indicatif::ProgressDrawTarget::hidden());
    }
    pb_parse.set_style(
        ProgressStyle::with_template(
            "  {spinner:.cyan} Parsing [{bar:40.cyan/dim}] {pos}/{len} files  {elapsed_precise}",
        )
        .unwrap_or_else(|_| ProgressStyle::with_template("{pos}/{len}").unwrap())
        .progress_chars("█▓░"),
    );
    if !hide_bars {
        pb_parse.enable_steady_tick(std::time::Duration::from_millis(80));
    }

    let parse_progress = NonTtyPhaseProgress::start("parsing", total, "files", json);
    let parse_counter = Arc::clone(&parse_progress.counter);

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
                parse_counter.fetch_add(1, Ordering::Relaxed);
                res
            })
            .collect()
    });
    parse_progress.finish();
    pb_parse.finish_and_clear();
    emit_semantic_progress(
        json,
        &format!("Semantic index: parsing done {total} files…"),
    );

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
        emit_semantic_progress(
            json,
            &format!("Semantic index: embedding 0/{chunks_to_embed} chunks…"),
        );

        let pb_embed = ProgressBar::new(flat_chunks.len() as u64);
        if hide_bars {
            pb_embed.set_draw_target(indicatif::ProgressDrawTarget::hidden());
        }
        pb_embed.set_style(
            ProgressStyle::with_template(
                "  {spinner:.cyan} Embedding [{bar:40.green/dim}] {pos}/{len} chunks  {elapsed_precise}",
            )
            .unwrap_or_else(|_| ProgressStyle::with_template("{pos}/{len}").unwrap())
            .progress_chars("█▓░"),
        );
        if !hide_bars {
            pb_embed.enable_steady_tick(std::time::Duration::from_millis(80));
        }

        let chunk_batches: Vec<Vec<crate::semantic::chunker::AstChunk>> =
            semantic_embedding_batches(&flat_chunks, SEMANTIC_EMBEDDING_BATCH_SIZE);

        let pb_embed_ref = pb_embed.clone();
        let embed_sem_ref = embed_semaphore.clone();
        let embed_progress =
            NonTtyPhaseProgress::start("embedding", chunks_to_embed, "chunks", json);
        let embed_counter = Arc::clone(&embed_progress.counter);

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
                    let n = batch.len();
                    pb_embed_ref.inc(n as u64);
                    embed_counter.fetch_add(n, Ordering::Relaxed);
                    embedder_res
                })
                .collect()
        });

        embed_progress.finish();
        pb_embed.finish_and_clear();

        match embedding_results {
            Ok(batches) => {
                emit_semantic_progress(
                    json,
                    &format!("Semantic index: embedding done {chunks_to_embed} chunks…"),
                );
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
        for (path_key, _) in &successful_files {
            if let Err(e) = semantic.remove_file_snippets(path_key) {
                warn!("Failed to prune stale snippets for {path_key}: {e}");
            }
        }
    }

    let mut hnsw_rebuilt = false;
    if !flat_chunks.is_empty() {
        let threshold = config.semantic.hnsw_rebuild_threshold();
        let skip_hnsw = config.local_model.disable_hnsw;
        let will_rebuild = !skip_hnsw && flat_chunks.len() >= threshold;
        // C7: announce total store size (after prune + new), not batch-only.
        let count_after_prune = semantic.get_vector_count().unwrap_or(0);
        let total_after = count_after_prune.saturating_add(flat_chunks.len());
        if will_rebuild {
            emit_semantic_progress(
                json,
                &format!(
                    "Semantic index: rebuilding HNSW (~{total_after} total snippets; batch {} new) (may take several minutes)…",
                    flat_chunks.len()
                ),
            );
            hnsw_rebuilt = true;
        } else {
            emit_semantic_progress(
                json,
                &format!("Semantic index: ingesting {} snippets…", flat_chunks.len()),
            );
        }

        let spinner = ProgressBar::new_spinner();
        if hide_bars {
            spinner.set_draw_target(indicatif::ProgressDrawTarget::hidden());
        }
        spinner.set_style(
            ProgressStyle::with_template(
                "  {spinner:.yellow} Building HNSW index… {elapsed_precise}",
            )
            .unwrap_or_else(|_| ProgressStyle::default_spinner()),
        );
        if !hide_bars {
            spinner.enable_steady_tick(std::time::Duration::from_millis(100));
        }

        info!(
            "Ingesting {} snippets into vector store...",
            flat_chunks.len()
        );
        let chunk_count = flat_chunks.len();
        semantic.index_chunks_batched(flat_chunks, all_embeddings)?;
        spinner.finish_and_clear();
        emit_semantic_progress(
            json,
            &format!("Semantic index: ingest done ({chunk_count} snippets)…"),
        );
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

    // ── 0167 non-TTY progress step (0161-A) ─────────────────────────────────
    // Poller only starts when total > 1 (caller gate in NonTtyPhaseProgress::start).

    #[test]
    fn non_tty_progress_step_matrix() {
        assert_eq!(non_tty_progress_step(0), 1);
        assert_eq!(non_tty_progress_step(1), 1);
        assert_eq!(non_tty_progress_step(2), 1);
        assert_eq!(non_tty_progress_step(20), 1);
        assert_eq!(non_tty_progress_step(25), 1);
        assert_eq!(non_tty_progress_step(100), 5);
        assert_eq!(non_tty_progress_step(500), 25);
        // Large totals must not clamp to 25 (0161 flood: ~522 mid-lines).
        assert_eq!(non_tty_progress_step(13_065), 653);
        assert_ne!(non_tty_progress_step(13_065), 25);
    }

    // ── 0161 mode resolution matrix ────────────────────────────────────────

    #[test]
    fn resolve_mode_cold_neither_is_full_cold_store() {
        let (incr, reason) = resolve_semantic_index_mode(false, false, 0);
        assert!(!incr);
        assert_eq!(reason, "cold-store");
    }

    #[test]
    fn resolve_mode_warm_neither_is_auto_incremental() {
        let (incr, reason) = resolve_semantic_index_mode(false, false, 42);
        assert!(incr);
        assert_eq!(reason, "auto-incremental");
    }

    /// Pure cold-store alias of `resolve_mode_cold_neither_is_full_cold_store`.
    ///
    /// Documents the wipe-edge product rule: warm SoT is `vector_count > 0` only.
    /// Orphan file hashes with zero vectors still resolve to full / `cold-store`
    /// (not a separate wipe-edge mode string).
    #[test]
    fn resolve_mode_wipe_edge_orphan_hashes_is_pure_cold_store_alias() {
        let (incr, reason) = resolve_semantic_index_mode(false, false, 0);
        assert!(!incr);
        assert_eq!(reason, "cold-store");
    }

    #[test]
    fn resolve_mode_full_wins_over_incremental_and_warm() {
        let (incr, reason) = resolve_semantic_index_mode(true, true, 999);
        assert!(!incr);
        assert_eq!(reason, "--full");
    }

    #[test]
    fn resolve_mode_explicit_incremental() {
        let (incr, reason) = resolve_semantic_index_mode(false, true, 0);
        assert!(incr);
        assert_eq!(reason, "explicit-incremental");
        let (incr_warm, reason_warm) = resolve_semantic_index_mode(false, true, 10);
        assert!(incr_warm);
        assert_eq!(reason_warm, "explicit-incremental");
    }

    #[test]
    fn resolve_mode_vector_count_zero_never_panics() {
        // unwrap_or(0) path: callers pass 0 on error; pure fn must not panic.
        let _ = resolve_semantic_index_mode(false, false, 0);
        let _ = resolve_semantic_index_mode(false, false, usize::MAX);
    }

    /// 0161 Codex R1 / B7: production `get_vector_count().unwrap_or(0)` must resolve
    /// cold-store (full) without panic — count Err and missing relation both map to 0.
    #[test]
    fn count_error_or_zero_treated_as_cold_store_no_panic() {
        // Production call site (execute_semantic_index):
        //   let vector_count = semantic.get_vector_count().unwrap_or(0);
        //   resolve_semantic_index_mode(..., vector_count)
        // Same soft-fail contract as production unwrap_or(0).
        fn soft_vector_count(count: Result<usize, miette::Report>) -> usize {
            count.unwrap_or(0)
        }
        let vector_count = soft_vector_count(Err(miette::miette!("cozo count failed")));
        assert_eq!(vector_count, 0);
        let (incremental, reason) = resolve_semantic_index_mode(false, false, vector_count);
        assert!(
            !incremental,
            "count error must not warm-skip into incremental"
        );
        assert_eq!(reason, "cold-store");
        // Ok(0) / missing-relation path also resolves cold.
        assert_eq!(soft_vector_count(Ok(0)), 0);
        let (incr0, reason0) = resolve_semantic_index_mode(false, false, soft_vector_count(Ok(0)));
        assert!(!incr0);
        assert_eq!(reason0, "cold-store");
        // Bounds: huge counts still pure / non-panicking.
        let _ = resolve_semantic_index_mode(false, false, usize::MAX);
    }

    /// 0161 Codex R1 / B7: orphan file hashes + zero vectors ⇒ full/`cold-store` on the
    /// real post-purge execute path, and C1 filter re-processes (not silent up-to-date).
    ///
    /// Mirrors `execute_semantic_index` order: purge → get_vector_count → resolve → filter.
    #[test]
    fn wipe_edge_orphan_hashes_execute_path_cold_store_and_reprocesses() {
        use crate::config::model::LocalModelConfig;
        use crate::semantic::SemanticDiscovery;
        use crate::state::storage_cozo::CozoStorage;

        let storage = CozoStorage::new_in_memory().expect("cozo");
        let config = LocalModelConfig {
            dimensions: 3,
            disable_hnsw: true,
            ..Default::default()
        };
        let semantic = SemanticDiscovery::new(config, &storage).expect("semantic");
        semantic.ensure_file_hash_schema().expect("hash schema");

        // Plant orphan hash rows with zero snippet_embedding vectors (dim-wipe edge).
        let path_key = "src/orphan.rs";
        let content_hash = "deadbeef_hash_only_no_vectors";
        semantic
            .record_file_hash(path_key, content_hash)
            .expect("record orphan hash");
        assert!(
            semantic.is_file_hash_current(path_key, content_hash),
            "precondition: hash must be current"
        );
        assert!(
            !semantic.file_has_snippets(path_key),
            "precondition: no snippet rows for orphan hash"
        );

        // Execute-path order: dual foreign purge BEFORE warm detection.
        let work_root = tempfile::tempdir().expect("work root");
        let purged = semantic
            .purge_foreign_semantic_keys(work_root.path())
            .expect("purge");
        assert_eq!(
            purged, 0,
            "relative orphan hash is not foreign; purge must not remove it"
        );

        // Warm SoT = vector_count > 0 after purge (safe 0 on error — never panic).
        let vector_count = semantic.get_vector_count().unwrap_or(0);
        assert_eq!(
            vector_count, 0,
            "hash-only store must report zero vectors (not warm)"
        );

        let (incremental, reason) = resolve_semantic_index_mode(false, false, vector_count);
        assert!(
            !incremental,
            "orphan hashes + empty vectors must full-bootstrap, not auto-incremental"
        );
        assert_eq!(reason, "cold-store");

        // C1 filter (incremental branch): skip only when hash-current AND has snippets.
        // Hash-only must re-process even under forced incremental.
        let would_skip = semantic.is_file_hash_current(path_key, content_hash)
            && semantic.file_has_snippets(path_key);
        assert!(
            !would_skip,
            "hash-current without snippets must re-process (not silent up-to-date)"
        );
    }

    #[test]
    fn semantic_json_result_omits_zero_purge_and_false_hnsw() {
        let result = SemanticIndexJsonResult {
            schema_version: 1,
            mode: "incremental",
            reason: "auto-incremental",
            files_processed: 0,
            files_candidates: 3,
            chunks_embedded: 0,
            purged_foreign: None,
            up_to_date: true,
            hnsw_rebuilt: None,
        };
        let s = serde_json::to_string(&result).expect("serialize");
        assert!(s.contains("\"schemaVersion\":1"));
        assert!(s.contains("\"upToDate\":true"));
        assert!(!s.contains("purgedForeign"));
        assert!(!s.contains("hnswRebuilt"));
        // Pure single object — no human prose.
        assert!(s.starts_with('{'));
        assert!(s.ends_with('}'));
    }

    #[test]
    fn semantic_json_result_includes_purge_and_hnsw_when_set() {
        let result = SemanticIndexJsonResult {
            schema_version: 1,
            mode: "full",
            reason: "--full",
            files_processed: 2,
            files_candidates: 2,
            chunks_embedded: 10,
            purged_foreign: Some(3),
            up_to_date: false,
            hnsw_rebuilt: Some(true),
        };
        let s = serde_json::to_string(&result).expect("serialize");
        assert!(s.contains("\"purgedForeign\":3"));
        assert!(s.contains("\"hnswRebuilt\":true"));
        assert!(s.contains("\"mode\":\"full\""));
    }

    /// DoD-1: unconfigured backend refuses before any semantic write.
    #[test]
    fn execute_semantic_index_refuses_when_unconfigured() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = camino::Utf8Path::from_path(tmp.path()).expect("utf8 path");
        let layout = Layout::new(root);
        layout.ensure_state_dir().expect("state dir");
        let db_path = layout.state_subdir().join("ledger.db");
        let storage = StorageManager::init(db_path.as_std_path()).expect("storage init");

        // Default Config has empty embedding URL — not configured.
        let config = Config::default();
        assert!(
            !crate::embed::client::is_embedding_backend_configured(&config.local_model),
            "precondition: default config must be unconfigured"
        );

        let err = execute_semantic_index(&layout, storage, &config, false, false, None, false)
            .expect_err("must refuse when unconfigured");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("embedding backend") || msg.contains("base_url"),
            "error must name backend/config requirement: {msg}"
        );
        assert!(
            msg.contains("semantic-dry-run"),
            "error must point at dry-run inspect: {msg}"
        );
    }
}
