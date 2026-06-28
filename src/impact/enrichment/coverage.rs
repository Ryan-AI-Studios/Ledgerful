use crate::coverage::dataflow::compute_data_flow_coupling;
use crate::coverage::sdk::detect_sdk_changes;
use crate::coverage::traces::{detect_trace_config_changes, detect_trace_env_vars};
use crate::impact::enrichment::{EnrichmentContext, EnrichmentProvider};
use crate::impact::packet::{ApiRoute, DataModel, ImpactPacket};
use crate::index::call_graph::{CallEdge, CallGraph, CallKind, ResolutionStatus};
use miette::{IntoDiagnostic, Result};
use std::path::PathBuf;

pub struct CoverageProvider;

impl EnrichmentProvider for CoverageProvider {
    fn name(&self) -> &'static str {
        "Engineering Coverage Enrichment Provider"
    }

    fn enrich(&self, context: &EnrichmentContext, packet: &mut ImpactPacket) -> Result<()> {
        let config = &context.config.coverage;
        if !config.enabled {
            return Ok(());
        }

        // 1. Trace Config Drift
        if config.traces.enabled {
            packet.trace_config_drift =
                detect_trace_config_changes(&packet.changes, &config.traces.config_patterns);
            packet.trace_env_vars = detect_trace_env_vars(
                &packet.env_var_deps,
                &config.traces.env_var_patterns,
                &config.traces.exclude_env_patterns,
            );
        }

        // 2. SDK Dependencies Delta
        if config.sdk.enabled {
            packet.sdk_dependencies_delta = Some(detect_sdk_changes(
                &packet.changes,
                &config.sdk.patterns,
                &context.project_root,
            ));
        }

        // 3. Data Flow Enrichment
        if config.data_flow.enabled {
            self.enrich_data_flow(context, packet)?;
        }

        Ok(())
    }
}

