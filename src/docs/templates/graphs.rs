use crate::docs::types::{
    DocTemplate, mermaid_id, query_file_dependencies, query_module_groups, write_file,
};
use crate::state::storage_cozo::CozoStorage;
use camino::{Utf8Path, Utf8PathBuf};
use miette::Result;
use std::collections::BTreeSet;

pub struct DependencyGraphTemplate;
pub struct ModuleMapTemplate;
pub struct DataFlowDiagramTemplate;
pub struct CiPipelineMapTemplate;

impl DocTemplate for DependencyGraphTemplate {
    fn name(&self) -> &'static str {
        "dependency_graph"
    }

    fn description(&self) -> &'static str {
        "Mermaid diagram of file-level dependencies"
    }

    fn generate(&self, storage: &CozoStorage, output_dir: &Utf8Path) -> Result<Utf8PathBuf> {
        let deps = query_file_dependencies(storage)?;
        let mut lines = Vec::new();
        lines.push("graph TD".to_string());

        let mut nodes = BTreeSet::new();
        for dep in &deps {
            nodes.insert(dep.source_file.clone());
            nodes.insert(dep.target_file.clone());
        }

        for node in &nodes {
            let id = mermaid_id(node);
            lines.push(format!("    {id}[\"{node}\"]"));
        }

        for dep in &deps {
            let src = mermaid_id(&dep.source_file);
            let tgt = mermaid_id(&dep.target_file);
            lines.push(format!("    {src} --> {tgt}"));
        }

        let content = lines.join("\n") + "\n";
        let path = output_dir.join("dependency_graph.md");
        write_file(&path, &content)?;
        Ok(path)
    }
}

impl DocTemplate for ModuleMapTemplate {
    fn name(&self) -> &'static str {
        "module_map"
    }

    fn description(&self) -> &'static str {
        "Mermaid flowchart of file-level dependencies grouped by directory"
    }

    fn generate(&self, storage: &CozoStorage, output_dir: &Utf8Path) -> Result<Utf8PathBuf> {
        let deps = query_file_dependencies(storage)?;
        let modules = query_module_groups(storage)?;

        let mut lines = Vec::new();
        lines.push("flowchart TD".to_string());

        // Subgraphs per directory
        for group in &modules {
            let subgraph_id = mermaid_id(&group.dir);
            lines.push(format!("    subgraph {subgraph_id} [{}]", group.dir));
            for file in &group.files {
                let id = mermaid_id(file);
                lines.push(format!("        {id}[\"{file}\"]"));
            }
            lines.push("    end".to_string());
        }

        // Edges between files
        for dep in &deps {
            let src = mermaid_id(&dep.source_file);
            let tgt = mermaid_id(&dep.target_file);
            lines.push(format!("    {src} --> {tgt}"));
        }

        let content = lines.join("\n") + "\n";
        let path = output_dir.join("module_map.md");
        write_file(&path, &content)?;
        Ok(path)
    }
}

impl DocTemplate for DataFlowDiagramTemplate {
    fn name(&self) -> &'static str {
        "data_flow_diagram"
    }

    fn description(&self) -> &'static str {
        "Mermaid diagram of handler-to-model coupling using edge relations"
    }

    fn generate(&self, storage: &CozoStorage, output_dir: &Utf8Path) -> Result<Utf8PathBuf> {
        use crate::docs::types::{query_all_edges, query_symbols_with_kinds};
        use std::collections::{BTreeMap, BTreeSet};

        let symbols = query_symbols_with_kinds(storage)?;
        let edges = query_all_edges(storage)?;

        let symbol_kind_map: BTreeMap<String, String> = symbols
            .into_iter()
            .map(|s| (s.qualified_name, s.symbol_kind.to_lowercase()))
            .collect();

        let handler_kinds: BTreeSet<String> = ["function", "method", "fn"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let model_kinds: BTreeSet<String> = ["struct", "class", "interface", "type", "enum"]
            .iter()
            .map(|s| s.to_string())
            .collect();

        let mut lines = Vec::new();
        lines.push("flowchart LR".to_string());

        let mut drawn_nodes = BTreeSet::new();
        let mut drawn_edges = BTreeSet::new();

        for edge in &edges {
            let src_kind = symbol_kind_map.get(&edge.source).map(|s| s.as_str());
            let tgt_kind = symbol_kind_map.get(&edge.target).map(|s| s.as_str());

            let src_is_handler = src_kind.is_some_and(|k| handler_kinds.contains(k));
            let src_is_model = src_kind.is_some_and(|k| model_kinds.contains(k));
            let tgt_is_handler = tgt_kind.is_some_and(|k| handler_kinds.contains(k));
            let tgt_is_model = tgt_kind.is_some_and(|k| model_kinds.contains(k));

            let is_data_flow = (src_is_handler && tgt_is_model) || (src_is_model && tgt_is_handler);

            if is_data_flow {
                if !drawn_nodes.contains(&edge.source) {
                    let id = mermaid_id(&edge.source);
                    lines.push(format!("    {id}[\"{}\"]", edge.source));
                    drawn_nodes.insert(edge.source.clone());
                }
                if !drawn_nodes.contains(&edge.target) {
                    let id = mermaid_id(&edge.target);
                    lines.push(format!("    {id}[\"{}\"]", edge.target));
                    drawn_nodes.insert(edge.target.clone());
                }
                let edge_key = if edge.source < edge.target {
                    (edge.source.clone(), edge.target.clone())
                } else {
                    (edge.target.clone(), edge.source.clone())
                };
                if drawn_edges.insert(edge_key) {
                    let src_id = mermaid_id(&edge.source);
                    let tgt_id = mermaid_id(&edge.target);
                    lines.push(format!("    {src_id} --> {tgt_id}"));
                }
            }
        }

        if drawn_nodes.is_empty() {
            lines.push("    %% No handler-to-model edges detected".to_string());
        }

        let content = lines.join("\n") + "\n";
        let path = output_dir.join("data_flow_diagram.md");
        write_file(&path, &content)?;
        Ok(path)
    }
}

impl DocTemplate for CiPipelineMapTemplate {
    fn name(&self) -> &'static str {
        "ci_pipeline_map"
    }

    fn description(&self) -> &'static str {
        "Map of CI pipeline dependencies"
    }

    fn generate(&self, storage: &CozoStorage, output_dir: &Utf8Path) -> Result<Utf8PathBuf> {
        let script = r#"
            ?[ci_file, target] := *edge{source: ci_file, target: target},
                                  *node{id: ci_file, category: 'ci_config'}
        "#;
        let res = storage
            .run_script(script)
            .map_err(|e| miette::miette!("Query failed: {}", e))?;

        let mut lines = Vec::new();
        lines.push("graph TD".to_string());

        for row in res.rows {
            if let (Some(cozo::DataValue::Str(src)), Some(cozo::DataValue::Str(tgt))) =
                (row.first(), row.get(1))
            {
                lines.push(format!(
                    "    {} --> {}",
                    mermaid_id(src.as_str()),
                    mermaid_id(tgt.as_str())
                ));
            }
        }

        let content = lines.join("\n") + "\n";
        let path = output_dir.join("ci_pipeline_map.md");
        write_file(&path, &content)?;
        Ok(path)
    }
}
