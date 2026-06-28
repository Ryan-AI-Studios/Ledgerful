use crate::index::types::{ProjectFile, ProjectSymbol};
use miette::{IntoDiagnostic, Result};
use rusqlite::Connection;

pub fn insert_file_row(conn: &Connection, pf: &ProjectFile) -> Result<()> {
    conn.execute(
        "INSERT INTO project_files (file_path, language, content_hash, git_blob_oid, file_size, mtime_ns, parser_version, parse_status, last_indexed_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        rusqlite::params![
            pf.file_path,
            pf.language,
            pf.content_hash,
            pf.git_blob_oid,
            pf.file_size,
            pf.mtime_ns,
            pf.parser_version,
            pf.parse_status,
            pf.last_indexed_at
        ],
    )
    .into_diagnostic()?;
    Ok(())
}

pub fn upsert_file_row(conn: &Connection, pf: &ProjectFile) -> Result<()> {
    conn.execute(
        "INSERT INTO project_files (file_path, language, content_hash, git_blob_oid, file_size, mtime_ns, parser_version, parse_status, last_indexed_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9) ON CONFLICT(file_path) DO UPDATE SET language=excluded.language, content_hash=excluded.content_hash, git_blob_oid=excluded.git_blob_oid, file_size=excluded.file_size, mtime_ns=excluded.mtime_ns, parser_version=excluded.parser_version, parse_status=excluded.parse_status, last_indexed_at=excluded.last_indexed_at",
        rusqlite::params![
            pf.file_path,
            pf.language,
            pf.content_hash,
            pf.git_blob_oid,
            pf.file_size,
            pf.mtime_ns,
            pf.parser_version,
            pf.parse_status,
            pf.last_indexed_at
        ],
    )
    .into_diagnostic()?;
    Ok(())
}

pub fn get_file_id_by_path(conn: &Connection, file_path: &str) -> Result<i64> {
    conn.query_row(
        "SELECT id FROM project_files WHERE file_path = ?1",
        [file_path],
        |row| row.get(0),
    )
    .into_diagnostic()
}

pub fn delete_file_index_dependents(conn: &Connection, file_path: &str) -> Result<()> {
    let file_id_subquery = "SELECT id FROM project_files WHERE file_path = ?1";
    let symbol_id_subquery = "SELECT id FROM project_symbols WHERE file_id IN (SELECT id FROM project_files WHERE file_path = ?1)";
    for statement in [
        format!(
            "DELETE FROM symbol_centrality WHERE file_id IN ({file_id_subquery}) OR symbol_id IN ({symbol_id_subquery})"
        ),
        format!(
            "DELETE FROM structural_edges WHERE caller_file_id IN ({file_id_subquery}) OR callee_file_id IN ({file_id_subquery}) OR caller_symbol_id IN ({symbol_id_subquery}) OR callee_symbol_id IN ({symbol_id_subquery})"
        ),
        format!(
            "DELETE FROM api_routes WHERE handler_file_id IN ({file_id_subquery}) OR handler_symbol_id IN ({symbol_id_subquery})"
        ),
        format!("DELETE FROM data_models WHERE model_file_id IN ({file_id_subquery})"),
        format!("DELETE FROM observability_patterns WHERE file_id IN ({file_id_subquery})"),
        format!(
            "DELETE FROM test_mapping WHERE test_file_id IN ({file_id_subquery}) OR tested_file_id IN ({file_id_subquery}) OR test_symbol_id IN ({symbol_id_subquery}) OR tested_symbol_id IN ({symbol_id_subquery})"
        ),
        format!("DELETE FROM ci_gates WHERE ci_file_id IN ({file_id_subquery})"),
        format!(
            "DELETE FROM env_references WHERE file_id IN ({file_id_subquery}) OR symbol_id IN ({symbol_id_subquery})"
        ),
        format!("DELETE FROM env_declarations WHERE source_file_id IN ({file_id_subquery})"),
        format!("DELETE FROM project_docs WHERE file_id IN ({file_id_subquery})"),
    ] {
        conn.execute(&statement, [file_path]).into_diagnostic()?;
    }
    Ok(())
}

pub fn insert_symbol_row(conn: &Connection, ps: &ProjectSymbol, file_id: i64) -> Result<()> {
    conn.execute(
        "INSERT INTO project_symbols (file_id, qualified_name, symbol_name, symbol_kind, visibility, entrypoint_kind, is_public, cognitive_complexity, cyclomatic_complexity, line_start, line_end, byte_start, byte_end, signature_hash, confidence, evidence, last_indexed_at, metadata) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18) ON CONFLICT(file_id, qualified_name, symbol_kind) DO UPDATE SET symbol_name=excluded.symbol_name, visibility=excluded.visibility, entrypoint_kind=excluded.entrypoint_kind, is_public=excluded.is_public, cognitive_complexity=excluded.cognitive_complexity, cyclomatic_complexity=excluded.cyclomatic_complexity, line_start=excluded.line_start, line_end=excluded.line_end, byte_start=excluded.byte_start, byte_end=excluded.byte_end, signature_hash=excluded.signature_hash, confidence=excluded.confidence, evidence=excluded.evidence, last_indexed_at=excluded.last_indexed_at, metadata=excluded.metadata",
        rusqlite::params![
            file_id,
            ps.qualified_name,
            ps.symbol_name,
            ps.symbol_kind,
            ps.visibility,
            ps.entrypoint_kind,
            ps.is_public as i32,
            ps.cognitive_complexity,
            ps.cyclomatic_complexity,
            ps.line_start,
            ps.line_end,
            ps.byte_start,
            ps.byte_end,
            ps.signature_hash,
            ps.confidence,
            ps.evidence,
            ps.last_indexed_at,
            ps.metadata
        ],
    )
    .into_diagnostic()?;
    Ok(())
}

use crate::state::storage::StorageManager;

/// Delete all symbols for a given file path, including dependent rows.
pub fn delete_file_symbols(storage: &mut StorageManager, file_path: &str) -> Result<()> {
    let conn = storage.get_connection_mut();
    delete_file_index_dependents(conn, file_path)?;
    conn.execute(
        "DELETE FROM project_symbols WHERE file_id IN (SELECT id FROM project_files WHERE file_path = ?1)",
        [file_path],
    )
    .into_diagnostic()?;
    Ok(())
}
