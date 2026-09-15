use miette::{IntoDiagnostic, Result};
use rusqlite::Connection;
use std::path::Path;

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
