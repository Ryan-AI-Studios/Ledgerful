use miette::{IntoDiagnostic, Result};
use rusqlite::Connection;
use std::path::Path;

/// Hash / residual-edge probe for SCIP index metadata.
///
/// **Not used to skip edge re-application** (`execute_scip_index` always
/// re-applies). Retained for audit/diagnostics and optional future generation
/// optimization. A matching hash alone is not enough (DoD-10): if
/// `clear_project_data` or incremental reindex wiped edges, residual
/// `scip_indices` rows would otherwise look "fresh" while coverage is incomplete.
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

pub(crate) enum ArtifactRegistration {
    Exact(String),
    ExactKeyNotFound,
    RelativeKeyExists,
}

/// Audit-only exact spelling lookup. Never opens a stored path or inspects edges.
pub(crate) fn read_artifact_registration(
    conn: &Connection,
    raw: &str,
) -> rusqlite::Result<ArtifactRegistration> {
    use rusqlite::OptionalExtension;
    let hash: Option<String> = conn
        .query_row(
            "SELECT CASE WHEN length(CAST(blake3_hash AS BLOB)) = 64 THEN blake3_hash ELSE '' END
             FROM scip_indices WHERE index_path COLLATE BINARY = ?1 LIMIT 1",
            [raw],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(hash) = hash {
        return Ok(ArtifactRegistration::Exact(hash));
    }
    // Follow this host's Path grammar. An exact-key miss remains ambiguous
    // when a relative spelling or a key beyond the audit limit may exist.
    const KEY_LIMIT: i64 = 50_000;
    let mut statement = conn.prepare(
        "SELECT CASE WHEN length(CAST(index_path AS BLOB)) BETWEEN 1 AND 4096
                     THEN index_path END FROM scip_indices LIMIT ?1",
    )?;
    let mut keys = statement.query([KEY_LIMIT + 1])?;
    let mut count = 0;
    while let Some(row) = keys.next()? {
        count += 1;
        let key: Option<String> = row.get(0)?;
        if count > KEY_LIMIT || !key.is_some_and(|key| Path::new(&key).is_absolute()) {
            return Ok(ArtifactRegistration::RelativeKeyExists);
        }
    }
    Ok(ArtifactRegistration::ExactKeyNotFound)
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
    #[test]
    fn dependency_evidence_relative_receipt_uses_host_path_grammar() {
        let mut conn = Connection::open_in_memory().unwrap();
        get_migrations().to_latest(&mut conn).unwrap();
        #[cfg(windows)]
        let (relative, distinct) = ("/legacy.scip", r"C:\new.scip");
        #[cfg(not(windows))]
        let (relative, distinct) = (r"C:\legacy.scip", "/new.scip");
        assert!(!Path::new(relative).is_absolute());
        assert!(Path::new(distinct).is_absolute());
        let hash = "a".repeat(64);
        register_scip_index(&conn, Path::new(relative), &hash).unwrap();
        assert!(
            matches!(read_artifact_registration(&conn, relative).unwrap(),
                ArtifactRegistration::Exact(stored) if stored == hash
            )
        );
        assert!(matches!(
            read_artifact_registration(&conn, distinct).unwrap(),
            ArtifactRegistration::RelativeKeyExists
        ));
    }
}
