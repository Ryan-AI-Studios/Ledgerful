use crate::coverage::services::compute_cross_service_edges;
use crate::impact::enrichment::{EnrichmentContext, EnrichmentProvider};
use crate::impact::packet::{ImpactPacket, Service, ServiceMapDelta};
use crate::index::call_graph::{CallEdge, CallGraph, CallKind, ResolutionStatus};
use miette::{IntoDiagnostic, Result};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use tracing::debug;

pub struct ServiceProvider;

impl EnrichmentProvider for ServiceProvider {
    fn name(&self) -> &'static str {
        "Service Enrichment Provider"
    }

    fn enrich(&self, context: &EnrichmentContext, packet: &mut ImpactPacket) -> Result<()> {
        if !context.config.coverage.enabled || !context.config.coverage.services.enabled {
            debug!("Skipping service enrichment: coverage.services is disabled.");
            return Ok(());
        }

        if !context.storage.table_exists_and_has_data("project_files")? {
            debug!("Skipping service enrichment: project_files table is empty or missing.");
            return Ok(());
        }

        let conn = context.storage.get_connection();

        // 1. Detect affected services by checking service_name of changed files
        let mut affected_services_set = HashSet::new();
        for change in &packet.changes {
            let path_to_check = change.old_path.as_ref().unwrap_or(&change.path);
            let path_str = path_to_check.to_string_lossy().to_string();
            let service_name: Option<String> = conn
                .query_row(
                    "SELECT service_name FROM project_files WHERE file_path = ?1",
                    [path_str],
                    |row| row.get(0),
                )
                .unwrap_or(None);

            if let Some(name) = service_name {
                affected_services_set.insert(name);
            }
        }

        if affected_services_set.is_empty() {
            debug!("No services affected by these changes.");
            return Ok(());
        }

        // K4: Check CozoDB for downstream breakage
        if let Some(cozo) = context.storage.cozo.as_ref() {
            // Fetch all service_roots for prefix matching
            let mut service_roots = Vec::new();
            if let Ok(res) = cozo.run_script("?[name, dir_path] := *service_roots{name, dir_path}")
            {
                for row in res.rows {
                    if let (Some(cozo::DataValue::Str(name)), Some(cozo::DataValue::Str(dir))) =
                        (row.first(), row.get(1))
                    {
                        service_roots.push((name.to_string(), dir.to_string()));
                    }
                }
            }

            for change in &mut packet.changes {
                let path_to_check = change.old_path.as_ref().unwrap_or(&change.path);
                let path_str = path_to_check.to_string_lossy().replace('\\', "/");

                let has_contracts = !change.api_routes.is_empty() || !change.data_models.is_empty();
                if has_contracts {
                    let mut best_match: Option<&str> = None;
                    let mut best_len = 0;
                    for (name, dir) in &service_roots {
                        let dir_prefix = if dir == "." || dir.is_empty() {
                            ""
                        } else {
                            dir.as_str()
                        };
                        if path_str.starts_with(dir_prefix) && dir_prefix.len() >= best_len {
                            best_match = Some(name);
                            best_len = dir_prefix.len();
                        }
                    }

                    if let Some(svc_name) = best_match {
                        let mut broken_consumers = std::collections::HashSet::new();
                        for route in &change.api_routes {
                            let route_pattern = &route.path_pattern;
                            let script = format!(
                                "?[consumer] := *service_dependencies{{caller_service: consumer, pattern: p}}, p == '{}'",
                                route_pattern
                            );
                            if let Ok(res) = cozo.run_script(&script) {
                                for row in res.rows {
                                    if let Some(cozo::DataValue::Str(consumer)) = row.first() {
                                        broken_consumers.insert(consumer.to_string());
                                    }
                                }
                            }
                        }

                        if !broken_consumers.is_empty() {
                            change.analysis_warnings.push(format!("Downstream Breakage: Service '{}' has {} consumer(s) that may be affected by changes to its public contracts.", svc_name, broken_consumers.len()));
                            packet.risk_level = crate::impact::packet::RiskLevel::High;
                        }
                    }
                }
            }
        }

        // 2. Load All Known Services
        let mut service_stmt = conn
            .prepare(
                "SELECT DISTINCT service_name FROM project_files WHERE service_name IS NOT NULL",
            )
            .into_diagnostic()?;
        let service_names: Vec<String> = service_stmt
            .query_map([], |row| row.get(0))
            .into_diagnostic()?
            .collect::<rusqlite::Result<Vec<_>>>()
            .into_diagnostic()?;

        let total_services = service_names.len();
        let mut services = Vec::new();

        for name in &service_names {
            // Load routes
            let mut route_stmt = conn
                .prepare(
                    "SELECT ar.path_pattern 
                 FROM api_routes ar 
                 JOIN project_files pf ON ar.handler_file_id = pf.id 
                 WHERE pf.service_name = ?1",
                )
                .into_diagnostic()?;
            let routes = route_stmt
                .query_map([name], |row| row.get(0))
                .into_diagnostic()?
                .collect::<rusqlite::Result<Vec<String>>>()
                .into_diagnostic()?;

            // Load data models
            let mut model_stmt = conn
                .prepare(
                    "SELECT dm.model_name 
                 FROM data_models dm 
                 JOIN project_files pf ON dm.model_file_id = pf.id 
                 WHERE pf.service_name = ?1",
                )
                .into_diagnostic()?;
            let data_models = model_stmt
                .query_map([name], |row| row.get(0))
                .into_diagnostic()?
                .collect::<rusqlite::Result<Vec<String>>>()
                .into_diagnostic()?;

            // Load directory (take first file's parent as heuristic)
            let directory: String = conn
                .query_row(
                    "SELECT file_path FROM project_files WHERE service_name = ?1 LIMIT 1",
                    [name],
                    |row| row.get(0),
                )
                .unwrap_or_else(|_| ".".to_string());
            let directory = Path::new(&directory)
                .parent()
                .unwrap_or(Path::new("."))
                .to_path_buf();
            services.push(Service {
                name: name.clone(),
                directory,
                routes,
                data_models,
                owners: Vec::new(),
                runtime_name: None,
                queues: Vec::new(),
                topics: Vec::new(),
                rpc_endpoints: Vec::new(),
            });
        }

        // 3. Load Call Graph Edges
        let mut edge_stmt = conn
            .prepare(
                "SELECT COALESCE(ps_caller.qualified_name, ps_caller.symbol_name), \
                    pf_caller.file_path, \
                    COALESCE(ps_callee.qualified_name, ps_callee.symbol_name), \
                    pf_callee.file_path
             FROM structural_edges se
             JOIN project_symbols ps_caller ON se.caller_symbol_id = ps_caller.id
             JOIN project_files pf_caller ON se.caller_file_id = pf_caller.id
             JOIN project_symbols ps_callee ON se.callee_symbol_id = ps_callee.id
             JOIN project_files pf_callee ON se.callee_file_id = pf_callee.id",
            )
            .into_diagnostic()?;

        let edges = edge_stmt
            .query_map([], |row| {
                Ok(CallEdge {
                    caller_name: row.get(0)?,
                    caller_file: PathBuf::from(row.get::<_, String>(1)?),
                    callee_name: row.get(2)?,
                    callee_file: Some(PathBuf::from(row.get::<_, String>(3)?)),
                    call_kind: CallKind::Direct,
                    resolution_status: ResolutionStatus::Resolved,
                    confidence: 1.0,
                    evidence: "".to_string(),
                })
            })
            .into_diagnostic()?
            .collect::<rusqlite::Result<Vec<_>>>()
            .into_diagnostic()?;

        let call_graph = CallGraph { edges };
        let cross_service_edges = compute_cross_service_edges(&services, &call_graph);

        let mut affected_services: Vec<String> = affected_services_set.into_iter().collect();
        affected_services.sort();

        packet.service_map_delta = Some(ServiceMapDelta {
            services,
            affected_services,
            cross_service_edges,
            total_services,
        });

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::impact::packet::{ChangedFile, FileAnalysisStatus};
    use crate::state::migrations::get_migrations;
    use crate::state::storage::StorageManager;
    use rusqlite::Connection;
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    #[test]
    fn enrich_maps_services() {
        let mut conn = Connection::open_in_memory().unwrap();
        get_migrations().to_latest(&mut conn).unwrap();
        conn.execute(
            "INSERT INTO project_files (file_path, language, content_hash, file_size, last_indexed_at, service_name)
             VALUES ('svc/a.rs', 'Rust', 'hash', 1, '2026-01-01T00:00:00Z', 'svc_a')",
            [],
        )
        .unwrap();
        let file_id_a = conn.last_insert_rowid();

        conn.execute(
            "INSERT INTO project_files (file_path, language, content_hash, file_size, last_indexed_at, service_name)
             VALUES ('svc/b.rs', 'Rust', 'hash', 1, '2026-01-01T00:00:00Z', 'svc_b')",
            [],
        )
        .unwrap();
        let file_id_b = conn.last_insert_rowid();

        conn.execute(
            "INSERT INTO api_routes (method, path_pattern, handler_symbol_name, handler_file_id, framework, route_source, mount_prefix, is_dynamic, route_confidence, evidence, last_indexed_at)
             VALUES ('GET', '/a', 'handler_a', ?1, 'Axum', 'DECORATOR', NULL, 0, 1.0, 'test', '2026-01-01T00:00:00Z')",
            [file_id_a],
        )
        .unwrap();

        conn.execute(
            "INSERT INTO data_models (model_name, model_file_id, language, model_kind, confidence, evidence, last_indexed_at)
             VALUES ('ModelA', ?1, 'Rust', 'STRUCT', 1.0, 'test', '2026-01-01T00:00:00Z')",
            [file_id_a],
        )
        .unwrap();

        conn.execute(
            "INSERT INTO project_symbols (file_id, qualified_name, symbol_name, symbol_kind, last_indexed_at)
             VALUES (?1, 'crate::handler_a', 'handler_a', 'Function', '2026-01-01T00:00:00Z')",
            [file_id_a],
        )
        .unwrap();
        let sym_a = conn.last_insert_rowid();

        conn.execute(
            "INSERT INTO project_symbols (file_id, qualified_name, symbol_name, symbol_kind, last_indexed_at)
             VALUES (?1, 'crate::handler_b', 'handler_b', 'Function', '2026-01-01T00:00:00Z')",
            [file_id_b],
        )
        .unwrap();
        let sym_b = conn.last_insert_rowid();

        conn.execute(
            "INSERT INTO structural_edges (caller_symbol_id, caller_file_id, callee_symbol_id, callee_file_id, call_kind, resolution_status, confidence)
             VALUES (?1, ?2, ?3, ?4, 'DIRECT', 'RESOLVED', 1.0)",
            [sym_a, file_id_a, sym_b, file_id_b],
        )
        .unwrap();

        let storage = StorageManager::init_from_conn(conn);
        let mut file_id_map = HashMap::new();
        file_id_map.insert(PathBuf::from("svc/a.rs"), file_id_a);
        let mut config = crate::config::model::Config::default();
        config.coverage.enabled = true;
        config.coverage.services.enabled = true;
        let context = EnrichmentContext {
            storage: &storage,
            config: &config,
            file_id_map,
            project_root: PathBuf::new(),
            warnings: Arc::new(Mutex::new(Vec::new())),
        };
        let mut packet = ImpactPacket {
            changes: vec![ChangedFile {
                path: PathBuf::from("svc/a.rs"),
                status: "Modified".to_string(),
                old_path: None,
                is_staged: false,
                symbols: None,
                imports: None,
                runtime_usage: None,
                analysis_status: FileAnalysisStatus::default(),
                analysis_warnings: Vec::new(),
                api_routes: Vec::new(),
                data_models: Vec::new(),
                ci_gates: Vec::new(),
            }],
            ..Default::default()
        };

        ServiceProvider.enrich(&context, &mut packet).unwrap();

        let delta = packet.service_map_delta.unwrap();
        assert_eq!(delta.total_services, 2);
        assert!(delta.affected_services.contains(&"svc_a".to_string()));
    }
}
