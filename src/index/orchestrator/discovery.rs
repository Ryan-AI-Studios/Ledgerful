use super::ProjectIndexer;
use crate::index::walker::RepoWalker;
use camino::Utf8PathBuf;
use miette::{IntoDiagnostic, Result};
use std::path::PathBuf;

pub fn discover_files(indexer: &ProjectIndexer) -> Result<Vec<Utf8PathBuf>> {
    RepoWalker::new(
        indexer.repo_path.clone(),
        super::SUPPORTED_EXTENSIONS,
        super::BINARY_EXTENSIONS,
    )
    .discover_files()
}

pub fn discover_doc_files(indexer: &ProjectIndexer) -> Result<Vec<Utf8PathBuf>> {
    RepoWalker::new(
        indexer.repo_path.clone(),
        super::SUPPORTED_EXTENSIONS,
        super::BINARY_EXTENSIONS,
    )
    .discover_doc_files()
}

pub fn get_semantic_sample_files(
    indexer: &ProjectIndexer,
) -> Result<Vec<(std::path::PathBuf, String)>> {
    let conn = indexer.storage.get_connection();
    let mut stmt = conn
        .prepare(
            "SELECT pf.file_path \
             FROM project_files pf \
             WHERE pf.parse_status = 'OK' \
               AND pf.language IN ('Rust', 'TypeScript', 'Python', 'Go') \
             ORDER BY (SELECT COUNT(*) FROM project_symbols ps WHERE ps.file_id = pf.id) DESC \
             LIMIT 10",
        )
        .into_diagnostic()?;

    let rows: Vec<String> = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .into_diagnostic()?
        .collect::<Result<Vec<_>, _>>()
        .into_diagnostic()?;
    drop(stmt);

    let mut files = Vec::new();
    for path_str in rows {
        let full_path = indexer.repo_path.join(&path_str);
        if let Ok(content) = crate::util::fs::read_to_string_with_encoding(full_path.as_std_path())
        {
            files.push((PathBuf::from(full_path.as_str()), content));
        }
    }
    Ok(files)
}
