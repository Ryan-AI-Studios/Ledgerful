use super::ProjectIndexer;
use crate::index::analysis::analyze_file;
use crate::index::languages::Language;
use crate::index::types::{ProjectFile, ProjectSymbol, symbol_to_project_symbol};
use camino::Utf8Path;
use miette::Result;
use std::fs;
use tracing::warn;

pub fn index_file(
    indexer: &ProjectIndexer,
    path: &Utf8Path,
) -> Result<(ProjectFile, Vec<ProjectSymbol>)> {
    let relative = path.strip_prefix(&indexer.repo_path).unwrap_or(path);
    let outcome = analyze_file(relative.as_std_path(), indexer.repo_path.as_std_path());

    let now = chrono::Utc::now().to_rfc3339();
    let mut pf = ProjectFile {
        id: None,
        file_path: relative.to_string().replace('\\', "/"),
        language: relative
            .extension()
            .and_then(Language::from_extension)
            .map(|l| format!("{:?}", l)),
        content_hash: None,
        git_blob_oid: None,
        file_size: fs::metadata(path).ok().map(|m| m.len() as i64),
        mtime_ns: None,
        parser_version: super::PARSER_VERSION.to_string(),
        parse_status: if outcome.analysis_status.symbols
            == crate::impact::packet::AnalysisStatus::Ok
        {
            "OK".to_string()
        } else {
            "PARSE_FAILED".to_string()
        },
        last_indexed_at: now.clone(),
    };

    if let Ok(content) = crate::util::fs::read_to_string_with_encoding(path.as_std_path()) {
        pf.content_hash = Some(blake3::hash(content.as_bytes()).to_hex().to_string());
    }

    let ps = outcome
        .symbols
        .unwrap_or_default()
        .into_iter()
        .map(|s| symbol_to_project_symbol(&s, 0, &now))
        .collect();

    Ok((pf, ps))
}

pub fn index_file_with_edges(
    indexer: &ProjectIndexer,
    path: &Utf8Path,
) -> Result<(
    ProjectFile,
    Vec<ProjectSymbol>,
    Vec<crate::index::call_graph::CallEdge>,
)> {
    let (project_file, project_symbols) = index_file(indexer, path)?;
    if project_file.parse_status != "OK" {
        return Ok((project_file, project_symbols, Vec::new()));
    }

    let content = match crate::util::fs::read_to_string_with_encoding(path.as_std_path()) {
        Ok(c) => c,
        Err(e) => {
            warn!("Failed to read file {} for call extraction: {}", path, e);
            return Ok((project_file, project_symbols, Vec::new()));
        }
    };

    let symbols: Vec<crate::index::symbols::Symbol> = project_symbols
        .iter()
        .filter_map(|ps| {
            Some(crate::index::symbols::Symbol {
                name: ps.symbol_name.clone(),
                kind: crate::index::symbols::SymbolKind::parse(&ps.symbol_kind)?,
                is_public: ps.is_public,
                cognitive_complexity: ps.cognitive_complexity,
                cyclomatic_complexity: ps.cyclomatic_complexity,
                line_start: ps.line_start,
                line_end: ps.line_end,
                qualified_name: Some(ps.qualified_name.clone()),
                byte_start: ps.byte_start,
                byte_end: ps.byte_end,
                entrypoint_kind: Some(ps.entrypoint_kind.clone()),
                metadata: ps
                    .metadata
                    .as_ref()
                    .and_then(|m| serde_json::from_str(m).ok())
                    .unwrap_or_default(),
            })
        })
        .collect();

    let calls = match crate::index::languages::extract_calls(path.as_std_path(), &content, &symbols)
    {
        Ok(c) => c,
        Err(e) => {
            warn!("Call extraction failed for {}: {}", path, e);
            Vec::new()
        }
    };

    Ok((project_file, project_symbols, calls))
}
