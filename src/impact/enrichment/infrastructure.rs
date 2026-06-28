use crate::impact::enrichment::{EnrichmentContext, EnrichmentProvider};
use crate::impact::packet::ImpactPacket;
use miette::{IntoDiagnostic, Result};
use tracing::debug;

pub struct InfrastructureProvider;

impl EnrichmentProvider for InfrastructureProvider {
    fn name(&self) -> &'static str {
        "Infrastructure Enrichment Provider"
    }

    fn enrich(&self, context: &EnrichmentContext, packet: &mut ImpactPacket) -> Result<()> {
        if !context
            .storage
            .table_exists_and_has_data("project_topology")?
        {
            debug!(
                "Skipping infrastructure enrichment: project_topology table is empty or missing."
            );
            return Ok(());
        }

        let conn = context.storage.get_connection();

        let mut stmt = conn
            .prepare("SELECT dir_path FROM project_topology WHERE role = 'INFRASTRUCTURE'")
            .into_diagnostic()?;

        let dirs = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .into_diagnostic()?;

        for dir in dirs {
            packet.infrastructure_dirs.push(dir.into_diagnostic()?);
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::migrations::get_migrations;
    use crate::state::storage::StorageManager;
    use rusqlite::Connection;
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    #[test]
    fn enrich_reads_current_topology_schema() {
        let mut conn = Connection::open_in_memory().unwrap();
        get_migrations().to_latest(&mut conn).unwrap();
        conn.execute(
            "INSERT INTO project_topology (dir_path, role, confidence, evidence, last_indexed_at)
             VALUES ('.github/workflows', 'INFRASTRUCTURE', 1.0, 'ci', '2026-01-01T00:00:00Z')",
            [],
        )
        .unwrap();

        let storage = StorageManager::init_from_conn(conn);
        let config = crate::config::model::Config::default();
        let context = EnrichmentContext {
            storage: &storage,
            config: &config,
            file_id_map: HashMap::new(),
            project_root: PathBuf::new(),
            warnings: Arc::new(Mutex::new(Vec::new())),
        };
        let mut packet = ImpactPacket::default();

        InfrastructureProvider
            .enrich(&context, &mut packet)
            .unwrap();

        assert_eq!(packet.infrastructure_dirs, vec![".github/workflows"]);
    }
}
