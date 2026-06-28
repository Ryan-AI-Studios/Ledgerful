use super::ProjectIndexer;
use crate::index::entrypoint::{
    EntrypointKind, EntrypointStats, detect_python_entrypoints, detect_rust_entrypoints,
    detect_typescript_entrypoints,
};
use crate::index::topology::{DirectoryRole, TopologyIndexStats, classify_directory};
use miette::{IntoDiagnostic, Result};
use std::collections::{HashMap, HashSet};

pub fn index_topology(indexer: &mut ProjectIndexer) -> Result<TopologyIndexStats> {
    let all_files = super::discovery::discover_files(indexer)?;
    let now = chrono::Utc::now().to_rfc3339();
    let mut dir_files: HashMap<String, Vec<String>> = HashMap::new();
    for file_path in &all_files {
        let relative = file_path
            .strip_prefix(&indexer.repo_path)
            .unwrap_or(file_path);
        if let Some(parent) = relative.parent() {
            let dir = parent.to_string().replace('\\', "/");
            if !dir.is_empty() {
                dir_files.entry(dir).or_default().push(relative.to_string());
            }
        }
    }
    let mut all_dirs: HashSet<String> = dir_files.keys().cloned().collect();
    for dir in dir_files.keys() {
        let mut current = dir.as_str();
        while let Some(parent) = std::path::Path::new(current)
            .parent()
            .and_then(|p| p.to_str())
        {
            if !parent.is_empty() && !all_dirs.contains(parent) {
                all_dirs.insert(parent.to_string());
            }
            current = parent;
        }
    }
    let mut directories_classified = 0usize;
    let mut unclassified = 0usize;
    let mut role_counts: HashMap<DirectoryRole, usize> = HashMap::new();
    let conn = indexer.storage.get_connection_mut();
    for dir_path in &all_dirs {
        let files: Vec<&str> = dir_files
            .get(dir_path)
            .map(|v| v.iter().map(|s| s.as_str()).collect())
            .unwrap_or_default();
        if let Some(classification) = classify_directory(dir_path, &files) {
            conn.execute("INSERT OR REPLACE INTO project_topology (dir_path, role, confidence, evidence, last_indexed_at) VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![dir_path, classification.role.as_str(), classification.confidence, classification.evidence, now]).into_diagnostic()?;
            *role_counts.entry(classification.role).or_insert(0) += 1;
            directories_classified += 1;
        } else {
            unclassified += 1;
        }
    }

    // K4: Run marker-based boundary detection and persist to CozoDB
    index_service_boundaries(indexer)?;

    Ok(TopologyIndexStats {
        directories_classified,
        unclassified,
        role_counts,
    })
}

/// K4: Scan the repository for manifest-based service boundaries and store them in
/// the `service_roots` CozoDB relation. Also scans source files for HTTP client
/// calls and stores inter-service communication edges in `service_dependencies`.
pub fn index_service_boundaries(indexer: &mut ProjectIndexer) -> Result<()> {
    use crate::coverage::services::{BoundaryDetector, detect_http_client_calls};
    use cozo::{DataValue, ScriptMutability};
    use std::collections::BTreeMap;

    let cozo = match indexer.storage.cozo.as_ref() {
        Some(c) => c,
        None => return Ok(()), // CozoDB not available — skip silently
    };

    let now = chrono::Utc::now().to_rfc3339();
    let root = indexer.repo_path.as_std_path();

    // 1. Detect service boundaries via manifest markers
    let detector = BoundaryDetector::new(root);
    let boundaries = detector.detect();

    // Clear previous service_roots for a clean re-index
    let _ = cozo.run_script("?[name, dir_path, marker_kind, confidence, last_indexed_at] := *service_roots{name, dir_path, marker_kind, confidence, last_indexed_at} :rm service_roots {name}");

    for boundary in &boundaries {
        let dir_str = boundary
            .dir_path
            .strip_prefix(root)
            .unwrap_or(&boundary.dir_path)
            .to_string_lossy()
            .replace('\\', "/");

        let mut params: BTreeMap<String, DataValue> = BTreeMap::new();
        params.insert(
            "name".to_string(),
            DataValue::Str(boundary.name.clone().into()),
        );
        params.insert(
            "dir_path".to_string(),
            DataValue::Str(dir_str.clone().into()),
        );
        params.insert(
            "marker_kind".to_string(),
            DataValue::Str(boundary.marker.as_str().into()),
        );
        params.insert(
            "confidence".to_string(),
            DataValue::from(boundary.confidence),
        );
        params.insert("ts".to_string(), DataValue::Str(now.clone().into()));

        let script = "?[name, dir_path, marker_kind, confidence, last_indexed_at] <- [[$name, $dir_path, $marker_kind, $confidence, $ts]] :put service_roots";
        cozo.run_script_with_params(script, params, ScriptMutability::Mutable)
            .unwrap();
    }

    tracing::debug!("K4: Indexed {} service boundaries", boundaries.len());

    // 2. Scan source files for HTTP client call patterns
    let source_extensions = ["rs", "ts", "tsx", "js", "jsx", "py"];
    let all_files = super::discovery::discover_files(indexer)?;

    // Build a map from dir-prefix → service name for resolution
    let service_map: Vec<(String, String)> = boundaries
        .iter()
        .map(|b| {
            let dir_str = b
                .dir_path
                .strip_prefix(root)
                .unwrap_or(&b.dir_path)
                .to_string_lossy()
                .replace('\\', "/");
            (dir_str, b.name.clone())
        })
        .collect();

    let cozo = match indexer.storage.cozo.as_ref() {
        Some(c) => c,
        None => return Ok(()),
    };

    // Clear previous service_dependencies
    let _ = cozo.run_script("?[caller_service, callee_service, pattern, call_kind, confidence, last_indexed_at] := *service_dependencies{caller_service, callee_service, pattern, call_kind, confidence, last_indexed_at} :rm service_dependencies {caller_service, callee_service}");

    let mut dep_edges: std::collections::HashSet<(String, String, String)> =
        std::collections::HashSet::new();

    for file_path in &all_files {
        let ext = file_path.as_str().rsplit('.').next().unwrap_or("");
        if !source_extensions.contains(&ext) {
            continue;
        }

        let relative = file_path
            .strip_prefix(&indexer.repo_path)
            .unwrap_or(file_path)
            .to_string()
            .replace('\\', "/");

        let caller_service = resolve_service_for_path(&relative, &service_map);

        let content = match crate::util::fs::read_to_string_with_encoding(file_path.as_std_path()) {
            Ok(c) => c,
            Err(_) => continue,
        };

        let calls = detect_http_client_calls(&relative, &content);
        for call in calls {
            if let Some(ref caller) = caller_service {
                let edge_key = (
                    caller.clone(),
                    call.call_kind.clone(),
                    call.target_pattern.clone(),
                );
                if dep_edges.insert(edge_key) {
                    let mut params: BTreeMap<String, DataValue> = BTreeMap::new();
                    params.insert(
                        "caller_service".to_string(),
                        DataValue::Str(caller.clone().into()),
                    );
                    // Resolve callee_service by trying to match the target pattern against known routes
                    let mut callee_service = call.call_kind.clone(); // fallback
                    let conn = indexer.storage.get_connection();
                    if let Ok(mut stmt) =
                        conn.prepare("SELECT handler_file_id, path_pattern FROM api_routes")
                    {
                        #[allow(clippy::collapsible_if)]
                        if let Ok(mut rows) = stmt.query([]) {
                            while let Ok(Some(row)) = rows.next() {
                                let handler_file_id: i64 = row.get(0).unwrap_or(0);
                                let path_pattern: String = row.get(1).unwrap_or_default();
                                if !path_pattern.is_empty()
                                    && call.target_pattern.contains(&path_pattern)
                                {
                                    if let Ok(file_path) = conn.query_row(
                                        "SELECT file_path FROM project_files WHERE id = ?1",
                                        [handler_file_id],
                                        |r| r.get::<_, String>(0),
                                    ) {
                                        if let Some(resolved) =
                                            resolve_service_for_path(&file_path, &service_map)
                                        {
                                            callee_service = resolved;
                                            break;
                                        }
                                    }
                                }
                            }
                        }
                    }

                    params.insert(
                        "callee_service".to_string(),
                        DataValue::Str(callee_service.into()),
                    );
                    params.insert(
                        "pattern".to_string(),
                        DataValue::Str(call.target_pattern.into()),
                    );
                    params.insert(
                        "call_kind".to_string(),
                        DataValue::Str(call.call_kind.into()),
                    );
                    params.insert("confidence".to_string(), DataValue::from(call.confidence));
                    params.insert("ts".to_string(), DataValue::Str(now.clone().into()));

                    let script = "?[caller_service, callee_service, pattern, call_kind, confidence, last_indexed_at] <- [[$caller_service, $callee_service, $pattern, $call_kind, $confidence, $ts]] :put service_dependencies";
                    cozo.run_script_with_params(script, params, ScriptMutability::Mutable)
                        .unwrap();
                }
            }
        }
    }

    Ok(())
}

/// Resolve the service name for a source file path using the service map.
fn resolve_service_for_path(file_path: &str, service_map: &[(String, String)]) -> Option<String> {
    // Find the longest matching prefix (deepest service root wins)
    let mut best: Option<(&str, &str)> = None;
    for (dir, name) in service_map {
        #[allow(clippy::collapsible_if)]
        if file_path.starts_with(dir.as_str()) || (dir.is_empty() || dir == ".") {
            if best.is_none() || dir.len() > best.unwrap().0.len() {
                best = Some((dir.as_str(), name.as_str()));
            }
        }
    }
    best.map(|(_, n)| n.to_string())
}

pub fn classify_entrypoints(indexer: &mut ProjectIndexer) -> Result<EntrypointStats> {
    let conn = indexer.storage.get_connection();
    let mut stmt = conn.prepare("SELECT id, file_id, symbol_name, symbol_kind, is_public, metadata FROM project_symbols ORDER BY file_id").into_diagnostic()?;
    let rows: Vec<(i64, i64, String, String, bool, Option<String>)> = stmt
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get::<_, i32>(4)? != 0,
                row.get(5)?,
            ))
        })
        .into_diagnostic()?
        .collect::<Result<Vec<_>, _>>()
        .into_diagnostic()?;
    drop(stmt);

    #[allow(clippy::type_complexity)]
    let mut file_symbols: HashMap<i64, Vec<(i64, String, String, bool, Option<String>)>> =
        HashMap::new();
    for (id, file_id, name, kind, is_public, metadata) in &rows {
        file_symbols.entry(*file_id).or_default().push((
            *id,
            name.clone(),
            kind.clone(),
            *is_public,
            metadata.clone(),
        ));
    }

    let mut path_stmt = conn
        .prepare("SELECT id, file_path, language FROM project_files")
        .into_diagnostic()?;
    let path_rows: Vec<(i64, String, Option<String>)> = path_stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .into_diagnostic()?
        .collect::<Result<Vec<_>, _>>()
        .into_diagnostic()?;
    drop(path_stmt);

    let mut file_paths: HashMap<i64, String> = HashMap::new();
    for (id, path, _lang) in &path_rows {
        file_paths.insert(*id, path.clone());
    }

    let mut stats = EntrypointStats::default();
    let now = chrono::Utc::now().to_rfc3339();

    for (file_id, symbols) in &file_symbols {
        let file_path = match file_paths.get(file_id) {
            Some(p) => p.clone(),
            None => continue,
        };
        let file_lang = path_rows
            .iter()
            .find(|(id, _, _)| id == file_id)
            .and_then(|(_, _, lang)| lang.clone());
        let full_path = indexer.repo_path.join(&file_path);
        let Ok(content) = crate::util::fs::read_to_string_with_encoding(full_path.as_std_path())
        else {
            continue;
        };

        let sym_vec: Vec<crate::index::symbols::Symbol> = symbols
            .iter()
            .map(
                |(_, name, kind, is_public, metadata)| crate::index::symbols::Symbol {
                    name: name.clone(),
                    kind: crate::index::symbols::SymbolKind::parse(kind)
                        .unwrap_or(crate::index::symbols::SymbolKind::Function),
                    is_public: *is_public,
                    cognitive_complexity: None,
                    cyclomatic_complexity: None,
                    line_start: None,
                    line_end: None,
                    qualified_name: None,
                    byte_start: None,
                    byte_end: None,
                    entrypoint_kind: None,
                    metadata: metadata
                        .as_ref()
                        .and_then(|m| serde_json::from_str(m).ok())
                        .unwrap_or_default(),
                },
            )
            .collect();

        let classifications = match file_lang.as_deref() {
            Some("Rust") => detect_rust_entrypoints(&content, &sym_vec),
            Some("TypeScript") | Some("JavaScript") => {
                detect_typescript_entrypoints(&content, &sym_vec, &file_path)
            }
            Some("Python") => detect_python_entrypoints(&content, &sym_vec, &file_path),
            _ => continue,
        };

        let conn_mut = indexer.storage.get_connection_mut();
        for class in &classifications {
            let db_id = symbols
                .iter()
                .find(|(_, name, _, _, _)| name == &class.symbol_name)
                .map(|(id, _, _, _, _)| *id);
            if let Some(id) = db_id {
                conn_mut.execute("UPDATE project_symbols SET entrypoint_kind = ?1, confidence = ?2, evidence = ?3, last_indexed_at = ?4 WHERE id = ?5",
                    rusqlite::params![class.kind.as_str(), class.confidence, class.evidence, now, id]).into_diagnostic()?;
                match class.kind {
                    EntrypointKind::Entrypoint => stats.entrypoints += 1,
                    EntrypointKind::Handler => stats.handlers += 1,
                    EntrypointKind::PublicApi => stats.public_apis += 1,
                    EntrypointKind::Test => stats.tests += 1,
                    EntrypointKind::Ffi => stats.ffi += 1,
                    EntrypointKind::Macro => stats.macros += 1,
                    EntrypointKind::Internal => stats.internal += 1,
                }
            }
        }
    }
    Ok(stats)
}

