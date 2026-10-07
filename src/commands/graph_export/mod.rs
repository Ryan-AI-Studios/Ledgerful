//! `ledgerful graph export` — Cozo node/edge interchange (GraphML / Cypher).

mod escape;
mod load;
mod render;

use load::{Selection, load_graph};
use miette::{IntoDiagnostic, Result, miette};
use render::{ExportGraph, render};
use std::io::Write;
use std::path::Path;

pub(crate) use load::escape_script_literal;
pub use render::ExportFormat as GraphExportFormat;

pub const DEFAULT_LIMIT: u64 = 1000;
pub const MAX_LIMIT: u64 = 1000;

pub fn execute_graph_export(
    format: GraphExportFormat,
    output: Option<std::path::PathBuf>,
    limit: u64,
    entity: Option<String>,
    depth: Option<u32>,
) -> Result<()> {
    let layout = crate::commands::helpers::get_layout()?;
    let storage = crate::state::storage::StorageManager::open_read_only(&layout)?;
    let cozo = storage
        .cozo()
        .ok_or_else(|| miette!("CozoDB not initialized. Run 'index' first."))?;
    if depth.is_some() && entity.is_none() {
        miette::bail!("--depth requires --entity");
    }
    let selection = if let Some(entity) = entity {
        Selection::Walk {
            entity,
            depth: depth.unwrap_or(2),
        }
    } else {
        Selection::IdPrefix
    };
    let graph = load_graph(cozo, limit, selection)?;
    let rendered = render(&graph, format)?;
    let report = stderr_report(format.name(), &graph, rendered.illegal_xml_replacements)?;
    if let Some(path) = output {
        write_export_file(&path, &rendered.body)?;
        write_stdout(&format!("wrote: {}\n", path.display()))?;
    } else {
        write_stdout(&rendered.body)?;
    }
    write_stderr(&report)?;
    Ok(())
}

pub fn stderr_report(format_name: &str, graph: &ExportGraph, illegal: u64) -> Result<String> {
    let mut out = format!(
        "source: Cozo nodes/edges; format: {format_name}; limit: {limit}; truncated: {truncated}; emitted_nodes: {emitted_nodes}; emitted_edges: {emitted_edges}; store_nodes: {store_nodes}; store_edges: {store_edges}\n",
        limit = graph.limit,
        truncated = if graph.truncated { "yes" } else { "no" },
        emitted_nodes = graph.nodes.len(),
        emitted_edges = graph.edges.len(),
        store_nodes = graph.store_nodes,
        store_edges = graph.store_edges,
    );
    if graph.truncated {
        out.push_str(&format!(
            "truncated: graph export capped at limit {}\n",
            graph.limit
        ));
    }
    if illegal > 0 {
        out.push_str(&format!(
            "graph export replaced {illegal} illegal XML characters\n"
        ));
    }
    Ok(out)
}

fn write_export_file(out: &Path, body: &str) -> Result<()> {
    if let Some(parent) = out.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent).into_diagnostic()?;
    }
    std::fs::write(out, body.as_bytes()).into_diagnostic()?;
    Ok(())
}

fn write_stdout(text: &str) -> Result<()> {
    let mut stdout = std::io::stdout();
    stdout.write_all(text.as_bytes()).into_diagnostic()?;
    stdout.flush().into_diagnostic()?;
    Ok(())
}

fn write_stderr(text: &str) -> Result<()> {
    let mut stderr = std::io::stderr();
    stderr.write_all(text.as_bytes()).into_diagnostic()?;
    stderr.flush().into_diagnostic()?;
    Ok(())
}

#[cfg(test)]
#[allow(non_snake_case)]
mod tests {
    use super::*;
    use render::{ExportEdge, ExportNode};

    fn empty_graph(truncated: bool) -> ExportGraph {
        ExportGraph {
            nodes: vec![ExportNode {
                id: "a".into(),
                label: "a".into(),
                category: "file".into(),
                risk_score: 0.0,
                metadata: None,
            }],
            edges: vec![ExportEdge {
                source: "a".into(),
                target: "a".into(),
                relation: "calls".into(),
                confidence: 1.0,
                provenance_id: String::new(),
            }],
            limit: 1000,
            truncated,
            store_nodes: 3,
            store_edges: 1,
        }
    }

    #[test]
    fn stderr_report__order__summary_then_cap_then_illegal() {
        let capped = stderr_report("graphml", &empty_graph(true), 2).unwrap();
        let lines: Vec<_> = capped.lines().collect();
        assert_eq!(lines.len(), 3);
        assert!(lines[0].starts_with("source: Cozo nodes/edges; format: graphml;"));
        assert!(lines[0].contains("truncated: yes"));
        assert_eq!(lines[1], "truncated: graph export capped at limit 1000");
        assert_eq!(lines[2], "graph export replaced 2 illegal XML characters");

        let plain = stderr_report("cypher", &empty_graph(false), 0).unwrap();
        assert_eq!(plain.lines().count(), 1);
        assert!(plain.contains("truncated: no"));
        assert!(!plain.contains("capped"));
    }
}
