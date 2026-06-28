use crate::platform::urn::build_urn;
use crate::state::graph_kinds::{EdgeKind, NodeKind};
use crate::state::storage_cozo::{CozoStorage, GraphEdge, GraphNode};
use miette::{IntoDiagnostic, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OsvResult {
    pub results: Vec<OsvSourceResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OsvSourceResult {
    pub source: OsvSource,
    pub packages: Vec<OsvPackageResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OsvSource {
    pub path: String,
    #[serde(rename = "type")]
    pub source_type: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OsvPackageResult {
    pub package: OsvPackage,
    pub vulnerabilities: Option<Vec<OsvVulnerability>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OsvPackage {
    pub name: String,
    pub version: String,
    pub ecosystem: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OsvVulnerability {
    pub id: String,
    pub summary: Option<String>,
    pub details: Option<String>,
    pub modified: String,
    pub published: Option<String>,
    pub database_specific: Option<serde_json::Value>,
}

fn service_depends_on_manifest(service_root: &str, manifest_path: &str) -> bool {
    let clean_service_root = service_root
        .replace('\\', "/")
        .trim_start_matches("./")
        .trim_start_matches('/')
        .to_string();
    let clean_manifest_path = manifest_path
        .replace('\\', "/")
        .trim_start_matches("./")
        .trim_start_matches('/')
        .to_string();

    if !clean_manifest_path.contains('/') {
        return true;
    }

    if clean_service_root.is_empty() || clean_service_root == "." {
        return true;
    }

    clean_manifest_path.starts_with(&format!("{}/", clean_service_root))
}

pub struct OsvImporter;

impl OsvImporter {
    pub fn import_from_json(path: &Path) -> Result<OsvResult> {
        let content = std::fs::read_to_string(path).into_diagnostic()?;
        let result: OsvResult = serde_json::from_str(&content).into_diagnostic()?;
        Ok(result)
    }

    pub fn populate_kg(cozo: &CozoStorage, result: &OsvResult, provenance_id: &str) -> Result<()> {
        let mut nodes = Vec::new();
        let mut edges = Vec::new();

        // Query all services and their metadata from CozoDB
        let service_res = cozo.run_script(
            "?[id, metadata] := *node{id: id, category: 'service', metadata: metadata}",
        )?;
        let mut services = Vec::new();
        for row in service_res.rows {
            if let (Some(cozo::DataValue::Str(id)), Some(cozo::DataValue::Json(meta))) =
                (row.first(), row.get(1))
            {
                let service_root = meta
                    .get("root")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                services.push((id.to_string(), service_root));
            }
        }

        for src_res in &result.results {
            // Normalize path
            let normalized_path = src_res.source.path.replace('\\', "/");
            let file_urn = build_urn(NodeKind::File, &normalized_path);

            // Add File node
            nodes.push(GraphNode {
                id: file_urn.clone(),
                label: normalized_path.clone(),
                category: NodeKind::File,
                risk_score: 0.0,
                metadata: Some(json!({
                    "schema_version": "v1"
                })),
            });

            for pkg_res in &src_res.packages {
                let package_urn = build_urn(NodeKind::Package, &pkg_res.package.name);

                nodes.push(GraphNode {
                    id: package_urn.clone(),
                    label: pkg_res.package.name.clone(),
                    category: NodeKind::Package,
                    risk_score: 0.0,
                    metadata: Some(json!({
                        "version": pkg_res.package.version,
                        "ecosystem": pkg_res.package.ecosystem,
                        "schema_version": "v1"
                    })),
                });

                edges.push(GraphEdge {
                    source: file_urn.clone(),
                    target: package_urn.clone(),
                    relation: EdgeKind::DependsOn,
                    confidence: 1.0,
                    provenance_id: provenance_id.to_string(),
                });

                for (svc_urn, svc_root) in &services {
                    if service_depends_on_manifest(svc_root, &normalized_path) {
                        edges.push(GraphEdge {
                            source: svc_urn.clone(),
                            target: package_urn.clone(),
                            relation: EdgeKind::DependsOn,
                            confidence: 1.0,
                            provenance_id: provenance_id.to_string(),
                        });
                    }
                }

                if let Some(vulns) = &pkg_res.vulnerabilities {
                    for vuln in vulns {
                        let advisory_urn = build_urn(NodeKind::Advisory, &vuln.id);

                        nodes.push(GraphNode {
                            id: advisory_urn.clone(),
                            label: vuln.id.clone(),
                            category: NodeKind::Advisory,
                            risk_score: 0.8,
                            metadata: Some(json!({
                                "summary": vuln.summary,
                                "details": vuln.details,
                                "modified": vuln.modified,
                                "published": vuln.published,
                                "schema_version": "v1"
                            })),
                        });

                        edges.push(GraphEdge {
                            source: advisory_urn.clone(),
                            target: package_urn.clone(),
                            relation: EdgeKind::Affects,
                            confidence: 1.0,
                            provenance_id: provenance_id.to_string(),
                        });
                    }
                }
            }
        }

        cozo.insert_nodes(&nodes)?;
        cozo.insert_edges(&edges)?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_osv_importer_and_populate_kg() {
        let json_data = r#"{
            "results": [
                {
                    "source": {
                        "path": "Cargo.lock",
                        "type": "lockfile"
                    },
                    "packages": [
                        {
                            "package": {
                                "name": "foo-package",
                                "version": "1.0.0",
                                "ecosystem": "crates.io"
                            },
                            "vulnerabilities": [
                                {
                                    "id": "GHSA-1234-abcd-efgh",
                                    "summary": "Sample advisory",
                                    "details": "Details about sample advisory",
                                    "modified": "2026-01-01T00:00:00Z",
                                    "published": "2026-01-01T00:00:00Z"
                                }
                            ]
                        }
                    ]
                }
            ]
        }"#;

        let dir = tempdir().unwrap();
        let path = dir.path().join("osv.json");
        std::fs::write(&path, json_data).unwrap();

        let result = OsvImporter::import_from_json(&path).unwrap();
        assert_eq!(result.results.len(), 1);
        assert_eq!(result.results[0].packages.len(), 1);
        assert_eq!(result.results[0].packages[0].package.name, "foo-package");

        let cozo = CozoStorage::new_in_memory().unwrap();
        OsvImporter::populate_kg(&cozo, &result, "test-tx").unwrap();

        let nodes_res = cozo
            .run_script("?[id, label, category] := *node{id, label, category}")
            .unwrap();
        assert!(nodes_res.rows.len() >= 3);

        let edges_res = cozo
            .run_script("?[source, target, relation] := *edge{source, target, relation}")
            .unwrap();
        assert!(edges_res.rows.len() >= 2);
    }
}
