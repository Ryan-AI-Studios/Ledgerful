use super::ProjectIndexer;
use crate::index::analysis::analyze_file;
use crate::index::bindings::FileBinding;
use crate::index::call_graph::CallEdge;
use crate::index::languages::Language;
use crate::index::references::extract_file_bindings;
use crate::index::types::{ProjectFile, ProjectSymbol, symbol_to_project_symbol};
use camino::Utf8Path;
use miette::Result;
use std::fs;
use tracing::warn;

/// Result of indexing one file for the incremental / with-edges path.
pub struct IndexedFileWithEdges {
    pub project_file: ProjectFile,
    pub project_symbols: Vec<ProjectSymbol>,
    pub calls: Vec<CallEdge>,
    pub bindings: Vec<FileBinding>,
}

pub fn index_file(
    indexer: &ProjectIndexer,
    path: &Utf8Path,
) -> Result<(ProjectFile, Vec<ProjectSymbol>, Vec<FileBinding>)> {
    let relative = path.strip_prefix(&indexer.repo_path).unwrap_or(path);
    let outcome = analyze_file(relative.as_std_path(), indexer.repo_path.as_std_path());

    let now = chrono::Utc::now().to_rfc3339();
    let meta = fs::metadata(path).ok();
    let mut pf = ProjectFile {
        id: None,
        file_path: relative.to_string().replace('\\', "/"),
        language: relative
            .extension()
            .and_then(Language::from_extension)
            .map(|l| format!("{:?}", l)),
        content_hash: None,
        git_blob_oid: None,
        file_size: meta.as_ref().map(|m| m.len() as i64),
        mtime_ns: meta
            .as_ref()
            .and_then(crate::index::staleness::extract_mtime_ns),
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

    let mut bindings = Vec::new();
    if let Ok(content) = crate::util::fs::read_to_string_with_encoding(path.as_std_path()) {
        pf.content_hash = Some(blake3::hash(content.as_bytes()).to_hex().to_string());
        match extract_file_bindings(path.as_std_path(), &content) {
            Ok(Some(b)) => bindings = b,
            Ok(None) => {}
            Err(e) => {
                warn!("Binding extraction failed for {}: {}", path, e);
            }
        }
    }

    let ps = outcome
        .symbols
        .unwrap_or_default()
        .into_iter()
        .map(|s| symbol_to_project_symbol(&s, 0, &now))
        .collect();

    Ok((pf, ps, bindings))
}

pub fn index_file_with_edges(
    indexer: &ProjectIndexer,
    path: &Utf8Path,
) -> Result<IndexedFileWithEdges> {
    let (project_file, project_symbols, bindings) = index_file(indexer, path)?;
    if project_file.parse_status != "OK" {
        return Ok(IndexedFileWithEdges {
            project_file,
            project_symbols,
            calls: Vec::new(),
            bindings,
        });
    }

    let content = match crate::util::fs::read_to_string_with_encoding(path.as_std_path()) {
        Ok(c) => c,
        Err(e) => {
            warn!("Failed to read file {} for call extraction: {}", path, e);
            return Ok(IndexedFileWithEdges {
                project_file,
                project_symbols,
                calls: Vec::new(),
                bindings,
            });
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

    Ok(IndexedFileWithEdges {
        project_file,
        project_symbols,
        calls,
        bindings,
    })
}
