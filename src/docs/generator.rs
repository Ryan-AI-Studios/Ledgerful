use camino::{Utf8Path, Utf8PathBuf};
use miette::Result;
use tracing::{info, warn};

use crate::docs::templates::*;
use crate::docs::types::DocTemplate;
use crate::state::storage_cozo::CozoStorage;

pub struct DocRegistry {
    templates: Vec<Box<dyn DocTemplate>>,
}

impl DocRegistry {
    pub fn new() -> Self {
        Self {
            templates: Vec::new(),
        }
    }

    pub fn register(&mut self, template: Box<dyn DocTemplate>) {
        self.templates.push(template);
    }

    pub fn default_registry() -> Self {
        let mut registry = Self::new();
        registry.register(Box::new(DependencyGraphTemplate));
        registry.register(Box::new(SymbolTableTemplate));
        registry.register(Box::new(ModuleSummaryTemplate));
        registry.register(Box::new(ModuleMapTemplate));
        registry.register(Box::new(SymbolIndexTemplate));
        registry.register(Box::new(ServiceBoundaryTemplate));
        registry.register(Box::new(ChangeHotspotReportTemplate));
        registry.register(Box::new(SemanticNeighborIndexTemplate));
        registry.register(Box::new(DataFlowDiagramTemplate));
        registry.register(Box::new(TestCoverageGapTemplate));
        // ApiContractIndexTemplate is excluded until implementation is complete
        registry.register(Box::new(AdrStalenessReportTemplate));
        registry.register(Box::new(CiPipelineMapTemplate));
        registry.register(Box::new(TokenProvenanceMapTemplate));
        registry.register(Box::new(FederationSummaryTemplate));
        registry.register(Box::new(DependencyHealthTemplate));
        registry.register(Box::new(ObservabilitySignalSnapshotTemplate));
        registry.register(Box::new(CallGraphDetailTemplate));
        registry
    }

    pub fn resolve(&self, name: &str) -> Option<&dyn DocTemplate> {
        self.templates
            .iter()
            .find(|t| t.name() == name)
            .map(|t| t.as_ref())
    }

    pub fn run_all(
        &self,
        storage: &CozoStorage,
        output_dir: &Utf8Path,
    ) -> Result<Vec<Utf8PathBuf>> {
        let mut paths = Vec::new();
        for template in &self.templates {
            let name = template.name();
            match template.generate(storage, output_dir) {
                Ok(path) => {
                    paths.push(path);
                }
                Err(err) => {
                    warn!("Template '{}' failed: {:#}", name, err);
                }
            }
        }
        Ok(paths)
    }

    pub fn run_filtered(
        &self,
        names: &[String],
        storage: &CozoStorage,
        output_dir: &Utf8Path,
    ) -> Result<Vec<Utf8PathBuf>> {
        let mut paths = Vec::new();
        for name in names {
            match self.resolve(name) {
                Some(template) => match template.generate(storage, output_dir) {
                    Ok(path) => paths.push(path),
                    Err(err) => warn!("Template '{}' failed: {:#}", name, err),
                },
                None => warn!("Template '{}' not found in registry", name),
            }
        }
        Ok(paths)
    }
}

impl Default for DocRegistry {
    fn default() -> Self {
        Self::default_registry()
    }
}

pub fn execute_export(
    storage: &CozoStorage,
    output_dir: &Utf8Path,
    templates: Option<Vec<String>>,
) -> Result<Vec<Utf8PathBuf>> {
    if !output_dir.exists() {
        std::fs::create_dir_all(output_dir)
            .map_err(|e| miette::miette!("Failed to create output dir: {}", e))?;
    }

    let registry = DocRegistry::default();
    let paths = if let Some(names) = templates {
        registry.run_filtered(&names, storage, output_dir)?
    } else {
        registry.run_all(storage, output_dir)?
    };

    info!("Exported {} documents to {}", paths.len(), output_dir);
    Ok(paths)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::graph_kinds::{EdgeKind, NodeKind};
    use crate::state::storage_cozo::{CozoStorage, GraphEdge, GraphNode};
    use camino::Utf8Path;

    fn in_memory_cozo() -> CozoStorage {
        CozoStorage::new_in_memory().unwrap()
    }

    fn populate_file_nodes(cozo: &CozoStorage, paths: &[&str]) {
        let nodes: Vec<GraphNode> = paths
            .iter()
            .map(|path| GraphNode {
                id: path.to_string(),
                label: path.to_string(),
                category: NodeKind::File,
                risk_score: 0.0,
                metadata: None,
            })
            .collect();
        cozo.insert_nodes(&nodes).unwrap();
    }

    #[allow(clippy::type_complexity)]
    fn populate_symbols(
        cozo: &CozoStorage,
        symbols: &[(i64, &str, &str, &str, &str, bool, i64, i64)],
    ) {
        for s in symbols {
            let script = format!(
                "?[id, file_path, qualified_name, symbol_name, symbol_kind, is_public, line_start, line_end] <- [[{}, \"{}\", \"{}\", \"{}\", \"{}\", {}, {}, {}]] :put project_symbol",
                s.0, s.1, s.2, s.3, s.4, s.5, s.6, s.7
            );
            cozo.run_script(&script).unwrap();
        }
    }

    fn populate_edges(cozo: &CozoStorage, edges: &[(&str, &str)]) {
        let graph_edges: Vec<GraphEdge> = edges
            .iter()
            .map(|(src, tgt)| GraphEdge {
                source: src.to_string(),
                target: tgt.to_string(),
                relation: EdgeKind::Calls,
                confidence: 1.0,
                provenance_id: "ev".to_string(),
            })
            .collect();
        cozo.insert_edges(&graph_edges).unwrap();
    }

    #[test]
    fn test_empty_kg_produces_valid_output() {
        let cozo = in_memory_cozo();
        let registry = DocRegistry::default();
        let tmp = tempfile::tempdir().unwrap();
        let output_dir = Utf8Path::from_path(tmp.path()).unwrap();
        let paths = registry.run_all(&cozo, output_dir).unwrap();
        assert_eq!(paths.len(), registry.templates.len());
        for path in &paths {
            let content = std::fs::read_to_string(path).unwrap();
            assert!(!content.is_empty());
        }
    }

    #[test]
    fn test_dependency_graph_output() {
        let cozo = in_memory_cozo();
        populate_file_nodes(&cozo, &["src/a.rs", "src/b.rs"]);
        populate_symbols(
            &cozo,
            &[
                (1, "src/a.rs", "A", "A", "fn", true, 1, 5),
                (2, "src/b.rs", "B", "B", "fn", true, 1, 5),
            ],
        );
        populate_edges(&cozo, &[("A", "B")]);

        let template = DependencyGraphTemplate;
        let tmp = tempfile::tempdir().unwrap();
        let output_dir = Utf8Path::from_path(tmp.path()).unwrap();
        let path = template.generate(&cozo, output_dir).unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("graph TD"));
        assert!(content.contains("src_a_rs"));
        assert!(content.contains("src_b_rs"));
    }
}
