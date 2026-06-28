use miette::{IntoDiagnostic, Result};
use rusqlite::Connection;
use std::path::Path;

/// Detects if a SCIP index at the given path is stale compared to the database record.
pub fn is_scip_stale(conn: &Connection, index_path: &Path, current_hash: &str) -> Result<bool> {
    let index_path_str = index_path.to_string_lossy();

    let result: Result<String, rusqlite::Error> = conn.query_row(
        "SELECT blake3_hash FROM scip_indices WHERE index_path = ?1",
        [&index_path_str],
        |row| row.get(0),
    );

    match result {
        Ok(stored_hash) => {
            // If hashes match, it's not stale
            Ok(stored_hash != current_hash)
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
