//! 0545: stored rows for files that read but fail `AstChunker::chunk_file`.
//!
//! Such a file never reaches `successful_files`, so its old `semantic_file_hash`
//! row and snippets would otherwise survive and `index --check` would count it
//! drifted on every run, with a refresh hint that cannot clear it. When the
//! content changed (stored hash absent or different) the rows describe content
//! that is gone, so they are cleared and the file is retried each run. When the
//! stored hash is still current the content did not change (a grammar or
//! chunker regression), so the valid index is kept.

use super::stages::SemanticFileError;
use crate::semantic::SemanticDiscovery;
use crate::util::path::semantic_path_key;
use std::path::Path;
use tracing::warn;

/// A chunk-error file with its work-root-relative key and the hash of the
/// content that failed to chunk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ChunkFailure {
    pub(crate) key: String,
    pub(crate) content_hash: String,
}

/// What the drop did, per chunk-error file.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ChunkDropOutcome {
    /// Rows cleared (stale or absent stored hash).
    pub(crate) cleared: usize,
    /// Stored hash still matched the failing content; rows kept.
    pub(crate) kept_current: usize,
    /// A removal failed; the file stays drifted and is retried.
    pub(crate) failed: usize,
}

/// Keep only `Chunk` errors (a `Read` error says nothing about the content) and
/// map each to its slash-normalized key. A path outside the work root cannot come
/// from the walk; it is skipped defensively with a warning.
pub(crate) fn failed_chunk_files(
    errors: &[SemanticFileError],
    repo_root: &Path,
) -> Vec<ChunkFailure> {
    let mut out = Vec::new();
    for err in errors {
        let SemanticFileError::Chunk {
            file, content_hash, ..
        } = err
        else {
            continue;
        };
        match semantic_path_key(repo_root, file) {
            Ok(key) => out.push(ChunkFailure {
                key,
                content_hash: content_hash.clone(),
            }),
            Err(_) => warn!(
                "Skipping chunk-failure row cleanup for path outside work root: {}",
                file.display()
            ),
        }
    }
    out
}

/// Remove snippets, then the hash only if the snippets were removed. The hash is
/// never removed alone: with no hash and stale snippets `index --check` would
/// report fresh. Returns whether both were removed.
pub(crate) fn clear_rows<E: std::fmt::Display>(
    key: &str,
    remove_snippets: impl FnOnce() -> Result<(), E>,
    remove_hash: impl FnOnce() -> Result<(), E>,
) -> bool {
    if let Err(e) = remove_snippets() {
        warn!("Failed to clear stale snippets for {key}; keeping its hash: {e}");
        return false;
    }
    if let Err(e) = remove_hash() {
        warn!("Failed to clear stale hash for {key}: {e}");
        return false;
    }
    true
}

/// Apply the drop rule to every failure. Idempotent.
pub(crate) fn drop_failed_chunk_rows(
    semantic: &SemanticDiscovery<'_>,
    failures: &[ChunkFailure],
) -> ChunkDropOutcome {
    let mut outcome = ChunkDropOutcome::default();
    for failure in failures {
        if semantic.is_file_hash_current(&failure.key, &failure.content_hash) {
            warn!(
                "{} failed to chunk but its content is unchanged; keeping its stored index",
                failure.key
            );
            outcome.kept_current += 1;
            continue;
        }
        let cleared = clear_rows(
            &failure.key,
            || semantic.remove_file_snippets(&failure.key),
            || semantic.remove_file_hash(&failure.key),
        );
        if cleared {
            outcome.cleared += 1;
        } else {
            outcome.failed += 1;
        }
    }
    outcome
}

/// Human line for `failed` in-root chunk-error files; `None` at zero. States
/// intent, not a completed mutation.
pub(crate) fn chunk_failure_progress_line(failed: usize) -> Option<String> {
    if failed == 0 {
        return None;
    }
    Some(format!(
        "Semantic index: {failed} file(s) failed to chunk and will be retried; \
         stored hash and snippets cleared where the content changed."
    ))
}
