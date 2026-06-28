use crate::impact::enrichment::{EnrichmentContext, EnrichmentProvider};
use crate::impact::packet::{CIGate, ImpactPacket};
use miette::{IntoDiagnostic, Result};
use tracing::debug;

pub struct CIGateProvider;

impl EnrichmentProvider for CIGateProvider {
    fn name(&self) -> &'static str {
        "CI Gate Enrichment Provider"
    }

    fn enrich(&self, context: &EnrichmentContext, packet: &mut ImpactPacket) -> Result<()> {
        if !context.storage.table_exists_and_has_data("ci_gates")? {
            debug!("Skipping CI gate enrichment: ci_gates table is empty or missing.");
            return Ok(());
        }

        let conn = context.storage.get_connection();

        for changed_file in &mut packet.changes {
            let Some(&file_id) = context.file_id_map.get(&changed_file.path) else {
                continue;
            };

            let mut stmt = conn
                .prepare(
                    "SELECT platform, job_name, trigger, workflow_name, environment, artifacts, release_gates \
                     FROM ci_gates WHERE ci_file_id = ?1",
                )
                .into_diagnostic()?;

            let gates = stmt
                .query_map([file_id], |row| {
                    let artifacts_json: Option<String> = row.get(5)?;
                    let artifacts = artifacts_json
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default();

                    let release_gates_json: Option<String> = row.get(6)?;
                    let release_gates = release_gates_json
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default();

                    Ok(CIGate {
                        platform: row.get(0)?,
                        job_name: row.get(1)?,
                        trigger: row.get(2)?,
                        workflow_name: row.get(3)?,
                        environment: row.get(4)?,
                        artifacts,
                        release_gates,
                    })
                })
                .into_diagnostic()?;

            for gate in gates {
                changed_file.ci_gates.push(gate.into_diagnostic()?);
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
    fn enrich_reads_current_ci_gates_schema() {
        let mut conn = Connection::open_in_memory().unwrap();
        get_migrations().to_latest(&mut conn).unwrap();
        conn.execute(
            "INSERT INTO project_files (file_path, language, content_hash, file_size, last_indexed_at)
             VALUES ('src/lib.rs', 'Rust', 'hash', 1, '2026-01-01T00:00:00Z')",
            [],
        )
        .unwrap();
        let file_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO ci_gates (ci_file_id, platform, job_name, trigger, steps, last_indexed_at)
             VALUES (?1, 'github_actions', 'test', 'pull_request', 'cargo test', '2026-01-01T00:00:00Z')",
            [file_id],
        )
        .unwrap();

        let storage = StorageManager::init_from_conn(conn);
        let mut file_id_map = HashMap::new();
        file_id_map.insert(PathBuf::from("src/lib.rs"), file_id);
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
                path: PathBuf::from("src/lib.rs"),
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

        CIGateProvider.enrich(&context, &mut packet).unwrap();

        assert_eq!(packet.changes[0].ci_gates.len(), 1);
        assert_eq!(packet.changes[0].ci_gates[0].job_name, "test");
    }
}
