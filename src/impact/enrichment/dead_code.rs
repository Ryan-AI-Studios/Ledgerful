use crate::impact::analysis::dead_code::ConfidenceScorer;
use crate::impact::enrichment::{EnrichmentContext, EnrichmentProvider};
use crate::impact::packet::ImpactPacket;
use miette::Result;
use tracing::{debug, warn};

pub struct DeadCodeProvider;

impl EnrichmentProvider for DeadCodeProvider {
    fn name(&self) -> &'static str {
        "DeadCode"
    }

    fn enrich(&self, context: &EnrichmentContext, packet: &mut ImpactPacket) -> Result<()> {
        if !context.config.dead_code.enabled {
            debug!("Dead code detection is disabled in config");
            return Ok(());
        }

        debug!("Enriching impact packet with dead code findings...");

        let cozo = context.storage.cozo.as_ref();
        let scorer = ConfidenceScorer::new(
            cozo,
            context.storage,
            &context.config.dead_code,
            &context.project_root,
            false,
        );

        for change in &packet.changes {
            let file_path = &change.path;
            match scorer.score_file(file_path) {
                Ok(findings) => {
                    packet.dead_code_findings.extend(findings);
                }
                Err(e) => {
                    warn!(
                        "Dead code scoring failed for {}: {}",
                        file_path.display(),
                        e
                    );
                    context.add_warning(format!(
                        "Dead code scoring failed for {}: {}",
                        file_path.display(),
                        e
                    ));
                }
            }
        }

        packet.dead_code_findings.sort_unstable();
        debug!(
            "Dead code enrichment added {} findings",
            packet.dead_code_findings.len()
        );

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::model::Config;
    use crate::impact::enrichment::EnrichmentContext;
    use crate::impact::packet::{ChangedFile, ImpactPacket};
    use crate::state::storage::StorageManager;
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    #[test]
    fn test_enrichment_skipped_when_disabled() {
        let storage =
            StorageManager::init_from_conn(rusqlite::Connection::open_in_memory().unwrap());
        let config = Config::default();
        let context = EnrichmentContext {
            storage: &storage,
            config: &config,
            file_id_map: HashMap::new(),
            project_root: PathBuf::from("."),
            warnings: Arc::new(Mutex::new(Vec::new())),
        };
        let mut packet = ImpactPacket::default();
        let provider = DeadCodeProvider;
        provider.enrich(&context, &mut packet).unwrap();
        assert!(packet.dead_code_findings.is_empty());
    }

    #[test]
    fn test_enrichment_populates_findings_when_enabled() {
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::state::migrations::get_migrations()
            .to_latest(&mut conn)
            .unwrap();
        let storage = StorageManager::init_from_conn(conn);

        // Insert a file and symbol
        let conn = storage.get_connection();
        conn.execute(
            "INSERT INTO project_files (file_path, language, content_hash, file_size, parse_status, last_indexed_at) VALUES ('src/lib.rs', 'Rust', 'h1', 100, 'OK', '2026-01-01')",
            [],
        ).unwrap();
        let file_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO project_symbols (file_id, qualified_name, symbol_name, symbol_kind, entrypoint_kind, last_indexed_at) VALUES (?1, 'crate::unused', 'unused', 'Function', 'INTERNAL', '2026-01-01')",
            [file_id],
        ).unwrap();

        let mut config = Config::default();
        config.dead_code.enabled = true;
        config.dead_code.confidence_threshold = 0.0; // Include everything for test

        let context = EnrichmentContext {
            storage: &storage,
            config: &config,
            file_id_map: HashMap::new(),
            project_root: PathBuf::from("."),
            warnings: Arc::new(Mutex::new(Vec::new())),
        };
        let mut packet = ImpactPacket {
            changes: vec![ChangedFile {
                path: PathBuf::from("src/lib.rs"),
                status: "Modified".to_string(),
                old_path: None,
                is_staged: true,
                symbols: None,
                imports: None,
                runtime_usage: None,
                analysis_status: crate::impact::packet::FileAnalysisStatus::default(),
                analysis_warnings: Vec::new(),
                api_routes: Vec::new(),
                data_models: Vec::new(),
                ci_gates: Vec::new(),
            }],
            ..ImpactPacket::default()
        };
        let provider = DeadCodeProvider;
        provider.enrich(&context, &mut packet).unwrap();
        assert!(!packet.dead_code_findings.is_empty());
    }
}
