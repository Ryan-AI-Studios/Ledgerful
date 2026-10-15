use crate::impact::enrichment::{EnrichmentContext, EnrichmentProvider};
use crate::impact::packet::{ImpactPacket, Service, ServiceMapDelta};
use miette::{IntoDiagnostic, Result};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::Path;
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
            let path_to_check = match change.old_path.as_ref() {
                Some(old_path) => old_path,
                None => &change.path,
            };
            let path_str = path_to_check.to_string_lossy().replace('\\', "/");
            let looked_up = conn.query_row(
                "SELECT service_name FROM project_files WHERE file_path = ?1",
                [path_str],
                |row| row.get::<_, Option<String>>(0),
            );
            if let Some(name) = optional_text(looked_up)?
                && !name.is_empty()
            {
                affected_services_set.insert(name);
            }
        }

        if affected_services_set.is_empty() {
            debug!("No services affected by these changes.");
            return Ok(());
        }

        // K4: Check CozoDB for downstream breakage
        if let Some(cozo) = context.storage.cozo() {
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

            let mut escalate_risk = false;
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
                            escalate_risk = true;
                        }
                    }
                }
            }
            if escalate_risk {
                packet.set_risk_level(crate::impact::packet::RiskLevel::High);
            }
        }

        let mut candidates: Vec<String> = affected_services_set.into_iter().collect();
        candidates.sort();
        let route_counts = load_api_route_counts(conn, &candidates)?;
        let affected_services: Vec<String> = candidates
            .into_iter()
            .filter(|name| {
                let declared = name_is_declared(name, &context.config.services.definitions);
                let count = match route_counts.get(name) {
                    Some(count) => *count,
                    None => 0,
                };
                crate::coverage::services::is_canonical_service_name(name, declared, count)
            })
            .collect();
        if affected_services.is_empty() {
            debug!("No canonical services affected by these changes.");
            return Ok(());
        }

        // 2. Neighbors are the other service_name on a structural edge that
        // touches an affected service. Do not load every service_name.
        let placeholders = vec!["?"; affected_services.len()].join(",");
        let edge_sql = format!(
            "SELECT pf_caller.service_name, pf_callee.service_name \
             FROM structural_edges se \
             JOIN project_files pf_caller ON se.caller_file_id = pf_caller.id \
             JOIN project_files pf_callee ON se.callee_file_id = pf_callee.id \
             WHERE pf_caller.service_name IN ({placeholders}) \
                OR pf_callee.service_name IN ({placeholders})"
        );
        let mut bind: Vec<&str> = Vec::with_capacity(affected_services.len() * 2);
        for name in &affected_services {
            bind.push(name.as_str());
        }
        bind.extend(affected_services.iter().map(String::as_str));

        let mut edge_stmt = conn.prepare(&edge_sql).into_diagnostic()?;
        let pairs = edge_stmt
            .query_map(rusqlite::params_from_iter(bind), |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, Option<String>>(1)?,
                ))
            })
            .into_diagnostic()?
            .collect::<rusqlite::Result<Vec<_>>>()
            .into_diagnostic()?;

        let affected_lookup: HashSet<&str> = affected_services.iter().map(String::as_str).collect();
        let mut neighbors: BTreeSet<String> = BTreeSet::new();
        let mut edge_counts: BTreeMap<(String, String), usize> = BTreeMap::new();
        for (caller, callee) in pairs {
            let (Some(caller), Some(callee)) = (caller, callee) else {
                continue;
            };
            if caller.is_empty() || callee.is_empty() || caller == callee {
                continue;
            }
            let caller_in = affected_lookup.contains(caller.as_str());
            let callee_in = affected_lookup.contains(callee.as_str());
            if !caller_in && !callee_in {
                continue;
            }
            if caller_in && !callee_in {
                neighbors.insert(callee.clone());
            } else if callee_in && !caller_in {
                neighbors.insert(caller.clone());
            }
            *edge_counts.entry((caller, callee)).or_default() += 1;
        }

        if !neighbors.is_empty() {
            let neighbor_names: Vec<String> = neighbors.iter().cloned().collect();
            let neighbor_counts = load_api_route_counts(conn, &neighbor_names)?;
            neighbors.retain(|name| {
                let declared = name_is_declared(name, &context.config.services.definitions);
                let count = match neighbor_counts.get(name) {
                    Some(count) => *count,
                    None => 0,
                };
                crate::coverage::services::is_canonical_service_name(name, declared, count)
            });
        }

        let mut keep_names: BTreeSet<String> = affected_services.iter().cloned().collect();
        keep_names.extend(neighbors);
        let mut services = Vec::new();

        for name in &keep_names {
            let mut route_stmt = conn
                .prepare(
                    "SELECT ar.path_pattern \
                     FROM api_routes ar \
                     JOIN project_files pf ON ar.handler_file_id = pf.id \
                     WHERE pf.service_name = ?1 \
                     ORDER BY ar.path_pattern",
                )
                .into_diagnostic()?;
            let mut routes = route_stmt
                .query_map([name], |row| row.get(0))
                .into_diagnostic()?
                .collect::<rusqlite::Result<Vec<String>>>()
                .into_diagnostic()?;
            routes.sort();

            let mut model_stmt = conn
                .prepare(
                    "SELECT dm.model_name \
                     FROM data_models dm \
                     JOIN project_files pf ON dm.model_file_id = pf.id \
                     WHERE pf.service_name = ?1 \
                     ORDER BY dm.model_name",
                )
                .into_diagnostic()?;
            let mut data_models = model_stmt
                .query_map([name], |row| row.get(0))
                .into_diagnostic()?
                .collect::<rusqlite::Result<Vec<String>>>()
                .into_diagnostic()?;
            data_models.sort();

            let (directory, owners, runtime_name, queues, topics, rpc_endpoints) = if let Some(
                definition,
            ) =
                crate::coverage::services::selected_service_definition(
                    name,
                    &context.config.services.definitions,
                ) {
                let mut owners = definition.owners.clone();
                owners.sort();
                let mut queues = definition.queues.clone();
                queues.sort();
                let mut topics = definition.topics.clone();
                topics.sort();
                let mut rpc_endpoints = definition.rpc_endpoints.clone();
                rpc_endpoints.sort();
                (
                    std::path::PathBuf::from(definition.root.replace('\\', "/")),
                    owners,
                    definition.runtime_name.clone(),
                    queues,
                    topics,
                    rpc_endpoints,
                )
            } else {
                let looked_up = conn.query_row(
                    "SELECT file_path FROM project_files WHERE service_name = ?1 ORDER BY file_path LIMIT 1",
                    [name],
                    |row| row.get::<_, String>(0),
                );
                (
                    directory_from_file_lookup(looked_up)?,
                    Vec::new(),
                    None,
                    Vec::new(),
                    Vec::new(),
                    Vec::new(),
                )
            };
            services.push(Service {
                name: name.clone(),
                directory,
                routes,
                data_models,
                owners,
                runtime_name,
                queues,
                topics,
                rpc_endpoints,
            });
        }

        let total_services = services.len();
        let cross_service_edges: Vec<(String, String, usize)> = edge_counts
            .into_iter()
            .filter(|((caller, callee), _)| {
                keep_names.contains(caller) && keep_names.contains(callee)
            })
            .map(|((caller, callee), count)| (caller, callee, count))
            .collect();

        packet.set_service_map_delta(Some(ServiceMapDelta {
            services,
            affected_services,
            cross_service_edges,
            total_services,
        }));

        Ok(())
    }
}

