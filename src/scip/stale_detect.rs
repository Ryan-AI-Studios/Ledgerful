use miette::{IntoDiagnostic, Result};
use rusqlite::Connection;
use std::path::Path;

/// Detects if a SCIP index at the given path is stale compared to the database record.
///
/// A matching hash alone is not enough (0095 DoD-10): if `clear_project_data` or
/// incremental reindex wiped `structural_edges`, the scip_indices row would
/// otherwise skip re-ingest of edges that no longer exist. We also require at
/// least one `scip:%` evidence edge to treat the index as up to date.
pub fn is_scip_stale(conn: &Connection, index_path: &Path, current_hash: &str) -> Result<bool> {
    let index_path_str = index_path.to_string_lossy();

    let result: Result<String, rusqlite::Error> = conn.query_row(
        "SELECT blake3_hash FROM scip_indices WHERE index_path = ?1",
        [&index_path_str],
        |row| row.get(0),
    );

    match result {
        Ok(stored_hash) => {
            if stored_hash != current_hash {
                return Ok(true);
            }
            // Hash matches — but do the described rows still exist?
            let scip_edges: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM structural_edges WHERE evidence LIKE 'scip:%'",
                    [],
                    |row| row.get(0),
                )
                .unwrap_or(0);
            Ok(scip_edges == 0)
        }
        Err(rusqlite::Error::QueryReturnedNoRows) => {
            // Not in database, so it's "stale" (needs indexing)
            Ok(true)
        }
        Err(e) => Err(e).into_diagnostic(),
    }
}

/// Upserts a SCIP index record in the database.
pub fn register_scip_index(conn: &Connection, index_path: &Path, hash: &str) -> Result<()> {
    let index_path_str = index_path.to_string_lossy();

    conn.execute(
        "INSERT INTO scip_indices (index_path, blake3_hash, indexed_at)
         VALUES (?1, ?2, datetime('now'))
         ON CONFLICT(index_path) DO UPDATE SET
            blake3_hash = excluded.blake3_hash,
            indexed_at = excluded.indexed_at",
        (index_path_str, hash),
    )
    .into_diagnostic()?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::migrations::get_migrations;

    #[test]
    fn hash_match_but_no_edges_is_stale() {
        let mut conn = Connection::open_in_memory().unwrap();
        get_migrations().to_latest(&mut conn).unwrap();
        let path = std::path::Path::new("/tmp/test.scip");
        register_scip_index(&conn, path, "abc").unwrap();
        assert!(is_scip_stale(&conn, path, "abc").unwrap());
    }

    #[test]
    fn hash_mismatch_is_stale() {
        let mut conn = Connection::open_in_memory().unwrap();
        get_migrations().to_latest(&mut conn).unwrap();
        let path = std::path::Path::new("/tmp/test.scip");
        register_scip_index(&conn, path, "abc").unwrap();
        assert!(is_scip_stale(&conn, path, "xyz").unwrap());
    }
}
