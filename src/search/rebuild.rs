//! Full Tantivy rebuild helper (clear + whole-repo StreamIndexer).
//!
//! There is no incremental Tantivy update API. Callers rebuild only when
//! SQLite index work ran (`FullBootstrap` / `Incremental`) or the FTS floor is
//! empty (`document_count == 0`) / explicit `--index` / format stamp mismatch.

use crate::search::{StreamIndexer, TantivySearchEngine, write_stamp};
use crate::state::layout::Layout;
use miette::Result;

/// Clear the on-disk Tantivy index and re-index the full worktree.
///
/// Used by `ledgerful index` finish path and `search --auto-index` after
/// SQLite FullBootstrap/Incremental work. Writes the FTS format stamp on
/// success so tokenizer revisions (e.g. code_tokenizer_v2) do not silently
/// serve stale postings.
pub fn rebuild_tantivy_index(layout: &Layout) -> Result<()> {
    let index_path = layout.search_index_dir();
    let engine = TantivySearchEngine::open_or_create(index_path.as_std_path())?;
    engine.clear()?;
    let stream_indexer = StreamIndexer::new(engine);
    stream_indexer.index_repository(&layout.root)?;
    write_stamp(index_path.as_std_path())?;
    Ok(())
}