pub fn infer_services(indexer: &mut ProjectIndexer) -> Result<super::ServiceIndexStats> {
    use crate::coverage::services::{DataModelSource, DirectoryTopology, infer_services};
    use crate::impact::packet::{ApiRoute, DataModel};
    use crate::index::call_graph::CallGraph;
    let (routes, data_models, call_graph) = {
        let conn = indexer.storage.get_connection();
        let mut route_stmt = conn.prepare("SELECT method, path_pattern, handler_symbol_name, framework, route_source, mount_prefix, is_dynamic, route_confidence, evidence, \
                                               auth_requirements, schema_refs, owning_service, consumers FROM api_routes").into_diagnostic()?;
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
            .collect::<Result<Vec<_>, _>>()
            .into_diagnostic()?;
        let mut dm_stmt = conn.prepare("SELECT dm.model_name, dm.model_kind, dm.confidence, dm.evidence, pf.file_path FROM data_models dm JOIN project_files pf ON dm.model_file_id = pf.id").into_diagnostic()?;
        let data_models: Vec<DataModelSource> = dm_stmt
            .query_map([], |row| {
                Ok(DataModelSource {
                    model: DataModel {
                        model_name: row.get(0)?,
                        model_kind: row.get(1)?,
                        confidence: row.get(2)?,
                        evidence: row.get(3)?,
                    },
                    source_path: row.get(4)?,
                })
            })
            .into_diagnostic()?
            .collect::<Result<Vec<_>, _>>()
            .into_diagnostic()?;
        let call_graph = CallGraph {
            edges: super::extraction::get_all_call_edges(indexer)?,
        };
        (routes, data_models, call_graph)
    };

    let topology = DirectoryTopology {
        classifications: indexer
            .storage
            .get_directory_classifications()
            .unwrap_or_default(),
    };
    let services = infer_services(
        &routes,
        &data_models,
        &call_graph,
        &topology,
        &indexer.config.services.definitions,
    );

    let mut files_assigned = 0;
    let conn_mut = indexer.storage.get_connection_mut();
    let tx = conn_mut.unchecked_transaction().into_diagnostic()?;
    tx.execute("UPDATE project_files SET service_name = NULL", [])
        .into_diagnostic()?;
    let mut sorted_services = services.clone();
    sorted_services.sort_by(|a, b| {
        b.directory
            .components()
            .count()
            .cmp(&a.directory.components().count())
    });
    for service in &sorted_services {
        let dir_str = service.directory.to_string_lossy().replace('\\', "/");
        let affected = if dir_str.is_empty() || dir_str == "." {
            tx.execute("UPDATE project_files SET service_name = ?1 WHERE file_path NOT LIKE '%/%' AND service_name IS NULL", rusqlite::params![service.name])
        } else {
            let pattern = format!("{}/%", dir_str);
            tx.execute("UPDATE project_files SET service_name = ?1 WHERE (file_path LIKE ?2 OR file_path = ?3) AND service_name IS NULL", rusqlite::params![service.name, pattern, dir_str])
        }.into_diagnostic()?;
        files_assigned += affected;
    }
    tx.commit().into_diagnostic()?;
    Ok(super::ServiceIndexStats {
        services_inferred: services.len(),
        files_assigned,
    })
}
