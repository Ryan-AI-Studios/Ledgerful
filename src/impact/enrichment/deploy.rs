use crate::coverage::deploy::detect_deploy_manifest_changes;
use crate::impact::enrichment::{EnrichmentContext, EnrichmentProvider};
use crate::impact::packet::ImpactPacket;
use miette::Result;

pub struct DeployProvider;

impl EnrichmentProvider for DeployProvider {
    fn name(&self) -> &'static str {
        "Deployment Enrichment Provider"
    }

    fn enrich(&self, context: &EnrichmentContext, packet: &mut ImpactPacket) -> Result<()> {
        let config = &context.config.coverage;
        if !config.enabled || !config.deploy.enabled {
            return Ok(());
        }

        packet.deploy_manifest_changes = detect_deploy_manifest_changes(
            &packet.changes,
            &config.deploy.patterns,
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
    fn enrich_detects_dockerfile_change() {
        let mut conn = Connection::open_in_memory().unwrap();
        get_migrations().to_latest(&mut conn).unwrap();
        let storage = StorageManager::init_from_conn(conn);
        let mut config = crate::config::model::Config::default();
        config.coverage.enabled = true;
        config.coverage.deploy.enabled = true;
        let context = EnrichmentContext {
            storage: &storage,
            config: &config,
            file_id_map: HashMap::new(),
            project_root: PathBuf::new(),
            warnings: Arc::new(Mutex::new(Vec::new())),
        };
        let mut packet = ImpactPacket {
            changes: vec![ChangedFile {
                path: PathBuf::from("Dockerfile"),
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

        DeployProvider.enrich(&context, &mut packet).unwrap();

        assert_eq!(packet.deploy_manifest_changes.len(), 1);
        assert_eq!(
            packet.deploy_manifest_changes[0].manifest_type,
            crate::impact::packet::ManifestType::Dockerfile
        );
    }

    #[test]
    fn enrich_disabled_when_deploy_coverage_off() {
        let mut conn = Connection::open_in_memory().unwrap();
        get_migrations().to_latest(&mut conn).unwrap();
        let storage = StorageManager::init_from_conn(conn);
        let mut config = crate::config::model::Config::default();
        config.coverage.enabled = true;
        config.coverage.deploy.enabled = false;
        let context = EnrichmentContext {
            storage: &storage,
            config: &config,
            file_id_map: HashMap::new(),
            project_root: PathBuf::new(),
            warnings: Arc::new(Mutex::new(Vec::new())),
        };
        let mut packet = ImpactPacket {
            changes: vec![ChangedFile {
                path: PathBuf::from("Dockerfile"),
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

        DeployProvider.enrich(&context, &mut packet).unwrap();

        assert!(packet.deploy_manifest_changes.is_empty());
    }
}
