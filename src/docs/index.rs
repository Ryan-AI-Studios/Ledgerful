use crate::config::model::Config;
use crate::docs::chunker::{DocChunk, chunk_markdown};
use crate::docs::crawler::crawl_docs;
use crate::embed::embed_and_store;
use camino::Utf8Path;
use rusqlite::Connection;
use std::collections::HashSet;
use tracing::info;

pub struct DocsIndexSummary {
    pub files_crawled: usize,
    pub chunks_new: usize,
    pub chunks_updated: usize,
    pub chunks_deleted: usize,
}

/// Run the full document indexing pipeline:
/// crawl, chunk, store in doc_chunks, embed if configured.
pub fn run_docs_index(
    config: &Config,
    repo_root: &Utf8Path,
    conn: &Connection,
) -> Result<DocsIndexSummary, String> {
    let docs_config = &config.docs;
    let embed_config = &config.local_model;

    if docs_config.include.is_empty() {
        info!("No doc paths configured in [docs].include - skipping doc index.");
        return Ok(DocsIndexSummary {
            files_crawled: 0,
            chunks_new: 0,
            chunks_updated: 0,
            chunks_deleted: 0,
        });
    }

    let doc_files = crawl_docs(repo_root, &docs_config.include)?;
    let files_crawled = doc_files.len();
    let mut chunks_new = 0usize;
    let mut chunks_updated = 0usize;

    // Collect all (file_path, chunk_index) pairs we encounter
    let mut crawled_pairs: HashSet<(String, usize)> = HashSet::new();

    for doc_file in &doc_files {
        let chunks = chunk_markdown(
            &doc_file.content,
            doc_file.path.as_str(),
            docs_config.chunk_tokens,
            docs_config.chunk_overlap,
        );

        for chunk in &chunks {
            let chunk_content_hash = content_hash(&chunk.content);
            crawled_pairs.insert((chunk.file_path.clone(), chunk.chunk_index));

            let existing = get_existing_chunk_hash(conn, &chunk.file_path, chunk.chunk_index);

            match existing {
                Some(existing_hash) if existing_hash == chunk_content_hash => {
                    // Unchanged - skip
                }
                Some(_) => {
                    // Content changed - update
                    update_chunk(conn, chunk, &chunk_content_hash)?;
                    chunks_updated += 1;

                    if !embed_config.base_url.is_empty() {
                        let entity_id = format!("{}::{}", chunk.file_path, chunk.chunk_index);
                        embed_and_store(
                            embed_config,
                            conn,
                            "doc_chunk",
                            &entity_id,
                            &chunk.content,
                        )?;
                    }
                }
                None => {
                    // New chunk - insert
                    insert_chunk(conn, chunk, &chunk_content_hash)?;
                    chunks_new += 1;

                    if !embed_config.base_url.is_empty() {
                        let entity_id = format!("{}::{}", chunk.file_path, chunk.chunk_index);
                        embed_and_store(
                            embed_config,
                            conn,
                            "doc_chunk",
                            &entity_id,
                            &chunk.content,
                        )?;
                    }
                }
            }
        }
    }

    // Delete orphaned chunks and their embeddings
    let chunks_deleted = delete_orphaned_chunks(conn, &crawled_pairs)?;

    Ok(DocsIndexSummary {
        files_crawled,
        chunks_new,
        chunks_updated,
        chunks_deleted,
    })
}

fn content_hash(text: &str) -> String {
    blake3::hash(text.as_bytes()).to_hex().to_string()
}

fn get_existing_chunk_hash(
    conn: &Connection,
    file_path: &str,
    chunk_index: usize,
) -> Option<String> {
    let content: Option<String> = conn
        .query_row(
            "SELECT content FROM doc_chunks WHERE file_path = ?1 AND chunk_index = ?2",
            rusqlite::params![file_path, chunk_index as i64],
            |row| row.get(0),
        )
        .ok();

    content.map(|c| content_hash(&c))
}

