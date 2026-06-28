use crate::docs::types::{
    DocGenerationError, DocTemplate, mermaid_id, query_file_dependencies, query_module_groups,
    write_file,
};
use crate::state::storage_cozo::CozoStorage;
use camino::{Utf8Path, Utf8PathBuf};
use miette::Result;
use std::collections::{BTreeMap, BTreeSet};

pub struct ModuleSummaryTemplate;
pub struct ServiceBoundaryTemplate;
pub struct FederationSummaryTemplate;

impl DocTemplate for ModuleSummaryTemplate {
    fn name(&self) -> &'static str {
        "module_summary"
    }

    fn description(&self) -> &'static str {
        "High-level module overview"
    }

    fn generate(&self, storage: &CozoStorage, output_dir: &Utf8Path) -> Result<Utf8PathBuf> {
        let modules = query_module_groups(storage)?;
        let deps = query_file_dependencies(storage)?;

        // Count inter-module edges
        let mut module_edges: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for dep in &deps {
            let src_dir = std::path::Path::new(&dep.source_file)
                .parent()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|| ".".to_string());
            let tgt_dir = std::path::Path::new(&dep.target_file)
                .parent()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|| ".".to_string());
            if src_dir != tgt_dir {
                module_edges.entry(src_dir).or_default().insert(tgt_dir);
            }
        }

        let mut lines = Vec::new();
        lines.push("# Module Summary".to_string());
        lines.push(String::new());

        for group in &modules {
            let edge_count = module_edges.get(&group.dir).map(|s| s.len()).unwrap_or(0);
            lines.push(format!(
                "- **{}**: {} file(s), {} outgoing inter-module edge(s)",
                group.dir,
                group.files.len(),
                edge_count
            ));
            for file in &group.files {
                lines.push(format!("  - `{file}`"));
            }
        }

        if modules.is_empty() {
            lines.push("*No file nodes found.*".to_string());
        }

        let content = lines.join("\n") + "\n";
        let path = output_dir.join("module_summary.md");
        write_file(&path, &content)?;
        Ok(path)
    }
}

impl DocTemplate for ServiceBoundaryTemplate {
    fn name(&self) -> &'static str {
        "service_boundary"
    }

    fn description(&self) -> &'static str {
        "Mermaid subgraph from Louvain communities"
    }

    fn generate(&self, storage: &CozoStorage, output_dir: &Utf8Path) -> Result<Utf8PathBuf> {
        let communities = storage
            .run_community_louvain()
            .map_err(|e| DocGenerationError::QueryFailed(e.to_string()))?;

        let mut groups: BTreeMap<i64, BTreeSet<String>> = BTreeMap::new();
        for (node, comm) in communities {
            groups
                .entry(comm)
                .or_default()
                .insert(node.replace('\\', "/"));
        }

        let mut lines = vec![
            "# Service Boundaries".to_string(),
            String::new(),
            "```mermaid".to_string(),
            "graph TD".to_string(),
        ];

        for (comm_id, nodes) in &groups {
            let subgraph_id = format!("community_{}", comm_id);
            lines.push(format!(
                "    subgraph {subgraph_id} [Community {}]",
                comm_id
            ));
            for node in nodes {
                let id = mermaid_id(node);
                lines.push(format!("        {id}[\"{}\"]", node));
            }
            lines.push("    end".to_string());
        }

        lines.push("```".to_string());

        if groups.is_empty() {
            lines.push(String::new());
            lines.push("*No communities detected.*".to_string());
        }

        let content = lines.join("\n") + "\n";
        let path = output_dir.join("service_boundary.md");
        write_file(&path, &content)?;
        Ok(path)
    }
}

impl DocTemplate for FederationSummaryTemplate {
    fn name(&self) -> &'static str {
        "federation_summary"
    }

    fn description(&self) -> &'static str {
        "Summary of cross-repository links"
    }

    fn generate(&self, storage: &CozoStorage, output_dir: &Utf8Path) -> Result<Utf8PathBuf> {
        let script = r#"
            ?[remote_repo, entity] := *ledger_entry{entity_normalized: entity, trace_id: remote_repo},
                                      remote_repo != ""
        "#;
        let res = storage
            .run_script(script)
            .map_err(|e| miette::miette!("Query failed: {}", e))?;

        let mut lines = vec![
            "# Federation Summary".to_string(),
            String::new(),
            "| Remote Repository | Linked Entity |".to_string(),
            "|---|---|".to_string(),
        ];

        for row in res.rows {
            if let (Some(cozo::DataValue::Str(repo)), Some(cozo::DataValue::Str(entity))) =
                (row.first(), row.get(1))
            {
                lines.push(format!("| {} | {} |", repo, entity));
            }
        }

        let content = lines.join("\n") + "\n";
        let path = output_dir.join("federation_summary.md");
        write_file(&path, &content)?;
        Ok(path)
    }
}