impl CoverageProvider {
    fn enrich_data_flow(
        &self,
        context: &EnrichmentContext,
        packet: &mut ImpactPacket,
    ) -> Result<()> {
        let conn = context.storage.get_connection();

        // 1. Load all routes
        let mut route_stmt = conn
            .prepare("SELECT method, path_pattern, handler_symbol_name, framework, route_source, mount_prefix, is_dynamic, route_confidence, evidence, \
                      auth_requirements, schema_refs, owning_service, consumers FROM api_routes")
            .into_diagnostic()?;
        let routes: Vec<ApiRoute> = route_stmt
            .query_map([], |row| {
                let auth_raw: Option<String> = row.get(9)?;
                let schema_raw: Option<String> = row.get(10)?;
                let consumers_raw: Option<String> = row.get(12)?;

                let auth_requirements = auth_raw.and_then(|s| serde_json::from_str(&s).ok());
                let schema_refs = schema_raw.and_then(|s| serde_json::from_str(&s).ok());
                let consumers = consumers_raw.and_then(|s| serde_json::from_str(&s).ok());

                Ok(ApiRoute {
                    method: row.get(0)?,
                    path_pattern: row.get(1)?,
                    handler_symbol_name: row.get(2)?,
                    framework: row.get(3)?,
                    route_source: row.get(4)?,
                    mount_prefix: row.get(5)?,
                    is_dynamic: row.get::<_, i32>(6)? != 0,
                    route_confidence: row.get(7)?,
                    evidence: row.get(8)?,
                    auth_requirements,
                    schema_refs,
                    owning_service: row.get(11)?,
                    consumers,
                })
            })
            .into_diagnostic()?
            .collect::<rusqlite::Result<Vec<_>>>()
            .into_diagnostic()?;

        // 2. Load Call Graph Edges (full graph for data flow)
        let mut edge_stmt = conn
            .prepare(
                "SELECT COALESCE(ps_caller.qualified_name, ps_caller.symbol_name), \
                    pf_caller.file_path, \
                    COALESCE(ps_callee.qualified_name, ps_callee.symbol_name), \
                    pf_callee.file_path, \
                    se.call_kind \
             FROM structural_edges se \
             JOIN project_symbols ps_caller ON se.caller_symbol_id = ps_caller.id \
             JOIN project_files pf_caller ON se.caller_file_id = pf_caller.id \
             JOIN project_symbols ps_callee ON se.callee_symbol_id = ps_callee.id \
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
                    call_kind: match row.get::<_, String>(4)?.as_str() {
                        "METHOD_CALL" => CallKind::MethodCall,
                        "TRAIT_DISPATCH" => CallKind::TraitDispatch,
                        "DYNAMIC" => CallKind::Dynamic,
                        "EXTERNAL" => CallKind::External,
                        _ => CallKind::Direct,
                    },
                    resolution_status: ResolutionStatus::Resolved,
                    confidence: 1.0,
                    evidence: "".to_string(),
                })
            })
            .into_diagnostic()?
            .collect::<rusqlite::Result<Vec<_>>>()
            .into_diagnostic()?;

        let call_graph = CallGraph { edges };

        // 3. Load all Data Models
        let mut model_stmt = conn
            .prepare("SELECT model_name, model_kind, confidence, evidence FROM data_models")
            .into_diagnostic()?;
        let data_models: Vec<DataModel> = model_stmt
            .query_map([], |row| {
                Ok(DataModel {
                    model_name: row.get(0)?,
                    model_kind: row.get(1)?,
                    confidence: row.get(2)?,
                    evidence: row.get(3)?,
                })
            })
            .into_diagnostic()?
            .collect::<rusqlite::Result<Vec<_>>>()
            .into_diagnostic()?;

        // 4. Enumerate Call Chains
        let chains = call_graph.enumerate_call_chains(
            &routes,
            context.config.coverage.data_flow.chain_depth_max as usize,
        );

        // 5. Compute Coupling
        packet.data_flow_matches = compute_data_flow_coupling(
            &chains,
            &packet.changes,
            &data_models,
            0.2,
            &context.project_root,
        );

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
    fn enrich_trace_config_drift() {
        let mut conn = Connection::open_in_memory().unwrap();
        get_migrations().to_latest(&mut conn).unwrap();
        let storage = StorageManager::init_from_conn(conn);
        let mut config = crate::config::model::Config::default();
        config.coverage.enabled = true;
        config.coverage.traces.enabled = true;
        let context = EnrichmentContext {
            storage: &storage,
            config: &config,
            file_id_map: HashMap::new(),
            project_root: PathBuf::new(),
            warnings: Arc::new(Mutex::new(Vec::new())),
        };
        let mut packet = ImpactPacket {
            changes: vec![ChangedFile {
                path: PathBuf::from("otel-collector.yaml"),
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

        CoverageProvider.enrich(&context, &mut packet).unwrap();

        assert!(!packet.trace_config_drift.is_empty());
        assert_eq!(
            packet.trace_config_drift[0].config_type,
            crate::impact::packet::TraceConfigType::OpenTelemetryCollector
        );
    }

    #[test]
    fn enrich_data_flow_with_empty_db() {
        let mut conn = Connection::open_in_memory().unwrap();
        get_migrations().to_latest(&mut conn).unwrap();
        let storage = StorageManager::init_from_conn(conn);
        let mut config = crate::config::model::Config::default();
        config.coverage.enabled = true;
        config.coverage.data_flow.enabled = true;
        let context = EnrichmentContext {
            storage: &storage,
            config: &config,
            file_id_map: HashMap::new(),
            project_root: PathBuf::new(),
            warnings: Arc::new(Mutex::new(Vec::new())),
        };
        let mut packet = ImpactPacket::default();

        CoverageProvider.enrich(&context, &mut packet).unwrap();

        assert!(packet.data_flow_matches.is_empty());
    }

    #[test]
    fn test_data_flow_disabled_returns_empty() {
        let mut conn = Connection::open_in_memory().unwrap();
        get_migrations().to_latest(&mut conn).unwrap();
        let storage = StorageManager::init_from_conn(conn);

        // Seed DB with a route so that if data_flow were enabled it would run
        let conn_ref = storage.get_connection();
        conn_ref
            .execute(
                "INSERT INTO project_files (file_path, language, content_hash, file_size, last_indexed_at) VALUES (?1, ?2, ?3, ?4, ?5)",
                ("src/routes.rs", "Rust", "hash1", 100, "2026-05-01T00:00:00Z"),
            )
            .unwrap();
        let file_id = conn_ref.last_insert_rowid();
        conn_ref
            .execute(
                "INSERT INTO api_routes (method, path_pattern, handler_symbol_name, handler_file_id, framework, route_source, is_dynamic, route_confidence, last_indexed_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                ("GET", "/users", Some("get_users"), file_id, "Axum", "DECORATOR", 0, 1.0, "2026-05-01T00:00:00Z"),
            )
            .unwrap();

        let mut config = crate::config::model::Config::default();
        config.coverage.enabled = true;
        config.coverage.data_flow.enabled = false;
        let context = EnrichmentContext {
            storage: &storage,
            config: &config,
            file_id_map: HashMap::new(),
            project_root: PathBuf::new(),
            warnings: Arc::new(Mutex::new(Vec::new())),
        };
        let mut packet = ImpactPacket::default();

        CoverageProvider.enrich(&context, &mut packet).unwrap();

        assert!(
            packet.data_flow_matches.is_empty(),
            "data_flow_matches should be empty when data_flow is disabled"
        );
    }
}