fn insert_chunk(conn: &Connection, chunk: &DocChunk, _hash: &str) -> Result<(), String> {
    conn.execute(
        "INSERT INTO doc_chunks (file_path, chunk_index, heading, content, token_count) \
         VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![
            chunk.file_path,
            chunk.chunk_index as i64,
            chunk.heading,
            chunk.content,
            chunk.token_count as i64,
        ],
    )
    .map_err(|e| format!("Failed to insert doc_chunk: {}", e))?;

    Ok(())
}

fn update_chunk(conn: &Connection, chunk: &DocChunk, _hash: &str) -> Result<(), String> {
    conn.execute(
        "UPDATE doc_chunks SET heading = ?1, content = ?2, token_count = ?3 \
         WHERE file_path = ?4 AND chunk_index = ?5",
        rusqlite::params![
            chunk.heading,
            chunk.content,
            chunk.token_count as i64,
            chunk.file_path,
            chunk.chunk_index as i64,
        ],
    )
    .map_err(|e| format!("Failed to update doc_chunk: {}", e))?;

    Ok(())
}

fn delete_orphaned_chunks(
    conn: &Connection,
    crawled_pairs: &HashSet<(String, usize)>,
) -> Result<usize, String> {
    // Get all existing chunks
    let mut stmt = conn
        .prepare("SELECT file_path, chunk_index FROM doc_chunks")
        .map_err(|e| e.to_string())?;

    let existing: Vec<(String, usize)> = stmt
        .query_map([], |row| {
            let path: String = row.get(0)?;
            let idx: i64 = row.get(1)?;
            Ok((path, idx as usize))
        })
        .map_err(|e| e.to_string())?
        .filter_map(|r| r.ok())
        .collect();

    let mut deleted = 0usize;
    for (file_path, chunk_index) in existing {
        if !crawled_pairs.contains(&(file_path.clone(), chunk_index)) {
            conn.execute(
                "DELETE FROM doc_chunks WHERE file_path = ?1 AND chunk_index = ?2",
                rusqlite::params![file_path, chunk_index as i64],
            )
            .map_err(|e| e.to_string())?;

            // Also delete the corresponding embedding
            let entity_id = format!("{}::{}", file_path, chunk_index);
            conn.execute(
                "DELETE FROM embeddings WHERE entity_type = 'doc_chunk' AND entity_id = ?1",
                rusqlite::params![entity_id],
            )
            .map_err(|e| e.to_string())?;

            deleted += 1;
        }
    }

    Ok(deleted)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::model::{DocsConfig, LocalModelConfig};
    use crate::state::migrations::get_migrations;
    use std::fs;
    use tempfile::tempdir;

    fn setup_db() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        let migrations = get_migrations();
        migrations.to_latest(&mut conn).unwrap();
        conn
    }

    fn make_config(docs_config: DocsConfig) -> Config {
        Config {
            docs: docs_config,
            local_model: LocalModelConfig::default(),
            ..Config::default()
        }
    }

    #[test]
    fn test_index_docs_stores_chunks() {
        let tmp = tempdir().unwrap();
        let root = Utf8Path::from_path(tmp.path()).unwrap();

        fs::create_dir_all(root.join("docs")).unwrap();
        let body = "X".repeat(220); // ~55 tokens
        fs::write(
            root.join("docs").join("guide.md"),
            format!("# Guide\n\n{}\n\n## Section 1\n\n{}\n", body, body),
        )
        .unwrap();

        let conn = setup_db();
        let config = make_config(DocsConfig {
            include: vec!["docs/".to_string()],
            chunk_tokens: 512,
            chunk_overlap: 0,
            retrieval_top_k: 5,
        });

        let summary = run_docs_index(&config, root, &conn).unwrap();

        assert_eq!(summary.files_crawled, 1);
        assert!(summary.chunks_new > 0, "Should create new chunks");
        assert_eq!(summary.chunks_updated, 0);
        assert_eq!(summary.chunks_deleted, 0);

        // Verify doc_chunks table has rows
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM doc_chunks", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, summary.chunks_new as i64);
    }

    #[test]
    fn test_reindex_skips_unchanged_files() {
        let tmp = tempdir().unwrap();
        let root = Utf8Path::from_path(tmp.path()).unwrap();

        fs::create_dir_all(root.join("docs")).unwrap();
        let body = "X".repeat(220);
        let content = format!("# Guide\n\n{}\n", body);
        fs::write(root.join("docs").join("guide.md"), &content).unwrap();

        let conn = setup_db();
        let config = make_config(DocsConfig {
            include: vec!["docs/".to_string()],
            chunk_tokens: 512,
            chunk_overlap: 0,
            retrieval_top_k: 5,
        });

        // First index
        let summary1 = run_docs_index(&config, root, &conn).unwrap();
        assert!(summary1.chunks_new > 0);

        // Re-index with same content
        let summary2 = run_docs_index(&config, root, &conn).unwrap();
        assert_eq!(summary2.chunks_new, 0, "Should not create new chunks");
        assert_eq!(summary2.chunks_updated, 0, "Should not update chunks");
    }

    #[test]
    fn test_reindex_detects_changes() {
        let tmp = tempdir().unwrap();
        let root = Utf8Path::from_path(tmp.path()).unwrap();

        fs::create_dir_all(root.join("docs")).unwrap();
        let body = "X".repeat(220);
        let content_v1 = format!("# Guide\n\n{}\n", body);
        fs::write(root.join("docs").join("guide.md"), &content_v1).unwrap();

        let conn = setup_db();
        let config = make_config(DocsConfig {
            include: vec!["docs/".to_string()],
            chunk_tokens: 512,
            chunk_overlap: 0,
            retrieval_top_k: 5,
        });

        // First index
        let summary1 = run_docs_index(&config, root, &conn).unwrap();
        assert!(summary1.chunks_new > 0);

        // Modify the file
        let body2 = "Y".repeat(220);
        let content_v2 = format!("# Guide Updated\n\n{}\n", body2);
        fs::write(root.join("docs").join("guide.md"), &content_v2).unwrap();

        let summary2 = run_docs_index(&config, root, &conn).unwrap();
        assert!(summary2.chunks_updated > 0, "Should detect content changes");
    }

    #[test]
    fn test_index_docs_deletes_orphaned_chunks() {
        let tmp = tempdir().unwrap();
        let root = Utf8Path::from_path(tmp.path()).unwrap();

        fs::create_dir_all(root.join("docs")).unwrap();
        let body = "X".repeat(220);

        // Create two files
        fs::write(root.join("docs").join("a.md"), format!("# A\n\n{}\n", body)).unwrap();
        fs::write(root.join("docs").join("b.md"), format!("# B\n\n{}\n", body)).unwrap();

        let conn = setup_db();
        let config = make_config(DocsConfig {
            include: vec!["docs/".to_string()],
            chunk_tokens: 512,
            chunk_overlap: 0,
            retrieval_top_k: 5,
        });

        // First index with both files
        let _summary1 = run_docs_index(&config, root, &conn).unwrap();
        let initial_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM doc_chunks", [], |row| row.get(0))
            .unwrap();
        assert!(initial_count >= 2);

        // Delete file b.md
        fs::remove_file(root.join("docs").join("b.md")).unwrap();

        // Re-index
        let summary2 = run_docs_index(&config, root, &conn).unwrap();
        assert!(summary2.chunks_deleted > 0, "Should delete orphaned chunks");

        // Verify b.md chunks are gone
        let b_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM doc_chunks WHERE file_path LIKE '%b.md'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(b_count, 0);
    }

    #[test]
    fn test_index_docs_empty_include_returns_zero() {
        let tmp = tempdir().unwrap();
        let root = Utf8Path::from_path(tmp.path()).unwrap();

        let conn = setup_db();
        let config = make_config(DocsConfig {
            include: vec![],
            ..DocsConfig::default()
        });

        let summary = run_docs_index(&config, root, &conn).unwrap();
        assert_eq!(summary.files_crawled, 0);
        assert_eq!(summary.chunks_new, 0);
    }
}
