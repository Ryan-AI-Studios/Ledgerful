use crate::impact::enrichment::{EnrichmentContext, EnrichmentProvider};
use crate::impact::packet::ImpactPacket;
use miette::Result;

pub struct CISelfAwarenessProvider;

impl EnrichmentProvider for CISelfAwarenessProvider {
    fn name(&self) -> &'static str {
        "CI Self-Awareness Enrichment Provider"
    }

    fn enrich(&self, context: &EnrichmentContext, packet: &mut ImpactPacket) -> Result<()> {
        let config = &context.config.coverage;
        if !config.enabled || !config.ci_self_awareness.enabled {
            return Ok(());
        }

        packet.ci_config_change = crate::index::ci_gates::is_ci_config_changed(&packet.changes);

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

    fn make_storage() -> StorageManager {
        let mut conn = Connection::open_in_memory().unwrap();
        get_migrations().to_latest(&mut conn).unwrap();
        StorageManager::init_from_conn(conn)
    }

    #[test]
    fn enrich_populates_ci_config_change() {
        let config = {
            let mut c = crate::config::model::Config::default();
            c.coverage.enabled = true;
            c.coverage.ci_self_awareness.enabled = true;
            c
        };
        let storage = make_storage();
        let context = EnrichmentContext {
            storage: &storage,
            config: &config,
            file_id_map: HashMap::new(),
            project_root: PathBuf::new(),
            warnings: Arc::new(Mutex::new(Vec::new())),
        };
        let mut packet = ImpactPacket {
            changes: vec![ChangedFile {
                path: PathBuf::from(".github/workflows/ci.yml"),
                status: "Modified".to_string(),
                old_path: None,
                is_staged: true,
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

        CISelfAwarenessProvider
            .enrich(&context, &mut packet)
            .unwrap();

        assert!(packet.ci_config_change.is_some());
        let change = packet.ci_config_change.unwrap();
        assert_eq!(change.known_ci_files, vec![".github/workflows/ci.yml"]);
    }

    #[test]
    fn enrich_disabled_when_coverage_off() {
        let config = {
            let mut c = crate::config::model::Config::default();
            c.coverage.enabled = false;
            c.coverage.ci_self_awareness.enabled = true;
            c
        };
        let storage = make_storage();
        let context = EnrichmentContext {
            storage: &storage,
            config: &config,
            file_id_map: HashMap::new(),
            project_root: PathBuf::new(),
            warnings: Arc::new(Mutex::new(Vec::new())),
        };
        let mut packet = ImpactPacket {
            changes: vec![ChangedFile {
                path: PathBuf::from(".github/workflows/ci.yml"),
                status: "Modified".to_string(),
                old_path: None,
                is_staged: true,
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

        CISelfAwarenessProvider
            .enrich(&context, &mut packet)
            .unwrap();

        assert!(packet.ci_config_change.is_none());
    }

    #[test]
    fn enrich_disabled_when_dimension_off() {
        let config = {
            let mut c = crate::config::model::Config::default();
            c.coverage.enabled = true;
            c.coverage.ci_self_awareness.enabled = false;
            c
        };
        let storage = make_storage();
        let context = EnrichmentContext {
            storage: &storage,
            config: &config,
            file_id_map: HashMap::new(),
            project_root: PathBuf::new(),
            warnings: Arc::new(Mutex::new(Vec::new())),
        };
        let mut packet = ImpactPacket {
            changes: vec![ChangedFile {
                path: PathBuf::from(".github/workflows/ci.yml"),
                status: "Modified".to_string(),
                old_path: None,
                is_staged: true,
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

        CISelfAwarenessProvider
            .enrich(&context, &mut packet)
            .unwrap();

        assert!(packet.ci_config_change.is_none());
    }

    #[test]
    fn enrich_empty_when_no_ci_files() {
        let config = {
            let mut c = crate::config::model::Config::default();
            c.coverage.enabled = true;
            c.coverage.ci_self_awareness.enabled = true;
            c
        };
        let storage = make_storage();
        let context = EnrichmentContext {
            storage: &storage,
            config: &config,
            file_id_map: HashMap::new(),
            project_root: PathBuf::new(),
            warnings: Arc::new(Mutex::new(Vec::new())),
        };
        let mut packet = ImpactPacket {
            changes: vec![ChangedFile {
                path: PathBuf::from("src/main.rs"),
                status: "Modified".to_string(),
                old_path: None,
                is_staged: true,
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

        CISelfAwarenessProvider
            .enrich(&context, &mut packet)
            .unwrap();

        assert!(packet.ci_config_change.is_none());
    }
}
