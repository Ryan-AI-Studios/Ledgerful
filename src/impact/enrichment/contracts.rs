use crate::contracts::matcher::match_changed_files;
use crate::impact::enrichment::{EnrichmentContext, EnrichmentProvider};
use crate::impact::packet::ImpactPacket;
use miette::Result;
use tracing::warn;

pub struct ContractProvider;

impl EnrichmentProvider for ContractProvider {
    fn name(&self) -> &'static str {
        "Contract Matching Enrichment Provider"
    }

    fn enrich(&self, context: &EnrichmentContext, packet: &mut ImpactPacket) -> Result<()> {
        let config = context.config;

        // Skip if no contract spec paths configured
        if config.contracts.spec_paths.is_empty() {
            return Ok(());
        }

        // Skip if no embedding model configured
        if config.local_model.embedding_model.is_empty() {
            return Ok(());
        }

        let conn = context.storage.get_connection();

        // Collect changed file paths
        let file_paths: Vec<String> = packet
            .changes
            .iter()
            .map(|c| c.path.to_string_lossy().to_string())
            .collect();

        if file_paths.is_empty() {
            return Ok(());
        }

        match match_changed_files(&config.contracts, conn, &config.local_model, &file_paths) {
            Ok(matches) => {
                packet.affected_contracts = matches;
            }
            Err(e) => {
                warn!("Contract matching failed: {e}");
                context.add_warning(format!("Contract enrichment failed: {e}"));
            }
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
    fn enrich_skips_when_no_spec_paths() {
        let mut conn = Connection::open_in_memory().unwrap();
        get_migrations().to_latest(&mut conn).unwrap();
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
        ContractProvider.enrich(&context, &mut packet).unwrap();
        assert!(packet.affected_contracts.is_empty());
    }

    #[test]
    fn enrich_skips_when_no_embedding_model() {
        let mut conn = Connection::open_in_memory().unwrap();
        get_migrations().to_latest(&mut conn).unwrap();
        let storage = StorageManager::init_from_conn(conn);
        let mut config = crate::config::model::Config::default();
        config.contracts.spec_paths = vec!["openapi.yaml".to_string()];
        let context = EnrichmentContext {
            storage: &storage,
            config: &config,
            file_id_map: HashMap::new(),
            project_root: PathBuf::new(),
            warnings: Arc::new(Mutex::new(Vec::new())),
        };
        let mut packet = ImpactPacket::default();
        ContractProvider.enrich(&context, &mut packet).unwrap();
        assert!(packet.affected_contracts.is_empty());
    }

    #[test]
    fn enrich_returns_empty_when_no_api_endpoints() {
        let mut conn = Connection::open_in_memory().unwrap();
        get_migrations().to_latest(&mut conn).unwrap();
        let storage = StorageManager::init_from_conn(conn);
        let mut config = crate::config::model::Config::default();
        config.contracts.spec_paths = vec!["openapi.yaml".to_string()];
        config.local_model.embedding_model = "nomic-embed-text".to_string();
        let context = EnrichmentContext {
            storage: &storage,
            config: &config,
            file_id_map: HashMap::new(),
            project_root: PathBuf::new(),
            warnings: Arc::new(Mutex::new(Vec::new())),
        };
        let mut packet = ImpactPacket::default();
        ContractProvider.enrich(&context, &mut packet).unwrap();
        assert!(packet.affected_contracts.is_empty());
    }
}
