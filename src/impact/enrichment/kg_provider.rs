use crate::impact::enrichment::{EnrichmentContext, EnrichmentProvider};
use crate::impact::packet::{ImpactPacket, KGImpact};
use crate::ui::spinner::Spinner;
use cozo::{DataValue, ScriptMutability};
use miette::Result;
use serde_json::json;
use std::collections::BTreeMap;
use std::time::Instant;
use tracing::{debug, warn};

use crate::platform::urn::build_urn;
use crate::state::graph_kinds::NodeKind;

pub struct KGProvider;

impl EnrichmentProvider for KGProvider {
    fn name(&self) -> &'static str {
        "KnowledgeGraph"
    }

    fn enrich(&self, context: &EnrichmentContext, packet: &mut ImpactPacket) -> Result<()> {
        let Some(cozo) = context.storage.cozo() else {
            debug!("CozoStorage not available, skipping KG enrichment");
            return Ok(());
        };

        // Soft-open / reviewer RO storage: never issue Cozo :put mutations.
        // Pure read queries below remain allowed for reachability enrichment.
        let allow_kg_writes = !context.storage.is_read_only;
        if !allow_kg_writes {
            debug!(
                "Storage is read-only; skipping KG hotspot/propagation writes (read queries only)"
            );
        }

        debug!("Enriching impact packet with Knowledge Graph data...");
        let spinner = Spinner::new("Enriching Knowledge Graph...");
        let start_time = Instant::now();
        let timeout_secs = context.config.coverage.kg_timeout_secs as u64;

        let check_timeout = |context: &EnrichmentContext| -> bool {
            if start_time.elapsed().as_secs() >= timeout_secs {
                context.add_warning("Knowledge Graph enrichment timed out".to_string());
                return true;
            }
            false
        };

        // 1. Sync Hotspots to KG risk scores (write path — skipped on RO)
        if allow_kg_writes && !packet.hotspots.is_empty() {
            if check_timeout(context) {
                spinner.finish();
                return Ok(());
            }

            let mut risk_updates = Vec::new();
            for hotspot in &packet.hotspots {
                let id = build_urn(NodeKind::File, &hotspot.path.to_string_lossy());
                risk_updates.push(json!([id, hotspot.score as f64]));
            }

            let sync_script = "updates[id, score] <- $batch\n?[id, label, category, risk_score, metadata] := *node{id, label, category, metadata}, updates[id, risk_score]\n:put node";
            let mut params = BTreeMap::new();
            params.insert(
                "batch".to_string(),
                DataValue::from(serde_json::Value::Array(risk_updates)),
            );
            if let Err(e) =
                cozo.run_script_with_params(sync_script, params, ScriptMutability::Mutable)
            {
                warn!("Failed to sync hotspots to KG: {}", e);
                context.add_warning(format!("Failed to sync hotspots to KG: {e}"));
            } else {
                debug!("Synced hotspots to KG");
            }

            // 1.1 Simple propagation (1-hop)
            if check_timeout(context) {
                spinner.finish();
                return Ok(());
            }

            let propagation_query = "?[id, s] := *node{id: src, risk_score: src_s}, *edge{source: src, target: id}, s = src_s * 0.5";
            if let Ok(res) = cozo.run_script(propagation_query) {
                let mut updates = Vec::new();
                for row in res.rows {
                    if let (Some(cozo::DataValue::Str(id)), Some(cozo::DataValue::Num(num))) =
                        (row.first(), row.get(1))
                    {
                        let score = match num {
                            cozo::Num::Float(f) => *f,
                            cozo::Num::Int(i) => *i as f64,
                        };
                        if score > 0.0 {
                            updates.push(serde_json::json!([id, score]));
                        }
                    }
                }
                if !updates.is_empty() {
                    let put_script = "updates[id, score] <- $batch\n?[id, label, category, risk_score, metadata] := *node{id, label, category, metadata, risk_score: current}, updates[id, score], score > current, risk_score = score\n:put node";
                    let mut params = BTreeMap::new();
                    params.insert(
                        "batch".to_string(),
                        DataValue::from(serde_json::Value::Array(updates)),
                    );
                    if let Err(e) =
                        cozo.run_script_with_params(put_script, params, ScriptMutability::Mutable)
                    {
                        warn!("Failed to apply propagated risk: {}", e);
                        context.add_warning(format!("Failed to apply propagated risk: {e}"));
                    }
                }
            }
        }

        // 2. Identify changed files/symbols in KG
        let mut seed_nodes: Vec<Vec<String>> = Vec::new();
        for file in &packet.changes {
            if check_timeout(context) {
                spinner.finish();
                return Ok(());
            }

            // Find nodes associated with this file
            let file_path = file.path.to_string_lossy();
            let file_urn = build_urn(NodeKind::File, &file_path);

            // Query for symbol nodes associated with this file, bound as `$fp`
            // (quotes/backslashes must not be interpolated into CozoScript).
            let query = "?[id] := *project_symbol{file_path: $fp, id: id}, *node{id: id}";
            let mut fp_params = BTreeMap::new();
            fp_params.insert("fp".to_string(), DataValue::from(file_path.as_ref()));

            // Also check the file node directly
            seed_nodes.push(vec![file_urn]);

            match cozo.run_script_with_params(query, fp_params, ScriptMutability::Immutable) {
                Ok(res) => {
                    for row in res.rows {
                        if let Some(cozo::DataValue::Str(id)) = row.first() {
                            seed_nodes.push(vec![id.to_string()]);
                        }
                    }
                }
                Err(e) => {
                    warn!("KG file_path seed lookup failed for {file_path}: {e}");
                    context.add_warning(format!(
                        "KG file_path seed lookup failed for {file_path}: {e}"
                    ));
                }
            }
        }

        if seed_nodes.is_empty() {
            debug!("No seed nodes found in KG for changes");
            spinner.finish();
            return Ok(());
        }

        // 2. Perform reachability analysis with recursive Datalog query
        let depth = context.config.coverage.max_reachability_depth;
        let query = format!(
            "seeds[id] <- $seeds\n\
             reachable[t, r, len] := seeds[s], *edge{{source: s, target: t, relation: r}}, len = 1\n\
             reachable[t, r, len] := reachable[m, _, len_prev], *edge{{source: m, target: t, relation: r}}, len = len_prev + 1, len <= {depth}\n\
             ?[t, r, len] := reachable[t, r, len]"
        );
        let mut seed_params = BTreeMap::new();
        seed_params.insert(
            "seeds".to_string(),
            DataValue::from(serde_json::Value::Array(
                seed_nodes.iter().map(|row| json!(row)).collect(),
            )),
        );

        if check_timeout(context) {
            spinner.finish();
            return Ok(());
        }

        match cozo.run_script_with_params(&query, seed_params, ScriptMutability::Immutable) {
            Ok(res) => {
                for row in res.rows {
                    if let (
                        Some(cozo::DataValue::Str(target)),
                        Some(cozo::DataValue::Str(rel)),
                        Some(cozo::DataValue::Num(num)),
                    ) = (row.first(), row.get(1), row.get(2))
                    {
                        let len = match num {
                            cozo::Num::Int(i) => *i as usize,
                            cozo::Num::Float(f) => *f as usize,
                        };
                        let impacted_category =
                            target.split(':').nth(2).unwrap_or("unknown").to_string();
                        packet.knowledge_graph.push(KGImpact {
                            source_node: "change_seed".to_string(),
                            source_category: "seed".to_string(),
                            impacted_node: target.to_string(),
                            impacted_category,
                            relation: rel.to_string(),
                            path_length: len,
                            reason: format!("KG reachability via {} ({} hops)", rel, len),
                        });
                    }
                }
            }
            Err(e) => {
                warn!("KG reachability query failed: {e}");
                context.add_warning(format!("KG reachability query failed: {e}"));
            }
        }

        spinner.finish();
        debug!(
            "KG enrichment added {} impact links",
            packet.knowledge_graph.len()
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::impact::enrichment::EnrichmentContext;
    use crate::impact::packet::{ChangedFile, Hotspot, ImpactPacket};
    use crate::state::graph_kinds::{EdgeKind, NodeKind};
    use crate::state::storage::StorageManager;
    use crate::state::storage_cozo::{CozoStorage, GraphEdge, GraphNode};
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    /// Soft-open / reviewer RO: hotspot :put paths must be skipped; pure
    /// reachability reads still run without panic or write requirement.
    #[test]
    fn test_kg_enrichment_skips_writes_when_storage_read_only() {
        let cozo = CozoStorage::new(&PathBuf::from("")).unwrap();

        let nodes = vec![
            GraphNode {
                id: build_urn(NodeKind::File, "file_1.rs"),
                label: "file_1.rs".to_string(),
                category: NodeKind::File,
                risk_score: 0.0,
                metadata: None,
            },
            GraphNode {
                id: build_urn(NodeKind::File, "file_2.rs"),
                label: "file_2.rs".to_string(),
                category: NodeKind::File,
                risk_score: 0.0,
                metadata: None,
            },
        ];
        cozo.insert_nodes(&nodes).unwrap();
        cozo.insert_edges(&[GraphEdge {
            source: build_urn(NodeKind::File, "file_1.rs"),
            target: build_urn(NodeKind::File, "file_2.rs"),
            relation: EdgeKind::DependsOn,
            confidence: 1.0,
            provenance_id: "tx1".to_string(),
        }])
        .unwrap();

        let mut storage =
            StorageManager::init_from_conn(rusqlite::Connection::open_in_memory().unwrap());
        storage.set_cozo(Some(cozo));
        storage.is_read_only = true;

        let context = EnrichmentContext {
            storage: &storage,
            config: &crate::config::model::Config::default(),
            file_id_map: HashMap::new(),
            project_root: PathBuf::from("."),
            warnings: Arc::new(Mutex::new(Vec::new())),
            deadline: std::time::Instant::now() + std::time::Duration::from_secs(120),
        };

        let mut packet = ImpactPacket {
            changes: vec![ChangedFile {
                path: PathBuf::from("file_1.rs"),
                status: "Modified".to_string(),
                is_staged: true,
                ..Default::default()
            }],
            // Non-empty hotspots would previously issue :put node scripts.
            hotspots: vec![Hotspot {
                path: PathBuf::from("file_1.rs"),
                score: 0.9,
                display_score: 0.9,
                complexity: 5,
                frequency: 1.0,
                centrality: None,
            }],
            ..Default::default()
        };

        let provider = KGProvider;
        provider.enrich(&context, &mut packet).unwrap();

        let nodes: Vec<String> = packet
            .knowledge_graph
            .iter()
            .map(|k| k.impacted_node.clone())
            .collect();
        assert!(
            nodes.contains(&build_urn(NodeKind::File, "file_2.rs")),
            "RO enrich should still run pure reachability reads"
        );
    }

    #[test]
    fn test_kg_enrichment() {
        let cozo = CozoStorage::new(&PathBuf::from("")).unwrap();

        // Setup KG data
        let nodes = vec![
            GraphNode {
                id: build_urn(NodeKind::File, "file_1.rs"),
                label: "file_1.rs".to_string(),
                category: NodeKind::File,
                risk_score: 0.0,
                metadata: None,
            },
            GraphNode {
                id: build_urn(NodeKind::File, "file_2.rs"),
                label: "file_2.rs".to_string(),
                category: NodeKind::File,
                risk_score: 0.0,
                metadata: None,
            },
            GraphNode {
                id: build_urn(NodeKind::File, "file_3.rs"),
                label: "file_3.rs".to_string(),
                category: NodeKind::File,
                risk_score: 0.0,
                metadata: None,
            },
        ];
        cozo.insert_nodes(&nodes).unwrap();

        let edges = vec![
            GraphEdge {
                source: build_urn(NodeKind::File, "file_1.rs"),
                target: build_urn(NodeKind::File, "file_2.rs"),
                relation: EdgeKind::DependsOn,
                confidence: 1.0,
                provenance_id: "tx1".to_string(),
            },
            GraphEdge {
                source: build_urn(NodeKind::File, "file_2.rs"),
                target: build_urn(NodeKind::File, "file_3.rs"),
                relation: EdgeKind::DependsOn,
                confidence: 1.0,
                provenance_id: "tx2".to_string(),
            },
        ];
        cozo.insert_edges(&edges).unwrap();

        let mut storage =
            StorageManager::init_from_conn(rusqlite::Connection::open_in_memory().unwrap());
        storage.set_cozo(Some(cozo));

        let context = EnrichmentContext {
            storage: &storage,
            config: &crate::config::model::Config::default(),
            file_id_map: HashMap::new(),
            project_root: PathBuf::from("."),
            warnings: Arc::new(Mutex::new(Vec::new())),
            deadline: std::time::Instant::now() + std::time::Duration::from_secs(120),
        };

        let mut packet = ImpactPacket {
            changes: vec![ChangedFile {
                path: PathBuf::from("file_1.rs"),
                status: "Modified".to_string(),
                is_staged: true,
                ..Default::default()
            }],
            ..Default::default()
        };

        let provider = KGProvider;
        provider.enrich(&context, &mut packet).unwrap();

        // Should find file_2 (1 hop) and file_3 (2 hops)
        assert!(packet.knowledge_graph.len() >= 2);
        let nodes: Vec<String> = packet
            .knowledge_graph
            .iter()
            .map(|k| k.impacted_node.clone())
            .collect();
        assert!(nodes.contains(&build_urn(NodeKind::File, "file_2.rs")));
        assert!(nodes.contains(&build_urn(NodeKind::File, "file_3.rs")));

        // Verify categories are populated
        for impact in &packet.knowledge_graph {
            assert_eq!(impact.impacted_category, "file");
            assert_eq!(impact.source_category, "seed");
        }
    }

    #[test]
    fn test_kg_enrichment_transitive_and_mixed() {
        let cozo = CozoStorage::new(&PathBuf::from("")).unwrap();

        // Setup 4 files
        let nodes = vec![
            GraphNode {
                id: build_urn(NodeKind::File, "file_1.rs"),
                label: "file_1.rs".to_string(),
                category: NodeKind::File,
                risk_score: 0.0,
                metadata: None,
            },
            GraphNode {
                id: build_urn(NodeKind::File, "file_2.rs"),
                label: "file_2.rs".to_string(),
                category: NodeKind::File,
                risk_score: 0.0,
                metadata: None,
            },
            GraphNode {
                id: build_urn(NodeKind::File, "file_3.rs"),
                label: "file_3.rs".to_string(),
                category: NodeKind::File,
                risk_score: 0.0,
                metadata: None,
            },
            GraphNode {
                id: build_urn(NodeKind::File, "file_4.rs"),
                label: "file_4.rs".to_string(),
                category: NodeKind::File,
                risk_score: 0.0,
                metadata: None,
            },
        ];
        cozo.insert_nodes(&nodes).unwrap();

        // 3-hop mixed relation path:
        // file_1 -(DependsOn)-> file_2 -(Calls)-> file_3 -(DependsOn)-> file_4
        let edges = vec![
            GraphEdge {
                source: build_urn(NodeKind::File, "file_1.rs"),
                target: build_urn(NodeKind::File, "file_2.rs"),
                relation: EdgeKind::DependsOn,
                confidence: 1.0,
                provenance_id: "tx1".to_string(),
            },
            GraphEdge {
                source: build_urn(NodeKind::File, "file_2.rs"),
                target: build_urn(NodeKind::File, "file_3.rs"),
                relation: EdgeKind::Calls,
                confidence: 1.0,
                provenance_id: "tx2".to_string(),
            },
            GraphEdge {
                source: build_urn(NodeKind::File, "file_3.rs"),
                target: build_urn(NodeKind::File, "file_4.rs"),
                relation: EdgeKind::DependsOn,
                confidence: 1.0,
                provenance_id: "tx3".to_string(),
            },
        ];
        cozo.insert_edges(&edges).unwrap();

        let mut storage =
            StorageManager::init_from_conn(rusqlite::Connection::open_in_memory().unwrap());
        storage.set_cozo(Some(cozo));

        // Test with max_reachability_depth = 2 (should find file_2 and file_3, but NOT file_4)
        {
            let mut config = crate::config::model::Config::default();
            config.coverage.max_reachability_depth = 2;

            let context = EnrichmentContext {
                storage: &storage,
                config: &config,
                file_id_map: HashMap::new(),
                project_root: PathBuf::from("."),
                warnings: Arc::new(Mutex::new(Vec::new())),
                deadline: std::time::Instant::now() + std::time::Duration::from_secs(120),
            };

            let mut packet = ImpactPacket {
                changes: vec![ChangedFile {
                    path: PathBuf::from("file_1.rs"),
                    status: "Modified".to_string(),
                    is_staged: true,
                    ..Default::default()
                }],
                ..Default::default()
            };

            let provider = KGProvider;
            provider.enrich(&context, &mut packet).unwrap();

            let nodes: Vec<String> = packet
                .knowledge_graph
                .iter()
                .map(|k| k.impacted_node.clone())
                .collect();
            assert!(nodes.contains(&build_urn(NodeKind::File, "file_2.rs")));
            assert!(nodes.contains(&build_urn(NodeKind::File, "file_3.rs")));
            assert!(
                !nodes.contains(&build_urn(NodeKind::File, "file_4.rs")),
                "Should not reach file_4 with depth limit 2"
            );
        }

        // Test with max_reachability_depth = 3 (should find all up to file_4)
        {
            let mut config = crate::config::model::Config::default();
            config.coverage.max_reachability_depth = 3;

            let context = EnrichmentContext {
                storage: &storage,
                config: &config,
                file_id_map: HashMap::new(),
                project_root: PathBuf::from("."),
                warnings: Arc::new(Mutex::new(Vec::new())),
                deadline: std::time::Instant::now() + std::time::Duration::from_secs(120),
            };

            let mut packet = ImpactPacket {
                changes: vec![ChangedFile {
                    path: PathBuf::from("file_1.rs"),
                    status: "Modified".to_string(),
                    is_staged: true,
                    ..Default::default()
                }],
                ..Default::default()
            };

            let provider = KGProvider;
            provider.enrich(&context, &mut packet).unwrap();

            let nodes: Vec<String> = packet
                .knowledge_graph
                .iter()
                .map(|k| k.impacted_node.clone())
                .collect();
            assert!(nodes.contains(&build_urn(NodeKind::File, "file_2.rs")));
            assert!(nodes.contains(&build_urn(NodeKind::File, "file_3.rs")));
            assert!(nodes.contains(&build_urn(NodeKind::File, "file_4.rs")));
        }
    }

    #[test]
    fn test_kg_enrichment_quoted_file_path_bind() {
        let cozo = CozoStorage::new(&PathBuf::from("")).unwrap();
        let quoted = "src/o'reilly\\mod.rs";
        let neighbor = "src/neighbor.rs";
        cozo.insert_nodes(&[
            GraphNode {
                id: build_urn(NodeKind::File, quoted),
                label: quoted.to_string(),
                category: NodeKind::File,
                risk_score: 0.0,
                metadata: None,
            },
            GraphNode {
                id: build_urn(NodeKind::File, neighbor),
                label: neighbor.to_string(),
                category: NodeKind::File,
                risk_score: 0.0,
                metadata: None,
            },
        ])
        .unwrap();
        cozo.insert_edges(&[GraphEdge {
            source: build_urn(NodeKind::File, quoted),
            target: build_urn(NodeKind::File, neighbor),
            relation: EdgeKind::DependsOn,
            confidence: 1.0,
            provenance_id: "txq".to_string(),
        }])
        .unwrap();

        let mut storage =
            StorageManager::init_from_conn(rusqlite::Connection::open_in_memory().unwrap());
        storage.set_cozo(Some(cozo));
        let warnings = Arc::new(Mutex::new(Vec::new()));
        let context = EnrichmentContext {
            storage: &storage,
            config: &crate::config::model::Config::default(),
            file_id_map: HashMap::new(),
            project_root: PathBuf::from("."),
            warnings: Arc::clone(&warnings),
            deadline: std::time::Instant::now() + std::time::Duration::from_secs(120),
        };
        let mut packet = ImpactPacket {
            changes: vec![ChangedFile {
                path: PathBuf::from(quoted),
                status: "Modified".to_string(),
                is_staged: true,
                ..Default::default()
            }],
            ..Default::default()
        };
        KGProvider.enrich(&context, &mut packet).unwrap();
        let nodes: Vec<String> = packet
            .knowledge_graph
            .iter()
            .map(|k| k.impacted_node.clone())
            .collect();
        assert!(
            nodes.contains(&build_urn(NodeKind::File, neighbor)),
            "quoted/backslash path must bind as $fp and still enrich: {nodes:?}"
        );
        let warns = warnings.lock().expect("warnings mutex");
        assert!(
            warns.iter().all(|w| !w.contains("seed lookup failed")),
            "quoted path must not fail $fp bind: {warns:?}"
        );
    }
}