fn optional_text(result: rusqlite::Result<Option<String>>) -> Result<Option<String>> {
    match result {
        Ok(value) => Ok(value),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(err) => Err(err).into_diagnostic(),
    }
}

fn name_is_declared(name: &str, definitions: &[crate::config::model::ServiceDefinition]) -> bool {
    definitions.iter().any(|definition| definition.name == name)
}

fn load_api_route_counts(
    conn: &rusqlite::Connection,
    names: &[String],
) -> Result<std::collections::HashMap<String, i64>> {
    let mut counts = std::collections::HashMap::new();
    if names.is_empty() {
        return Ok(counts);
    }
    let placeholders = vec!["?"; names.len()].join(",");
    let sql = format!(
        "SELECT pf.service_name, count(ar.id) \
         FROM project_files pf \
         LEFT JOIN api_routes ar ON pf.id = ar.handler_file_id \
         WHERE pf.service_name IN ({placeholders}) \
         GROUP BY pf.service_name"
    );
    let mut stmt = conn.prepare(&sql).into_diagnostic()?;
    let rows = stmt
        .query_map(rusqlite::params_from_iter(names.iter()), |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })
        .into_diagnostic()?
        .collect::<rusqlite::Result<Vec<_>>>()
        .into_diagnostic()?;
    for (name, count) in rows {
        counts.insert(name, count);
    }
    Ok(counts)
}

