use crate::impact::enrichment::{EnrichmentContext, EnrichmentProvider};
use crate::impact::packet::{DataModel, ImpactPacket};
use miette::{IntoDiagnostic, Result};
use tracing::debug;

pub struct DataModelProvider;

impl EnrichmentProvider for DataModelProvider {
    fn name(&self) -> &'static str {
        "Data Model Enrichment Provider"
    }

    fn enrich(&self, context: &EnrichmentContext, packet: &mut ImpactPacket) -> Result<()> {
        if !context.storage.table_exists_and_has_data("data_models")? {
            debug!("Skipping data model enrichment: data_models table is empty or missing.");
            return Ok(());
        }

        let conn = context.storage.get_connection();

        for changed_file in &mut packet.changes {
            let Some(&file_id) = context.file_id_map.get(&changed_file.path) else {
                continue;
            };

            let mut stmt = conn
                .prepare(
                    "SELECT model_name, model_kind, confidence, evidence 
                     FROM data_models WHERE model_file_id = ?1",
                )
                .into_diagnostic()?;

            let models = stmt
                .query_map([file_id], |row| {
                    Ok(DataModel {
                        model_name: row.get(0)?,
                        model_kind: row.get(1)?,
                        confidence: row.get(2)?,
                        evidence: row.get(3)?,
                    })
                })
                .into_diagnostic()?;

            for model in models {
                changed_file.data_models.push(model.into_diagnostic()?);
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
    fn enrich_reads_data_models() {
        let mut conn = Connection::open_in_memory().unwrap();
        get_migrations().to_latest(&mut conn).unwrap();
        conn.execute(
            "INSERT INTO project_files (file_path, language, content_hash, file_size, last_indexed_at)
             VALUES ('src/models.rs', 'Rust', 'hash', 1, '2026-01-01T00:00:00Z')",
            [],
        )
        .unwrap();
        let file_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO data_models (model_name, model_file_id, language, model_kind, confidence, evidence, last_indexed_at)
             VALUES ('User', ?1, 'Rust', 'STRUCT', 1.0, 'test', '2026-01-01T00:00:00Z')",
            [file_id],
        )
        .unwrap();

        let storage = StorageManager::init_from_conn(conn);
        let mut file_id_map = HashMap::new();
        file_id_map.insert(PathBuf::from("src/models.rs"), file_id);
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
                path: PathBuf::from("src/models.rs"),
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

        DataModelProvider.enrich(&context, &mut packet).unwrap();

        assert_eq!(packet.changes[0].data_models.len(), 1);
        assert_eq!(packet.changes[0].data_models[0].model_name, "User");
    }
}
