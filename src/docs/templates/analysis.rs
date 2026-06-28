use crate::docs::types::{DocTemplate, write_file};
use crate::state::storage_cozo::CozoStorage;
use camino::{Utf8Path, Utf8PathBuf};
use cozo::DataValue;
use miette::Result;

pub struct ChangeHotspotReportTemplate;
pub struct SemanticNeighborIndexTemplate;
pub struct TestCoverageGapTemplate;
pub struct AdrStalenessReportTemplate;
pub struct TokenProvenanceMapTemplate;
pub struct DependencyHealthTemplate;
pub struct ObservabilitySignalSnapshotTemplate;
pub struct CallGraphDetailTemplate;

impl DocTemplate for ChangeHotspotReportTemplate {
    fn name(&self) -> &'static str {
        "change_hotspots"
    }

    fn description(&self) -> &'static str {
        "Identification of high-frequency change areas"
    }

    fn generate(&self, _storage: &CozoStorage, output_dir: &Utf8Path) -> Result<Utf8PathBuf> {
        let content =
            "# Change Hotspots\n\n*Hotspot report pending historical data aggregation logic.*\n";
        let path = output_dir.join("change_hotspots.md");
        write_file(&path, content)?;
        Ok(path)
    }
}

impl DocTemplate for SemanticNeighborIndexTemplate {
    fn name(&self) -> &'static str {
        "semantic_neighbors"
    }

    fn description(&self) -> &'static str {
        "Index of semantically similar code components"
    }

    fn generate(&self, storage: &CozoStorage, output_dir: &Utf8Path) -> Result<Utf8PathBuf> {
        let relations = storage
            .run_script("::relations")
            .map(|r| {
                r.rows
                    .into_iter()
                    .filter_map(|row| match row.first() {
                        Some(DataValue::Str(s)) => Some(s.to_string()),
                        _ => None,
                    })
                    .collect::<std::collections::HashSet<String>>()
            })
            .unwrap_or_default();

        if relations.contains("semantic_neighbor") {
            let script = r#"
                ?[node, neighbor, distance] := *semantic_neighbor{node, neighbor, distance}
            "#;
            let _res = storage
                .run_script(script)
                .map_err(|e| miette::miette!("Query failed: {}", e))?;
        }
        let content = "# Semantic Neighbor Index\n\n*Semantic neighbors pending vector similarity results.*\n";
        let path = output_dir.join("semantic_neighbors.md");
        write_file(&path, content)?;
        Ok(path)
    }
}

impl DocTemplate for TestCoverageGapTemplate {
    fn name(&self) -> &'static str {
        "coverage_gaps"
    }

    fn description(&self) -> &'static str {
        "Identification of symbols without direct test coverage"
    }

    fn generate(&self, storage: &CozoStorage, output_dir: &Utf8Path) -> Result<Utf8PathBuf> {
        let script = r#"
            ?[symbol] := *project_symbol{symbol_name: symbol},
                         not *edge{target: symbol, relation: "test_call"}
        "#;
        let _res = storage
            .run_script(script)
            .map_err(|e| miette::miette!("Query failed: {}", e))?;
        let content =
            "# Test Coverage Gaps\n\n*Coverage gap analysis pending test edge detection logic.*\n";
        let path = output_dir.join("coverage_gaps.md");
        write_file(&path, content)?;
        Ok(path)
    }
}

impl DocTemplate for AdrStalenessReportTemplate {
    fn name(&self) -> &'static str {
        "adr_staleness"
    }

    fn description(&self) -> &'static str {
        "Audit of ADR staleness based on commit history"
    }

    fn generate(&self, storage: &CozoStorage, output_dir: &Utf8Path) -> Result<Utf8PathBuf> {
        let script = r#"
            ?[file, last_decision] := *ledger_entry{entity_normalized: file, category: "DECISION", committed_at: last_decision}
        "#;
        let _res = storage
            .run_script(script)
            .map_err(|e| miette::miette!("Query failed: {}", e))?;
        let content = "# ADR Staleness Report\n\n*ADR staleness analysis pending ledger timestamp comparison logic.*\n";
        let path = output_dir.join("adr_staleness.md");
        write_file(&path, content)?;
        Ok(path)
    }
}

impl DocTemplate for TokenProvenanceMapTemplate {
    fn name(&self) -> &'static str {
        "token_provenance"
    }

    fn description(&self) -> &'static str {
        "Lineage of code tokens back to ledger transactions"
    }

    fn generate(&self, storage: &CozoStorage, output_dir: &Utf8Path) -> Result<Utf8PathBuf> {
        let script = r#"
            ?[token, tx_id] := *ledger_link{node_id: token, ledger_id: tx_id}
        "#;
        let _res = storage
            .run_script(script)
            .map_err(|e| miette::miette!("Query failed: {}", e))?;
        let content = "# Token Provenance Map\n\n*Token provenance mapping pending ledger link extraction logic.*\n";
        let path = output_dir.join("token_provenance.md");
        write_file(&path, content)?;
        Ok(path)
    }
}

impl DocTemplate for DependencyHealthTemplate {
    fn name(&self) -> &'static str {
        "dependency_health"
    }

    fn description(&self) -> &'static str {
        "Health score for project dependencies"
    }

    fn generate(&self, _storage: &CozoStorage, output_dir: &Utf8Path) -> Result<Utf8PathBuf> {
        let content = "# Dependency Health\n\n*Dependency health scoring pending advisory integration logic.*\n";
        let path = output_dir.join("dependency_health.md");
        write_file(&path, content)?;
        Ok(path)
    }
}

impl DocTemplate for ObservabilitySignalSnapshotTemplate {
    fn name(&self) -> &'static str {
        "observability_signals"
    }

    fn description(&self) -> &'static str {
        "Snapshot of observability markers in the codebase"
    }

    fn generate(&self, storage: &CozoStorage, output_dir: &Utf8Path) -> Result<Utf8PathBuf> {
        let script = r#"
            ?[marker] := *node{id: marker, category: "observability_signal"}
        "#;
        let _res = storage
            .run_script(script)
            .map_err(|e| miette::miette!("Query failed: {}", e))?;
        let content = "# Observability Signal Snapshot\n\n*Observability signals pending marker extraction logic.*\n";
        let path = output_dir.join("observability_signals.md");
        write_file(&path, content)?;
        Ok(path)
    }
}

impl DocTemplate for CallGraphDetailTemplate {
    fn name(&self) -> &'static str {
        "call_graph_detail"
    }

    fn description(&self) -> &'static str {
        "Detailed call graph for high-complexity areas"
    }

    fn generate(&self, storage: &CozoStorage, output_dir: &Utf8Path) -> Result<Utf8PathBuf> {
        let script = r#"
            ?[src, tgt] := *edge{source: src, target: tgt, relation: "call"}
        "#;
        let _res = storage
            .run_script(script)
            .map_err(|e| miette::miette!("Query failed: {}", e))?;
        let content =
            "# Call Graph Detail\n\n*Call graph details pending complexity filtering logic.*\n";
        let path = output_dir.join("call_graph_detail.md");
        write_file(&path, content)?;
        Ok(path)
    }
}