fn parent_directory(file_path: &str) -> std::path::PathBuf {
    match Path::new(file_path).parent() {
        None => std::path::PathBuf::from("."),
        Some(parent) => parent.to_path_buf(),
    }
}

fn directory_from_file_lookup(result: rusqlite::Result<String>) -> Result<std::path::PathBuf> {
    match result {
        Ok(file_path) => Ok(parent_directory(&file_path)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(std::path::PathBuf::from(".")),
        Err(err) => Err(err).into_diagnostic(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::impact::analysis::ImpactProvider;
    use crate::impact::packet::{ChangedFile, FileAnalysisStatus};
    use crate::state::migrations::get_migrations;
    use crate::state::storage::StorageManager;
    use rstest::rstest;
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
            "INSERT INTO api_routes (method, path_pattern, handler_symbol_name, handler_file_id, framework, route_source, mount_prefix, is_dynamic, route_confidence, evidence, last_indexed_at)
             VALUES ('GET', '/b', 'handler_b', ?1, 'Axum', 'DECORATOR', NULL, 0, 1.0, 'test', '2026-01-01T00:00:00Z')",
            [file_id_b],
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

        conn.execute(
            "INSERT INTO project_files (file_path, language, content_hash, file_size, last_indexed_at, service_name)
             VALUES ('svc/z.rs', 'Rust', 'hash', 1, '2026-01-01T00:00:00Z', 'svc_z')",
            [],
        )
        .unwrap();
        let file_id_z = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO project_symbols (file_id, qualified_name, symbol_name, symbol_kind, last_indexed_at)
             VALUES (?1, 'crate::handler_z', 'handler_z', 'Function', '2026-01-01T00:00:00Z')",
            [file_id_z],
        )
        .unwrap();
        let sym_z = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO structural_edges (caller_symbol_id, caller_file_id, callee_symbol_id, callee_file_id, call_kind, resolution_status, confidence)
             VALUES (?1, ?2, ?3, ?4, 'DIRECT', 'RESOLVED', 1.0)",
            [sym_a, file_id_a, sym_z, file_id_z],
        )
        .unwrap();

        conn.execute(
            "INSERT INTO project_files (file_path, language, content_hash, file_size, last_indexed_at, service_name)
             VALUES ('svc/c.rs', 'Rust', 'hash', 1, '2026-01-01T00:00:00Z', 'svc_c')",
            [],
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
            deadline: std::time::Instant::now() + std::time::Duration::from_secs(120),
            skip_git_history_enrichment: false,
            history_budget: None,
            overall_deadline: None,
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

        let delta = packet
            .service_map_delta
            .expect("routed services stay on the map");
        assert_eq!(delta.affected_services, vec!["svc_a".to_string()]);
        assert_eq!(
            delta
                .services
                .iter()
                .map(|service| service.name.as_str())
                .collect::<Vec<_>>(),
            vec!["svc_a", "svc_b"]
        );
        assert!(delta.services.iter().all(|service| service.name != "svc_c"));
        assert!(delta.services.iter().all(|service| service.name != "svc_z"));
        assert_eq!(
            delta.cross_service_edges,
            vec![("svc_a".to_string(), "svc_b".to_string(), 1)]
        );
        assert_eq!(delta.services.len(), 2);
        assert_eq!(delta.total_services, delta.services.len());
        let first = serde_json::to_vec(&delta).unwrap();
        let second = serde_json::to_vec(&delta).unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn enrich_leaves_service_map_unset_when_changed_file_has_no_service() {
        let mut conn = Connection::open_in_memory().unwrap();
        get_migrations().to_latest(&mut conn).unwrap();
        conn.execute(
            "INSERT INTO project_files (file_path, language, content_hash, file_size, last_indexed_at, service_name)
             VALUES ('svc/none.rs', 'Rust', 'hash', 1, '2026-01-01T00:00:00Z', NULL)",
            [],
        )
        .unwrap();
        let storage = StorageManager::init_from_conn(conn);
        let mut config = crate::config::model::Config::default();
        config.coverage.enabled = true;
        config.coverage.services.enabled = true;
        let context = EnrichmentContext {
            storage: &storage,
            config: &config,
            file_id_map: HashMap::new(),
            project_root: PathBuf::new(),
            warnings: Arc::new(Mutex::new(Vec::new())),
            deadline: std::time::Instant::now() + std::time::Duration::from_secs(30),
            skip_git_history_enrichment: false,
            history_budget: None,
            overall_deadline: None,
        };
        let mut packet = ImpactPacket {
            changes: vec![ChangedFile {
                path: PathBuf::from("svc/none.rs"),
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
        assert!(packet.service_map_delta.is_none());
    }

    fn enabled_config() -> crate::config::model::Config {
        let mut config = crate::config::model::Config::default();
        config.coverage.enabled = true;
        config.coverage.services.enabled = true;
        config
    }

    fn context_for<'a>(
        storage: &'a StorageManager,
        config: &'a crate::config::model::Config,
    ) -> EnrichmentContext<'a> {
        EnrichmentContext {
            storage,
            config,
            file_id_map: HashMap::new(),
            project_root: PathBuf::new(),
            warnings: Arc::new(Mutex::new(Vec::new())),
            deadline: std::time::Instant::now() + std::time::Duration::from_secs(30),
            skip_git_history_enrichment: false,
            history_budget: None,
            overall_deadline: None,
        }
    }

    fn changed(path: &str, old_path: Option<&str>) -> ChangedFile {
        ChangedFile {
            path: PathBuf::from(path),
            status: "Modified".to_string(),
            old_path: old_path.map(PathBuf::from),
            is_staged: false,
            symbols: None,
            imports: None,
            runtime_usage: None,
            analysis_status: FileAnalysisStatus::default(),
            analysis_warnings: Vec::new(),
            api_routes: Vec::new(),
            data_models: Vec::new(),
            ci_gates: Vec::new(),
        }
    }

    fn insert_file(conn: &Connection, path: &str, service: &str) -> i64 {
        conn.execute(
            "INSERT INTO project_files (file_path, language, content_hash, file_size, last_indexed_at, service_name)
             VALUES (?1, 'Rust', 'hash', 1, '2026-01-01T00:00:00Z', ?2)",
            [path, service],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    fn insert_route(conn: &Connection, file_id: i64, pattern: &str) {
        conn.execute(
            "INSERT INTO api_routes (method, path_pattern, handler_symbol_name, handler_file_id, framework, route_source, mount_prefix, is_dynamic, route_confidence, evidence, last_indexed_at)
             VALUES ('GET', ?1, 'handler', ?2, 'Axum', 'DECORATOR', NULL, 0, 1.0, 'test', '2026-01-01T00:00:00Z')",
            rusqlite::params![pattern, file_id],
        )
        .unwrap();
    }

    fn insert_symbol(conn: &Connection, file_id: i64, qualified: &str, symbol: &str) -> i64 {
        conn.execute(
            "INSERT INTO project_symbols (file_id, qualified_name, symbol_name, symbol_kind, last_indexed_at)
             VALUES (?1, ?2, ?3, 'Function', '2026-01-01T00:00:00Z')",
            rusqlite::params![file_id, qualified, symbol],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    fn insert_edge(
        conn: &Connection,
        caller_symbol: i64,
        caller_file: i64,
        callee_symbol: i64,
        callee_file: i64,
    ) {
        conn.execute(
            "INSERT INTO structural_edges (caller_symbol_id, caller_file_id, callee_symbol_id, callee_file_id, call_kind, resolution_status, confidence)
             VALUES (?1, ?2, ?3, ?4, 'DIRECT', 'RESOLVED', 1.0)",
            rusqlite::params![caller_symbol, caller_file, callee_symbol, callee_file],
        )
        .unwrap();
    }

    fn definition(
        name: &str,
        root: &str,
        owners: &[&str],
    ) -> crate::config::model::ServiceDefinition {
        crate::config::model::ServiceDefinition {
            name: name.to_string(),
            root: root.to_string(),
            owners: owners.iter().map(|owner| (*owner).to_string()).collect(),
            runtime_name: None,
            queues: Vec::new(),
            topics: Vec::new(),
            rpc_endpoints: Vec::new(),
        }
    }

    fn migrated() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        get_migrations().to_latest(&mut conn).unwrap();
        conn
    }

    #[rstest]
    #[case("src")]
    #[case("vendor")]
    #[case("ledgerful")]
    fn service_identity_directory_label_leaves_delta_unset(#[case] label: &str) {
        let conn = migrated();
        insert_file(&conn, "svc/a.rs", label);
        let storage = StorageManager::init_from_conn(conn);
        let config = enabled_config();
        let context = context_for(&storage, &config);
        let mut packet = ImpactPacket {
            changes: vec![changed("svc/a.rs", None)],
            ..Default::default()
        };
        ServiceProvider.enrich(&context, &mut packet).unwrap();
        assert!(
            packet.service_map_delta.is_none(),
            "{label} is a directory label"
        );
    }

    #[test]
    fn service_identity_declared_zero_route_keeps_definition() {
        let conn = migrated();
        insert_file(&conn, "services/beta/a.rs", "web");
        let storage = StorageManager::init_from_conn(conn);
        let mut config = enabled_config();
        config.services.definitions.push(definition(
            "web",
            "services\\web",
            &["owner-b", "owner-a"],
        ));
        let context = context_for(&storage, &config);
        let mut packet = ImpactPacket {
            changes: vec![changed("services/beta/a.rs", None)],
            ..Default::default()
        };
        ServiceProvider.enrich(&context, &mut packet).unwrap();
        let delta = packet.service_map_delta.expect("declared name stays");
        assert_eq!(delta.affected_services, vec!["web".to_string()]);
        assert_eq!(delta.services.len(), 1);
        assert_eq!(delta.services[0].directory, PathBuf::from("services/web"));
        assert_eq!(
            delta.services[0].owners,
            vec!["owner-a".to_string(), "owner-b".to_string()]
        );
    }

    #[test]
    fn service_identity_smallest_root_wins_when_larger_root_is_listed_first() {
        let conn = migrated();
        insert_file(&conn, "services/z.rs", "web");
        let storage = StorageManager::init_from_conn(conn);
        let mut config = enabled_config();
        config
            .services
            .definitions
            .push(definition("web", "services/beta", &["from-beta"]));
        config
            .services
            .definitions
            .push(definition("web", "services/alpha", &["from-alpha"]));
        let context = context_for(&storage, &config);
        let mut packet = ImpactPacket {
            changes: vec![changed("services/z.rs", None)],
            ..Default::default()
        };
        ServiceProvider.enrich(&context, &mut packet).unwrap();
        let delta = packet
            .service_map_delta
            .expect("declared name stays without routes");
        assert_eq!(delta.services[0].directory, PathBuf::from("services/alpha"));
        assert_eq!(delta.services[0].owners, vec!["from-alpha".to_string()]);
    }

    #[test]
    fn service_identity_undeclared_builder_with_routes_is_omitted() {
        let conn = migrated();
        let file_id = insert_file(&conn, "svc/builder.rs", "BUILDER");
        for index in 0..5 {
            insert_route(&conn, file_id, &format!("/b{index}"));
        }
        let storage = StorageManager::init_from_conn(conn);
        let config = enabled_config();
        let context = context_for(&storage, &config);
        let mut packet = ImpactPacket {
            changes: vec![changed("svc/builder.rs", None)],
            ..Default::default()
        };
        ServiceProvider.enrich(&context, &mut packet).unwrap();
        assert!(packet.service_map_delta.is_none());
    }

    #[test]
    fn service_identity_declared_builder_with_zero_routes_stays() {
        let conn = migrated();
        insert_file(&conn, "svc/builder.rs", "BUILDER");
        let storage = StorageManager::init_from_conn(conn);
        let mut config = enabled_config();
        config.services.definitions.push(definition(
            "BUILDER",
            "services/builder",
            &["builder-owner"],
        ));
        let context = context_for(&storage, &config);
        let mut packet = ImpactPacket {
            changes: vec![changed("svc/builder.rs", None)],
            ..Default::default()
        };
        ServiceProvider.enrich(&context, &mut packet).unwrap();
        let delta = packet.service_map_delta.expect("declared BUILDER stays");
        assert_eq!(delta.affected_services, vec!["BUILDER".to_string()]);
        assert_eq!(
            delta.services[0].directory,
            PathBuf::from("services/builder")
        );
        assert_eq!(delta.services[0].owners, vec!["builder-owner".to_string()]);
    }

    #[test]
    fn service_identity_coverage_off_leaves_delta_unset() {
        let conn = migrated();
        let file_id = insert_file(&conn, "svc/a.rs", "web");
        insert_route(&conn, file_id, "/a");
        let storage = StorageManager::init_from_conn(conn);
        let mut config = enabled_config();
        config.coverage.enabled = false;
        let context = context_for(&storage, &config);
        let mut packet = ImpactPacket {
            changes: vec![changed("svc/a.rs", None)],
            ..Default::default()
        };
        ServiceProvider.enrich(&context, &mut packet).unwrap();
        assert!(packet.service_map_delta.is_none());

        let mut services_off = enabled_config();
        services_off.coverage.services.enabled = false;
        let context = context_for(&storage, &services_off);
        let mut packet = ImpactPacket {
            changes: vec![changed("svc/a.rs", None)],
            ..Default::default()
        };
        ServiceProvider.enrich(&context, &mut packet).unwrap();
        assert!(packet.service_map_delta.is_none());
    }

    #[test]
    fn service_identity_old_path_wins_over_path() {
        let conn = migrated();
        let old_id = insert_file(&conn, "svc/old.rs", "web");
        insert_route(&conn, old_id, "/old");
        let new_id = insert_file(&conn, "svc/new.rs", "other");
        insert_route(&conn, new_id, "/new");
        let storage = StorageManager::init_from_conn(conn);
        let config = enabled_config();
        let context = context_for(&storage, &config);
        let mut packet = ImpactPacket {
            changes: vec![changed("svc/new.rs", Some("svc/old.rs"))],
            ..Default::default()
        };
        ServiceProvider.enrich(&context, &mut packet).unwrap();
        let delta = packet
            .service_map_delta
            .expect("old path names the service");
        assert_eq!(delta.affected_services, vec!["web".to_string()]);
        assert!(delta.services.iter().all(|service| service.name != "other"));
    }

    #[test]
    fn service_identity_backslash_change_path_matches_stored_slash_path() {
        let conn = migrated();
        let file_id = insert_file(&conn, "services/web/app.rs", "web");
        insert_route(&conn, file_id, "/app");
        let storage = StorageManager::init_from_conn(conn);
        let config = enabled_config();
        let context = context_for(&storage, &config);
        let mut packet = ImpactPacket {
            changes: vec![changed(r"services\web\app.rs", None)],
            ..Default::default()
        };
        ServiceProvider.enrich(&context, &mut packet).unwrap();
        let delta = packet
            .service_map_delta
            .expect("backslash change path matches the stored slash path");
        assert_eq!(delta.affected_services, vec!["web".to_string()]);
    }

    #[test]
    fn service_identity_routed_edges_order_by_caller_then_callee() {
        let conn = migrated();
        let file_m = insert_file(&conn, "svc/m.rs", "svc_m");
        insert_route(&conn, file_m, "/m");
        let sym_m = insert_symbol(&conn, file_m, "crate::handler_m", "handler_m");

        let file_d = insert_file(&conn, "svc/d.rs", "svc_d");
        insert_route(&conn, file_d, "/d");
        let sym_d = insert_symbol(&conn, file_d, "crate::handler_d", "handler_d");
        // Stored first. Caller `svc_m` sorts after caller `svc_a`.
        insert_edge(&conn, sym_m, file_m, sym_d, file_d);

        let file_a = insert_file(&conn, "svc/a.rs", "svc_a");
        insert_route(&conn, file_a, "/a");
        let sym_a = insert_symbol(&conn, file_a, "crate::handler_a", "handler_a");
        insert_edge(&conn, sym_a, file_a, sym_m, file_m);

        let file_z = insert_file(&conn, "svc/z.rs", "svc_z");
        let sym_z = insert_symbol(&conn, file_z, "crate::handler_z", "handler_z");
        insert_edge(&conn, sym_m, file_m, sym_z, file_z);
        insert_file(&conn, "svc/c.rs", "svc_c");

        let storage = StorageManager::init_from_conn(conn);
        let config = enabled_config();
        let context = context_for(&storage, &config);
        let mut packet = ImpactPacket {
            changes: vec![changed("svc/m.rs", None)],
            ..Default::default()
        };
        ServiceProvider.enrich(&context, &mut packet).unwrap();
        let delta = packet
            .service_map_delta
            .expect("routed neighbors stay on the map");
        assert_eq!(delta.affected_services, vec!["svc_m".to_string()]);
        assert_eq!(
            delta
                .services
                .iter()
                .map(|service| service.name.as_str())
                .collect::<Vec<_>>(),
            vec!["svc_a", "svc_d", "svc_m"]
        );
        assert!(delta.services.iter().all(|service| service.name != "svc_c"));
        assert!(delta.services.iter().all(|service| service.name != "svc_z"));
        assert_eq!(
            delta.cross_service_edges,
            vec![
                ("svc_a".to_string(), "svc_m".to_string(), 1),
                ("svc_m".to_string(), "svc_d".to_string(), 1),
            ]
        );
        assert_eq!(delta.total_services, delta.services.len());
    }

    #[test]
    fn service_identity_directory_label_does_not_raise_cross_service_risk() {
        let conn = migrated();
        let web = insert_file(&conn, "services/web/app.rs", "web");
        insert_route(&conn, web, "/app");
        insert_file(&conn, "src/lib.rs", "src");
        let storage = StorageManager::init_from_conn(conn);
        let config = enabled_config();
        assert_eq!(
            config.coverage.services.cross_service_elevation_threshold,
            2
        );
        let context = context_for(&storage, &config);
        let mut packet = ImpactPacket {
            changes: vec![
                changed("services/web/app.rs", None),
                changed("src/lib.rs", None),
            ],
            ..Default::default()
        };
        ServiceProvider.enrich(&context, &mut packet).unwrap();
        let delta = packet
            .service_map_delta
            .as_ref()
            .expect("the canonical service stays");
        assert_eq!(delta.affected_services, vec!["web".to_string()]);
        assert!(delta.services.iter().all(|service| service.name != "src"));

        let impact = crate::impact::analysis::dependencies::DependencyImpactProvider
            .analyze(&packet, &crate::policy::rules::Rules::default(), &config)
            .unwrap();
        assert!(
            impact
                .reasons
                .iter()
                .all(|reason| !reason.starts_with("Cross-service change affecting")),
            "{:?}",
            impact.reasons
        );
        assert_eq!(impact.weight, 0);
    }

    #[test]
    fn service_identity_root_file_directory_stays_empty() {
        let conn = migrated();
        let file_id = insert_file(&conn, "lib.rs", "top");
        insert_route(&conn, file_id, "/top");
        let storage = StorageManager::init_from_conn(conn);
        let config = enabled_config();
        let context = context_for(&storage, &config);
        let mut packet = ImpactPacket {
            changes: vec![changed("lib.rs", None)],
            ..Default::default()
        };
        ServiceProvider.enrich(&context, &mut packet).unwrap();
        let delta = packet.service_map_delta.expect("routed root file stays");
        assert!(delta.services[0].directory.as_os_str().is_empty());
    }

    #[test]
    fn service_identity_lookup_splits_missing_row_from_other_errors() {
        let missing: rusqlite::Result<Option<String>> = Err(rusqlite::Error::QueryReturnedNoRows);
        assert!(optional_text(missing).unwrap().is_none());
        let broken: rusqlite::Result<Option<String>> = Err(rusqlite::Error::InvalidQuery);
        assert!(optional_text(broken).is_err());

        let missing_dir: rusqlite::Result<String> = Err(rusqlite::Error::QueryReturnedNoRows);
        assert_eq!(
            directory_from_file_lookup(missing_dir).unwrap(),
            PathBuf::from(".")
        );
        assert!(
            directory_from_file_lookup(Ok("lib.rs".to_string()))
                .unwrap()
                .as_os_str()
                .is_empty()
        );
        assert_eq!(
            directory_from_file_lookup(Ok("svc/a.rs".to_string())).unwrap(),
            PathBuf::from("svc")
        );
        let broken_dir: rusqlite::Result<String> = Err(rusqlite::Error::InvalidQuery);
        assert!(directory_from_file_lookup(broken_dir).is_err());
        assert_eq!(parent_directory(r"C:\"), PathBuf::from("."));
    }
}
