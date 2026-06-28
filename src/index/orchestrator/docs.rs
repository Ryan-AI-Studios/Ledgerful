use super::ProjectIndexer;
use crate::index::docs::{DocIndexStats, parse_markdown};
use miette::{IntoDiagnostic, Result};
use std::fs;
use tracing::warn;

pub fn index_docs(indexer: &mut ProjectIndexer) -> Result<DocIndexStats> {
    let doc_files = super::discovery::discover_doc_files(indexer)?;
    let has_readme = indexer.repo_path.join("README.md").exists();
    let mut docs_indexed = 0usize;
    let mut parse_failures = 0usize;
    let now = chrono::Utc::now().to_rfc3339();

    for doc_path in &doc_files {
        let relative_path = doc_path
            .strip_prefix(&indexer.repo_path)
            .unwrap_or(doc_path)
            .to_string();
        let content = match fs::read_to_string(doc_path) {
            Ok(c) => c,
            Err(e) => {
                warn!("Failed to read doc file {}: {}", doc_path, e);
                parse_failures += 1;
                continue;
            }
        };
        let parsed = parse_markdown(&content, &relative_path);
        let file_id = ensure_file_entry(indexer, &relative_path, &content, &now)?;
        let sections_json =
            serde_json::to_string(&parsed.sections).unwrap_or_else(|_| "[]".to_string());
        let code_blocks_json =
            serde_json::to_string(&parsed.code_blocks).unwrap_or_else(|_| "[]".to_string());
        let internal_links_json =
            serde_json::to_string(&parsed.internal_links).unwrap_or_else(|_| "[]".to_string());

        let conn = indexer.storage.get_connection_mut();
        conn.execute("INSERT OR REPLACE INTO project_docs (file_id, title, summary, sections, code_blocks, internal_links, confidence, last_indexed_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![file_id, parsed.title, parsed.summary, sections_json, code_blocks_json, internal_links_json, 1.0_f64, now]).into_diagnostic()?;
        docs_indexed += 1;
    }
    Ok(DocIndexStats {
        docs_indexed,
        parse_failures,
        missing_readme: !has_readme,
    })
}

pub fn ensure_file_entry(
    indexer: &mut ProjectIndexer,
    relative_path: &str,
    content: &str,
    now: &str,
) -> Result<i64> {
    let content_hash = blake3::hash(content.as_bytes()).to_hex().to_string();
    let conn = indexer.storage.get_connection();
    let existing_id: Option<i64> = conn
        .query_row(
            "SELECT id FROM project_files WHERE file_path = ?1",
            [relative_path],
            |row| row.get(0),
        )
        .ok();
    if let Some(id) = existing_id {
        return Ok(id);
    }
    let conn = indexer.storage.get_connection_mut();
    conn.execute("INSERT INTO project_files (file_path, language, content_hash, git_blob_oid, file_size, mtime_ns, parser_version, parse_status, last_indexed_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        rusqlite::params![relative_path, "Markdown", content_hash, Option::<String>::None, content.len() as i64, Option::<i64>::None, super::PARSER_VERSION, "OK", now]).into_diagnostic()?;
    Ok(conn.last_insert_rowid())
}
