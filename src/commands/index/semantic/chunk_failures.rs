//! 0545: stored rows for files that read but fail `AstChunker::chunk_file`.
//!
//! Such a file never reaches `successful_files`, so its old `semantic_file_hash`
//! row and snippets would otherwise survive and `index --check` would count it
//! drifted on every run, with a refresh hint that cannot clear it. When the
//! content changed (stored hash absent or different) the rows describe content
//! that is gone, so they are cleared and the file is retried each run. When the
//! stored hash is still current the content did not change (a grammar or
//! chunker regression), so the valid index is kept.
//!
//! 0553: the decision is tri-state. When the stored-hash lookup itself fails the
//! outcome is `lookup-unknown`, not stale: the stored hash and snippets are kept
//! and no `clear_rows` call is made, because a read failure must never authorize
//! deletion. Only a known stale or absent stored hash clears rows; `clear_rows`
//! keeps its snippets-then-hash order.

use super::stages::SemanticFileError;
use crate::semantic::SemanticDiscovery;
use crate::util::path::semantic_path_key;
use std::path::Path;
use tracing::{debug, warn};

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
    /// The stored-hash lookup failed; rows were kept and the file is retried when
    /// the lookup succeeds.
    pub(crate) lookup_unknown: usize,
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

/// Apply the drop rule to every failure with an injected hash lookup. Idempotent.
///
/// The lookup is tri-state: `Ok(Some(hash))` equal to the failing content keeps the
/// rows, `Ok(Some(other))` and `Ok(None)` clear them, and `Err` keeps them (0553) -
/// a read failure must never authorize deletion.
pub(crate) fn drop_failed_chunk_rows_with(
    semantic: &SemanticDiscovery<'_>,
    failures: &[ChunkFailure],
    lookup: impl Fn(&str) -> miette::Result<Option<String>>,
) -> ChunkDropOutcome {
    let mut outcome = ChunkDropOutcome::default();
    for failure in failures {
        match lookup(&failure.key) {
            Ok(Some(stored)) if stored == failure.content_hash => {
                warn!(
                    "{} failed to chunk but its content is unchanged; keeping its stored index",
                    failure.key
                );
                outcome.kept_current += 1;
                continue;
            }
            Ok(_) => {}
            Err(e) => {
                // Unknown, not stale: keep both rows and report the aggregate only.
                debug!(
                    "Hash lookup failed for {}; keeping stored hash and snippets: {e}",
                    failure.key
                );
                outcome.lookup_unknown += 1;
                continue;
            }
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
    if let Some(notice) = lookup_unknown_notice(outcome.lookup_unknown) {
        warn!("{notice}");
    }
    outcome
}

/// Apply the drop rule to every failure. Idempotent.
pub(crate) fn drop_failed_chunk_rows(
    semantic: &SemanticDiscovery<'_>,
    failures: &[ChunkFailure],
) -> ChunkDropOutcome {
    drop_failed_chunk_rows_with(semantic, failures, |key| semantic.stored_file_hash(key))
}

/// One bounded aggregate line for unknown lookups; `None` at zero. No file key and
/// no error text, so a store-wide read failure stays a single WARN per run.
pub(crate) fn lookup_unknown_notice(count: usize) -> Option<String> {
    if count == 0 {
        return None;
    }
    Some(format!(
        "Semantic index: {count} file(s) could not be checked for a current stored hash; \
         not deleting their stored hash or snippets."
    ))
}

impl ChunkDropOutcome {
    /// Total in-root chunk failures this run; the four classes are disjoint.
    pub(crate) fn total(&self) -> usize {
        self.cleared + self.kept_current + self.failed + self.lookup_unknown
    }
}

/// Human line for the in-root chunk-error files; `None` at zero. Reports what
/// cleanup actually did, per outcome class, in a fixed order. "Retried on the
/// next run" means the file re-enters processing (no stored hash matches), not
/// that the chunk error itself is resolved. Kept-current and unknown-lookup
/// files get a fact, not a retry promise: a kept-current file is normally
/// skipped by later incremental runs.
pub(crate) fn chunk_failure_progress_line(outcome: &ChunkDropOutcome) -> Option<String> {
    let total = outcome.total();
    if total == 0 {
        return None;
    }
    let mut parts = Vec::new();
    if outcome.cleared > 0 {
        parts.push(format!(
            "{} cleared and retried on the next run",
            outcome.cleared
        ));
    }
    if outcome.failed > 0 {
        parts.push(format!(
            "{} cleanup failed and retried on the next run",
            outcome.failed
        ));
    }
    if outcome.kept_current > 0 {
        parts.push(format!(
            "{} kept because the content is unchanged",
            outcome.kept_current
        ));
    }
    if outcome.lookup_unknown > 0 {
        parts.push(format!(
            "{} unchecked because the stored-hash lookup failed",
            outcome.lookup_unknown
        ));
    }
    Some(format!(
        "Semantic index: {total} file(s) failed to chunk: {}.",
        parts.join("; ")
    ))
}
