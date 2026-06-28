use crate::impact::enrichment::{EnrichmentContext, EnrichmentProvider};
use crate::impact::packet::ImpactPacket;
use crate::index::env_schema::EnvVarDep;
use miette::{IntoDiagnostic, Result};
use std::collections::HashSet;
use tracing::debug;

pub struct EnvironmentProvider;

impl EnrichmentProvider for EnvironmentProvider {
    fn name(&self) -> &'static str {
        "Environment Enrichment Provider"
    }

    fn enrich(&self, context: &EnrichmentContext, packet: &mut ImpactPacket) -> Result<()> {
        if !context
            .storage
            .table_exists_and_has_data("env_references")?
        {
            debug!("Skipping environment enrichment: env_references table is empty or missing.");
            return Ok(());
        }

        let conn = context.storage.get_connection();

        // 1. Collect all declared var names
        let mut decl_stmt = conn
            .prepare("SELECT var_name FROM env_declarations")
            .into_diagnostic()?;

        let declared_names: HashSet<String> = decl_stmt
            .query_map([], |row| row.get::<_, String>(0))
            .into_diagnostic()?
            .collect::<rusqlite::Result<HashSet<_>>>()
            .into_diagnostic()?;

        // 2. For each changed file, find env var references and check if declared
        for change in &packet.changes {
            let Some(&file_id) = context.file_id_map.get(&change.path) else {
                continue;
            };

            let mut ref_stmt = conn
                .prepare("SELECT var_name, reference_kind FROM env_references WHERE file_id = ?1")
                .into_diagnostic()?;

            let refs = ref_stmt
                .query_map([file_id], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })
                .into_diagnostic()?;

            for res in refs {
                let (var_name, reference_kind) = res.into_diagnostic()?;

                if var_name == "*" {
                    continue;
                }

                if !declared_names.contains(&var_name) {
                    packet.env_var_deps.push(EnvVarDep {
                        var_name: var_name.clone(),
                        declared: false,
                        evidence: format!(
                            "Referenced as {} in {} but not declared in any env config",
                            reference_kind,
                            change.path.display()
                        ),
                    });
                }
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
    fn enrich_detects_undeclared_env_var() {
        let mut conn = Connection::open_in_memory().unwrap();
        get_migrations().to_latest(&mut conn).unwrap();
        conn.execute(
            "INSERT INTO project_files (file_path, language, content_hash, file_size, last_indexed_at)
             VALUES ('src/main.rs', 'Rust', 'hash', 1, '2026-01-01T00:00:00Z')",
            [],
        )
        .unwrap();
        let file_id = conn.last_insert_rowid();

        conn.execute(
            "INSERT INTO env_references (file_id, var_name, reference_kind, last_indexed_at)
             VALUES (?1, 'UNDECLARED_VAR', 'READ', '2026-01-01T00:00:00Z')",
            [file_id],
        )
        .unwrap();

        let storage = StorageManager::init_from_conn(conn);
        let mut file_id_map = HashMap::new();
        file_id_map.insert(PathBuf::from("src/main.rs"), file_id);
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
                path: PathBuf::from("src/main.rs"),
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

        EnvironmentProvider.enrich(&context, &mut packet).unwrap();

        assert_eq!(packet.env_var_deps.len(), 1);
        assert_eq!(packet.env_var_deps[0].var_name, "UNDECLARED_VAR");
        assert!(!packet.env_var_deps[0].declared);
    }
}
