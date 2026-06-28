//! Git metadata backfill for `project_files` (Track TA30).
//!
//! After indexing, walks git history (first-parent, up to 1000 commits) to
//! populate `last_touched_at` (committer-time ISO-8601) and `last_contributor`
//! (author name) for each file in `project_files`.
//!
//! The walk builds an in-memory `HashMap` during the git walk — **no SQL is
//! issued during the walk**. After the walk, a single batch `UPDATE`
//! transaction writes all results. This avoids N separate `UPDATE` queries
//! during tree-diffing (which would heavily degrade indexing performance).
//!
//! For incremental indexing, the backfill only runs if there are rows with
//! `last_touched_at IS NULL` (either new files or existing files not yet
//! backfilled). If all rows are populated and no files changed, the walk is
//! skipped entirely (fast path).
//!
//! The actual walk logic lives in `crate::git::metadata` (shared with TA29's
//! web API TTL cache) to avoid code duplication.

use crate::git::metadata::collect_git_metadata;
use crate::state::storage::StorageManager;
use camino::Utf8Path;
use miette::{IntoDiagnostic, Result};

const BATCH_SIZE: usize = 500;

/// Backfill `last_touched_at` and `last_contributor` in `project_files` by
/// walking git history. Only updates rows where `last_touched_at IS NULL`.
///
/// Uses the **author** signature for `last_contributor` (the person who wrote
/// the code) and the **committer time** for `last_touched_at` (when the commit
/// landed in the repo). This distinction matters in GitHub squash-merge flow
/// where the committer is often "GitHub" but the author is the developer.
pub fn backfill_git_metadata(storage: &mut StorageManager, repo_root: &Utf8Path) -> Result<()> {
    // Fast path: check if any rows need backfill.
    let null_count: i64 = {
        let conn = storage.get_connection();
        conn.query_row(
            "SELECT COUNT(*) FROM project_files WHERE last_touched_at IS NULL",
            [],
            |row| row.get(0),
        )
        .into_diagnostic()?
    };

    if null_count == 0 {
        tracing::debug!("Git metadata backfill: no NULL rows, skipping walk.");
        return Ok(());
    }

    tracing::info!(
        "Git metadata backfill: {} rows need population, walking up to 1000 commits.",
        null_count
    );

    // Build the metadata map by walking git history (shared walk logic).
    let meta_map = collect_git_metadata(repo_root, 1000)?;

    if meta_map.is_empty() {
        tracing::info!("Git metadata backfill: no git history found, leaving rows NULL.");
        return Ok(());
    }

    // Batch UPDATE in a single transaction.
    let conn = storage.get_connection_mut();
    let tx = conn.transaction().into_diagnostic()?;

    let mut updated = 0usize;
    let entries: Vec<(String, String, String)> = meta_map
        .into_iter()
        .map(|(path, (ts, author))| (path, ts, author))
        .collect();

    for chunk in entries.chunks(BATCH_SIZE) {
        // Match on both the raw file_path and a backslash-normalized
        // version, because existing rows may use Windows backslashes while
        // the git walk always produces forward slashes.
        let mut stmt = tx
            .prepare(
                "UPDATE project_files SET last_touched_at = ?1, last_contributor = ?2 \
                 WHERE (file_path = ?3 OR REPLACE(file_path, '\\', '/') = ?3) \
                 AND last_touched_at IS NULL",
            )
            .into_diagnostic()?;
        for (path, ts, author) in chunk {
            updated += stmt
                .execute(rusqlite::params![ts, author, path])
                .into_diagnostic()?;
        }
    }

    tx.commit().into_diagnostic()?;
    tracing::info!("Git metadata backfill: updated {} rows.", updated);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::migrations::get_migrations;
    use crate::state::storage::StorageManager;
    use rusqlite::Connection;
    use tempfile::tempdir;

    fn make_storage() -> StorageManager {
        let conn = Connection::open_in_memory().unwrap();
        let mut conn = conn;
        get_migrations().to_latest(&mut conn).unwrap();
        StorageManager::init_from_conn(conn)
    }

    #[test]
    fn backfill_no_null_rows_skips_walk() {
        let mut storage = make_storage();

        // Insert a row with last_touched_at already set.
        {
            let conn = storage.get_connection_mut();
            conn.execute(
                "INSERT INTO project_files (file_path, language, last_indexed_at, last_touched_at, last_contributor)
                 VALUES ('src/main.rs', 'Rust', '2024-01-01', '2024-01-01T00:00:00+00:00', 'Alice')",
                [],
            )
            .unwrap();
        }

        // Should skip the walk (no NULL rows).
        let dir = tempdir().unwrap();
        let result = backfill_git_metadata(
            &mut storage,
            camino::Utf8Path::from_path(dir.path()).unwrap(),
        );
        assert!(result.is_ok());

        // Verify the row is unchanged.
        let conn = storage.get_connection();
        let (ts, author): (String, String) = conn
            .query_row(
                "SELECT last_touched_at, last_contributor FROM project_files WHERE file_path = 'src/main.rs'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(ts, "2024-01-01T00:00:00+00:00");
        assert_eq!(author, "Alice");
    }

    #[test]
    fn backfill_null_rows_no_git_returns_ok() {
        let mut storage = make_storage();

        // Insert a row with NULL last_touched_at.
        {
            let conn = storage.get_connection_mut();
            conn.execute(
                "INSERT INTO project_files (file_path, language, last_indexed_at)
                 VALUES ('src/main.rs', 'Rust', '2024-01-01')",
                [],
            )
            .unwrap();
        }

        // No git repo → should return Ok and leave rows NULL.
        let dir = tempdir().unwrap();
        let result = backfill_git_metadata(
            &mut storage,
            camino::Utf8Path::from_path(dir.path()).unwrap(),
        );
        assert!(result.is_ok());

        // Verify the row is still NULL.
        let conn = storage.get_connection();
        let ts: Option<String> = conn
            .query_row(
                "SELECT last_touched_at FROM project_files WHERE file_path = 'src/main.rs'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(ts.is_none());
    }

    #[test]
    fn backfill_preserves_existing_non_null_rows() {
        let mut storage = make_storage();

        // Insert two rows: one with data, one NULL.
        {
            let conn = storage.get_connection_mut();
            conn.execute(
                "INSERT INTO project_files (file_path, language, last_indexed_at, last_touched_at, last_contributor)
                 VALUES ('src/a.rs', 'Rust', '2024-01-01', '2024-01-01T00:00:00+00:00', 'Bob')",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO project_files (file_path, language, last_indexed_at)
                 VALUES ('src/b.rs', 'Rust', '2024-01-01')",
                [],
            )
            .unwrap();
        }

        // No git repo → backfill will skip (no git history).
        let dir = tempdir().unwrap();
        let _ = backfill_git_metadata(
            &mut storage,
            camino::Utf8Path::from_path(dir.path()).unwrap(),
        );

        // Verify the existing row is unchanged.
        let conn = storage.get_connection();
        let (ts, author): (String, String) = conn
            .query_row(
                "SELECT last_touched_at, last_contributor FROM project_files WHERE file_path = 'src/a.rs'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(ts, "2024-01-01T00:00:00+00:00");
        assert_eq!(author, "Bob");
    }

    #[test]
    fn migration_adds_columns() {
        let mut conn = Connection::open_in_memory().unwrap();
        let migrations = crate::state::migrations::get_migrations();
        migrations.to_latest(&mut conn).unwrap();

        // Verify columns exist.
        let has_last_touched_at: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('project_files') WHERE name = 'last_touched_at'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(has_last_touched_at, 1);

        let has_last_contributor: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('project_files') WHERE name = 'last_contributor'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(has_last_contributor, 1);
    }

    #[test]
    fn backfill_null_count_query_works() {
        let mut storage = make_storage();

        // Insert two rows: one with data, one NULL.
        {
            let conn = storage.get_connection_mut();
            conn.execute(
                "INSERT INTO project_files (file_path, language, last_indexed_at, last_touched_at, last_contributor)
                 VALUES ('src/a.rs', 'Rust', '2024-01-01', '2024-01-01T00:00:00+00:00', 'Alice')",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO project_files (file_path, language, last_indexed_at)
                 VALUES ('src/b.rs', 'Rust', '2024-01-01')",
                [],
            )
            .unwrap();
        }

        // Verify the NULL count query returns 1.
        let conn = storage.get_connection();
        let null_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM project_files WHERE last_touched_at IS NULL",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(null_count, 1);
    }

    /// Verify that the UPDATE in backfill matches rows with backslash paths
    /// when the git walk produces forward-slash paths. This is the path
    /// normalization fix — without it, rows inserted by the indexer with
    /// Windows backslashes would never get backfilled.
    #[test]
    fn backfill_update_matches_backslash_paths() {
        let mut storage = make_storage();

        // Insert a row with a BACKSLASH path (as the indexer does on Windows).
        {
            let conn = storage.get_connection_mut();
            conn.execute(
                "INSERT INTO project_files (file_path, language, last_indexed_at)
                 VALUES ('src\\main.rs', 'Rust', '2024-01-01')",
                [],
            )
            .unwrap();
        }

        // Simulate what the backfill UPDATE does: match using
        // REPLACE(file_path, '\\', '/') to normalize backslashes.
        {
            let conn = storage.get_connection_mut();
            let tx = conn.transaction().unwrap();
            tx.execute(
                "UPDATE project_files SET last_touched_at = ?1, last_contributor = ?2 \
                 WHERE (file_path = ?3 OR REPLACE(file_path, '\\', '/') = ?3) \
                 AND last_touched_at IS NULL",
                rusqlite::params!["2024-06-24T12:00:00+00:00", "Alice", "src/main.rs"],
            )
            .unwrap();
            tx.commit().unwrap();
        }

        // Verify the row was updated.
        let conn = storage.get_connection();
        let (ts, author): (String, String) = conn
            .query_row(
                "SELECT last_touched_at, last_contributor FROM project_files WHERE file_path = 'src\\main.rs'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(ts, "2024-06-24T12:00:00+00:00");
        assert_eq!(author, "Alice");
    }
}
