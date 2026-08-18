use crate::config::model::Config;
use crate::semantic::SemanticDiscovery;
use crate::semantic::concurrency::EmbedSemaphore;
use indicatif::ProgressBar;
use rayon::prelude::*;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use tracing::{info, warn};

use super::progress::{
    NonTtyPhaseProgress, emit_semantic_progress, hide_semantic_progress_bars, semantic_bar_style,
};

pub(crate) type ParsedSemanticFile = (PathBuf, String, Vec<crate::semantic::chunker::AstChunk>);

#[derive(Debug, thiserror::Error)]
pub(crate) enum SemanticFileError {
    #[error("{path}: {message}")]
    Read { path: String, message: String },
    #[error("{path}: {message}")]
    Chunk { path: String, message: String },
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum SemanticEmbedError {
    #[error("{0}")]
    Batch(String),
}

const SEMANTIC_EMBEDDING_BATCH_SIZE: usize = 8;

pub(crate) fn semantic_embedding_batches(
    chunks: &[crate::semantic::chunker::AstChunk],
    batch_size: usize,
) -> Vec<Vec<crate::semantic::chunker::AstChunk>> {
    debug_assert!(batch_size > 0);
    chunks
        .chunks(batch_size)
        .map(|batch| batch.to_vec())
        .collect()
}

pub(crate) fn parse_semantic_files(
    pool: &rayon::ThreadPool,
    files_to_process: Vec<PathBuf>,
    json: bool,
) -> Vec<Result<ParsedSemanticFile, SemanticFileError>> {
    let total = files_to_process.len();
    emit_semantic_progress(json, &format!("Semantic index: parsing 0/{total} files…"));

    let hide_bars = hide_semantic_progress_bars(json, crate::util::term::is_interactive());
    let pb_parse = ProgressBar::new(total as u64);
    if hide_bars {
        pb_parse.set_draw_target(indicatif::ProgressDrawTarget::hidden());
    }
    pb_parse.set_style(
        semantic_bar_style(
            "  {spinner:.cyan} Parsing [{bar:40.cyan/dim}] {pos}/{len} files  {elapsed_precise}",
        )
        .progress_chars("█▓░"),
    );
    if !hide_bars {
        pb_parse.enable_steady_tick(std::time::Duration::from_millis(80));
    }

    let parse_progress = NonTtyPhaseProgress::start("parsing", total, "files", json);
    let parse_counter = Arc::clone(&parse_progress.counter);

    let parsed_files_res: Vec<Result<ParsedSemanticFile, SemanticFileError>> = pool.install(|| {
        files_to_process
            .into_par_iter()
            .map(|path| {
                let res = match crate::util::fs::read_to_string_with_encoding(&path) {
                    Ok(content) => {
                        match crate::semantic::chunker::AstChunker::chunk_file(&path, &content) {
                            Ok(chunks) => Ok((path, content, chunks)),
                            Err(e) => Err(SemanticFileError::Chunk {
                                path: path.display().to_string(),
                                message: e.to_string(),
                            }),
                        }
                    }
                    Err(e) => Err(SemanticFileError::Read {
                        path: path.display().to_string(),
                        message: e.to_string(),
                    }),
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
    parsed_files_res
}

pub(crate) fn embed_semantic_chunks(
    pool: &rayon::ThreadPool,
    semantic: &SemanticDiscovery,
    embed_semaphore: Arc<EmbedSemaphore>,
    flat_chunks: &[crate::semantic::chunker::AstChunk],
    json: bool,
) -> Result<Vec<Vec<Vec<f32>>>, SemanticEmbedError> {
    let chunks_to_embed = flat_chunks.len();
    emit_semantic_progress(
        json,
        &format!("Semantic index: embedding 0/{chunks_to_embed} chunks…"),
    );

    let hide_bars = hide_semantic_progress_bars(json, crate::util::term::is_interactive());
    let pb_embed = ProgressBar::new(flat_chunks.len() as u64);
    if hide_bars {
        pb_embed.set_draw_target(indicatif::ProgressDrawTarget::hidden());
    }
    pb_embed.set_style(
        semantic_bar_style(
            "  {spinner:.cyan} Embedding [{bar:40.green/dim}] {pos}/{len} chunks  {elapsed_precise}",
        )
        .progress_chars("█▓░"),
    );
    if !hide_bars {
        pb_embed.enable_steady_tick(std::time::Duration::from_millis(80));
    }

    let chunk_batches: Vec<Vec<crate::semantic::chunker::AstChunk>> =
        semantic_embedding_batches(flat_chunks, SEMANTIC_EMBEDDING_BATCH_SIZE);

    let pb_embed_ref = pb_embed.clone();
    let embed_sem_ref = embed_semaphore.clone();
    let embed_progress = NonTtyPhaseProgress::start("embedding", chunks_to_embed, "chunks", json);
    let embed_counter = Arc::clone(&embed_progress.counter);

    let embedding_results: Result<Vec<Vec<Vec<f32>>>, SemanticEmbedError> = pool.install(|| {
        chunk_batches
            .into_par_iter()
            .map(|batch| {
                let _permit = embed_sem_ref.acquire();
                let texts: Vec<String> = batch.iter().map(|c| c.to_embedding_text()).collect();
                let text_refs: Vec<&str> = texts.iter().map(|s| s.as_str()).collect();
                let embedder_res = semantic
                    .embedder
                    .embed_batch(&text_refs)
                    .map_err(|e| SemanticEmbedError::Batch(e.to_string()));
                let n = batch.len();
                pb_embed_ref.inc(n as u64);
                embed_counter.fetch_add(n, Ordering::Relaxed);
                embedder_res
            })
            .collect()
    });

    embed_progress.finish();
    pb_embed.finish_and_clear();
    embedding_results
}

pub(crate) fn persist_semantic_chunks(
    semantic: &SemanticDiscovery,
    config: &Config,
    successful_files: &[(String, String)],
    flat_chunks: Vec<crate::semantic::chunker::AstChunk>,
    all_embeddings: Vec<Vec<f32>>,
    json: bool,
) -> miette::Result<bool> {
    if !successful_files.is_empty() {
        info!("Pruning stale semantic database rows...");
        for (path_key, _) in successful_files {
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

        let hide_bars = hide_semantic_progress_bars(json, crate::util::term::is_interactive());
        let spinner = ProgressBar::new_spinner();
        if hide_bars {
            spinner.set_draw_target(indicatif::ProgressDrawTarget::hidden());
        }
        spinner.set_style(
            indicatif::ProgressStyle::with_template(
                "  {spinner:.yellow} Building HNSW index… {elapsed_precise}",
            )
            .unwrap_or_else(|_| indicatif::ProgressStyle::default_spinner()),
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

    Ok(hnsw_rebuilt)
}
