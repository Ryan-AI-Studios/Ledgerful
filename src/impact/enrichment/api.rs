use crate::impact::enrichment::{EnrichmentContext, EnrichmentProvider};
use crate::impact::packet::{ApiRoute, ImpactPacket};
use miette::{IntoDiagnostic, Result};
use tracing::debug;

pub struct ApiProvider;

impl EnrichmentProvider for ApiProvider {
    fn name(&self) -> &'static str {
        "API Enrichment Provider"
    }

    fn enrich(&self, context: &EnrichmentContext, packet: &mut ImpactPacket) -> Result<()> {
        if !context.storage.table_exists_and_has_data("api_routes")? {
            debug!("Skipping API enrichment: api_routes table is empty or missing.");
            return Ok(());
        }

        let conn = context.storage.get_connection();

        for changed_file in &mut packet.changes {
            let Some(&file_id) = context.file_id_map.get(&changed_file.path) else {
                continue;
            };

            let mut stmt = conn
                .prepare(
                    "SELECT method, path_pattern, handler_symbol_name, framework, route_source, mount_prefix, is_dynamic, route_confidence, evidence, \
                     auth_requirements, schema_refs, owning_service, consumers \
                     FROM api_routes WHERE handler_file_id = ?1"
                )
                .into_diagnostic()?;

            let routes = stmt
                .query_map([file_id], |row| {
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
                .into_diagnostic()?;

            for route in routes {
                let route = route.into_diagnostic()?;

                // Add impact reasons for API changes
                if changed_file.status == "Deleted" {
                    packet.risk_reasons.push(format!(
                        "Public API removal: {} {}",
                        route.method, route.path_pattern
                    ));
                }

                changed_file.api_routes.push(route);
            }
        }

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
    fn enrich_reads_api_routes() {
        let mut conn = Connection::open_in_memory().unwrap();
        get_migrations().to_latest(&mut conn).unwrap();
        conn.execute(
            "INSERT INTO project_files (file_path, language, content_hash, file_size, last_indexed_at)
             VALUES ('src/routes.rs', 'Rust', 'hash', 1, '2026-01-01T00:00:00Z')",
            [],
        )
        .unwrap();
        let file_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO api_routes (method, path_pattern, handler_symbol_name, handler_file_id, framework, route_source, mount_prefix, is_dynamic, route_confidence, evidence, last_indexed_at)
             VALUES ('GET', '/api/users', 'get_users', ?1, 'Axum', 'DECORATOR', NULL, 0, 1.0, 'test', '2026-01-01T00:00:00Z')",
            [file_id],
        )
        .unwrap();

        let storage = StorageManager::init_from_conn(conn);
        let mut file_id_map = HashMap::new();
        file_id_map.insert(PathBuf::from("src/routes.rs"), file_id);
        let config = crate::config::model::Config::default();
        let context = EnrichmentContext {
            storage: &storage,
            config: &config,
            file_id_map,
            project_root: PathBuf::new(),
            warnings: Arc::new(Mutex::new(Vec::new())),
        };
        let mut packet = ImpactPacket {
            changes: vec![ChangedFile {
                path: PathBuf::from("src/routes.rs"),
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

        ApiProvider.enrich(&context, &mut packet).unwrap();

        assert_eq!(packet.changes[0].api_routes.len(), 1);
        assert_eq!(packet.changes[0].api_routes[0].method, "GET");
        assert_eq!(packet.changes[0].api_routes[0].path_pattern, "/api/users");
    }
}
