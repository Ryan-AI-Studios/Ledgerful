use camino::{Utf8Path, Utf8PathBuf};
use cozo::{DataValue, Num};
use miette::{Diagnostic, IntoDiagnostic, Result};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use thiserror::Error;

use crate::state::storage_cozo::CozoStorage;

#[derive(Debug, Error, Diagnostic)]
pub enum DocGenerationError {
    #[error("CozoDB query failed: {0}")]
    QueryFailed(String),
    #[error("I/O error writing doc output")]
    IoFailed(#[from] std::io::Error),
    #[error("Knowledge Graph unavailable")]
    CozoUnavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct FileDependency {
    pub source_file: String,
    pub target_file: String,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct SymbolRow {
    pub qualified_name: String,
    pub symbol_name: String,
    pub symbol_kind: String,
    pub file_path: String,
    pub line_start: i64,
    pub line_end: i64,
    pub is_public: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleGroup {
    pub dir: String,
    pub files: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RiskyNode {
    pub id: String,
    pub label: String,
    pub category: String,
    pub risk_score: f64,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct NeighborEdge {
    pub source: String,
    pub target: String,
    pub relation: String,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct SymbolWithKind {
    pub qualified_name: String,
    pub symbol_name: String,
    pub symbol_kind: String,
    pub file_path: String,
}

pub trait DocTemplate: Send + Sync {
    fn name(&self) -> &'static str;
    fn description(&self) -> &'static str;
    fn generate(&self, storage: &CozoStorage, output_dir: &Utf8Path) -> Result<Utf8PathBuf>;
}

// --- Query functions ---

pub fn query_file_dependencies(cozo: &CozoStorage) -> Result<Vec<FileDependency>> {
    let script = r#"
        ?[source_file, target_file] := 
            *edge{source: src_sym, target: tgt_sym},
            *project_symbol{qualified_name: src_sym, file_path: source_file},
            *project_symbol{qualified_name: tgt_sym, file_path: target_file},
            source_file != target_file
    "#;
    let res = cozo
        .run_script(script)
        .map_err(|e| DocGenerationError::QueryFailed(e.to_string()))?;

    let mut dedup = BTreeSet::new();
    for row in res.rows {
        if let (Some(DataValue::Str(source)), Some(DataValue::Str(target))) =
            (row.first(), row.get(1))
        {
            let source_norm = source.to_string().replace('\\', "/");
            let target_norm = target.to_string().replace('\\', "/");
            if source_norm != target_norm {
                dedup.insert((source_norm, target_norm));
            }
        }
    }
    Ok(dedup
        .into_iter()
        .map(|(s, t)| FileDependency {
            source_file: s,
            target_file: t,
        })
        .collect())
}

pub fn query_symbol_table(cozo: &CozoStorage) -> Result<Vec<SymbolRow>> {
    let script = r#"
        ?[qualified_name, symbol_name, symbol_kind, file_path, line_start, line_end, is_public] := 
            *project_symbol{id, qualified_name, symbol_name, symbol_kind, file_path, line_start, line_end, is_public}
    "#;
    let res = cozo
        .run_script(script)
        .map_err(|e| DocGenerationError::QueryFailed(e.to_string()))?;

    let mut rows = Vec::new();
    for row in res.rows {
        if let (
            Some(DataValue::Str(qualified_name)),
            Some(DataValue::Str(symbol_name)),
            Some(DataValue::Str(symbol_kind)),
            Some(DataValue::Str(file_path)),
            Some(DataValue::Num(Num::Int(line_start))),
            Some(DataValue::Num(Num::Int(line_end))),
            Some(DataValue::Bool(is_public)),
        ) = (
            row.first(),
            row.get(1),
            row.get(2),
            row.get(3),
            row.get(4),
            row.get(5),
            row.get(6),
        ) {
            rows.push(SymbolRow {
                qualified_name: qualified_name.to_string(),
                symbol_name: symbol_name.to_string(),
                symbol_kind: symbol_kind.to_string(),
                file_path: file_path.to_string().replace('\\', "/"),
                line_start: *line_start,
                line_end: *line_end,
                is_public: *is_public,
            });
        }
    }
    rows.sort_by(|a, b| {
        a.file_path
            .cmp(&b.file_path)
            .then_with(|| a.line_start.cmp(&b.line_start))
            .then_with(|| a.qualified_name.cmp(&b.qualified_name))
    });
    Ok(rows)
}

pub fn query_module_groups(cozo: &CozoStorage) -> Result<Vec<ModuleGroup>> {
    let script = r#"
        ?[file_path, label] := *node{id: file_path, label, category: 'file'}
    "#;
    let res = cozo
        .run_script(script)
        .map_err(|e| DocGenerationError::QueryFailed(e.to_string()))?;

    let mut groups: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for row in res.rows {
        if let (Some(DataValue::Str(file_path)), Some(DataValue::Str(_label))) =
            (row.first(), row.get(1))
        {
            let path_norm = file_path.to_string().replace('\\', "/");
            let dir = std::path::Path::new(&path_norm)
                .parent()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|| ".".to_string());
            groups.entry(dir).or_default().insert(path_norm);
        }
    }

    let mut result = Vec::new();
    for (dir, files) in groups {
        let mut files_vec: Vec<String> = files.into_iter().collect();
        files_vec.sort();
        result.push(ModuleGroup {
            dir,
            files: files_vec,
        });
    }
    Ok(result)
}

pub fn query_risky_nodes(cozo: &CozoStorage) -> Result<Vec<RiskyNode>> {
    let script = r#"
        ?[id, label, category, risk_score] :=
            *node{id, label, category, risk_score}, risk_score > 0
    "#;
    let res = cozo
        .run_script(script)
        .map_err(|e| DocGenerationError::QueryFailed(e.to_string()))?;

    let mut rows = Vec::new();
    for row in res.rows {
        if let (
            Some(DataValue::Str(id)),
            Some(DataValue::Str(label)),
            Some(DataValue::Str(category)),
            Some(DataValue::Num(Num::Float(risk_score))),
        ) = (row.first(), row.get(1), row.get(2), row.get(3))
        {
            rows.push(RiskyNode {
                id: id.to_string(),
                label: label.to_string(),
                category: category.to_string(),
                risk_score: *risk_score,
            });
        }
    }
    rows.sort_by(|a, b| {
        b.risk_score
            .partial_cmp(&a.risk_score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.id.cmp(&b.id))
    });
    Ok(rows)
}

pub fn query_all_edges(cozo: &CozoStorage) -> Result<Vec<NeighborEdge>> {
    let script = r#"
        ?[source, target, relation] := *edge{source, target, relation}
    "#;
    let res = cozo
        .run_script(script)
        .map_err(|e| DocGenerationError::QueryFailed(e.to_string()))?;

    let mut dedup = BTreeSet::new();
    for row in res.rows {
        if let (
            Some(DataValue::Str(source)),
            Some(DataValue::Str(target)),
            Some(DataValue::Str(relation)),
        ) = (row.first(), row.get(1), row.get(2))
        {
            dedup.insert((source.to_string(), target.to_string(), relation.to_string()));
        }
    }
    Ok(dedup
        .into_iter()
        .map(|(s, t, r)| NeighborEdge {
            source: s,
            target: t,
            relation: r,
        })
        .collect())
}

pub fn query_symbols_with_kinds(cozo: &CozoStorage) -> Result<Vec<SymbolWithKind>> {
    let script = r#"
        ?[qualified_name, symbol_name, symbol_kind, file_path] :=
            *project_symbol{id, qualified_name, symbol_name, symbol_kind, file_path}
    "#;
    let res = cozo
        .run_script(script)
        .map_err(|e| DocGenerationError::QueryFailed(e.to_string()))?;

    let mut rows = Vec::new();
    for row in res.rows {
        if let (
            Some(DataValue::Str(qualified_name)),
            Some(DataValue::Str(symbol_name)),
            Some(DataValue::Str(symbol_kind)),
            Some(DataValue::Str(file_path)),
        ) = (row.first(), row.get(1), row.get(2), row.get(3))
        {
            rows.push(SymbolWithKind {
                qualified_name: qualified_name.to_string(),
                symbol_name: symbol_name.to_string(),
                symbol_kind: symbol_kind.to_string(),
                file_path: file_path.to_string().replace('\\', "/"),
            });
        }
    }
    rows.sort_by(|a, b| {
        a.file_path
            .cmp(&b.file_path)
            .then_with(|| a.qualified_name.cmp(&b.qualified_name))
    });
    Ok(rows)
}

// --- Helpers ---

pub fn mermaid_id(path: &str) -> String {
    path.replace(['/', '.', '-', ' '], "_")
}

pub fn write_file(path: &Utf8Path, content: &str) -> Result<()> {
    let mut file = std::fs::File::create(path).into_diagnostic()?;
    file.write_all(content.as_bytes()).into_diagnostic()?;
    Ok(())
}
