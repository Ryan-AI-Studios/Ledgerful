use crate::impact::enrichment::{EnrichmentContext, EnrichmentProvider};
use crate::impact::packet::{CoverageDelta, ImpactPacket};
use crate::index::languages::{
    extract_error_handling, extract_logging_patterns, extract_telemetry_patterns,
};
use miette::Result;
use std::fs;
use tracing::debug;

pub struct ObservabilityProvider;

impl EnrichmentProvider for ObservabilityProvider {
    fn name(&self) -> &'static str {
        "Observability Enrichment Provider"
    }

    fn enrich(&self, context: &EnrichmentContext, packet: &mut ImpactPacket) -> Result<()> {
        if !context
            .storage
            .table_exists_and_has_data("observability_patterns")?
        {
            debug!(
                "Skipping observability enrichment: observability_patterns table is empty or missing."
            );
            return Ok(());
        }

        let conn = context.storage.get_connection();

        for change in &packet.changes {
            let Some(&file_id) = context.file_id_map.get(&change.path) else {
                continue;
            };

            let full_path = context.project_root.join(&change.path);
            let content = fs::read_to_string(&full_path).ok();

            // 1. Error Handling
            self.enrich_coverage(
                conn,
                file_id,
                change,
                content.as_deref(),
                "ERROR_HANDLE",
                &mut packet.error_handling_delta,
                |p, c| {
                    extract_error_handling(p, c).map(|v| v.iter().filter(|x| !x.in_test).count())
                },
            )?;

            // 2. Logging
            self.enrich_coverage(
                conn,
                file_id,
                change,
                content.as_deref(),
                "LOG",
                &mut packet.logging_coverage_delta,
                |p, c| {
                    extract_logging_patterns(p, c).map(|v| v.iter().filter(|x| !x.in_test).count())
                },
            )?;

            // 3. Telemetry
            self.enrich_coverage(
                conn,
                file_id,
                change,
                content.as_deref(),
                "TRACE",
                &mut packet.telemetry_coverage_delta,
                |p, c| {
                    extract_telemetry_patterns(p, c)
                        .map(|v| v.iter().filter(|x| !x.in_test).count())
                },
            )?;
        }

        // 4. Enrich specific observability signals (M7 logic)
        crate::observability::enrich_observability(
            packet,
            context.config,
            context.storage.get_connection(),
        )
        .map_err(|e| miette::miette!(e))?;

        Ok(())
    }
}

impl ObservabilityProvider {
    #[allow(clippy::too_many_arguments)]
    fn enrich_coverage<F>(
        &self,
        conn: &rusqlite::Connection,
        file_id: i64,
        change: &crate::impact::packet::ChangedFile,
        content: Option<&str>,
        kind: &str,
        deltas: &mut Vec<CoverageDelta>,
        extractor: F,
    ) -> Result<()>
    where
        F: Fn(&std::path::Path, &str) -> Result<usize>,
    {
        let previous_count: i64 = conn
            .query_row(
                "SELECT count(*) FROM observability_patterns WHERE file_id = ?1 AND pattern_kind = ?2 AND in_test = 0",
                rusqlite::params![file_id, kind],
                |row| row.get(0),
            )
            .unwrap_or(0);

        let current_count = if let Some(content) = content {
            match extractor(&change.path, content) {
                Ok(count) => count as i64,
                Err(_) => previous_count,
            }
        } else {
            previous_count
        };

        if current_count < previous_count {
            let delta = (previous_count - current_count) as usize;
            deltas.push(CoverageDelta {
                file_path: change.path.to_string_lossy().to_string(),
                pattern_kind: kind.to_string(),
                previous_count: previous_count as usize,
                current_count: current_count as usize,
                message: format!(
                    "{} reduced in {}: {} patterns removed",
                    kind,
                    change.path.display(),
                    delta
                ),
            });
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
    fn enrich_reads_observability_patterns() {
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
            "INSERT INTO observability_patterns (file_id, pattern_kind, confidence, in_test, last_indexed_at)
             VALUES (?1, 'LOG', 1.0, 0, '2026-01-01T00:00:00Z')",
            [file_id],
        )
        .unwrap();

        let storage = StorageManager::init_from_conn(conn);
        let mut file_id_map = HashMap::new();
        file_id_map.insert(PathBuf::from("nonexistent.rs"), file_id);
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
                path: PathBuf::from("nonexistent.rs"),
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

        ObservabilityProvider.enrich(&context, &mut packet).unwrap();

        assert!(packet.logging_coverage_delta.is_empty());
        assert!(packet.error_handling_delta.is_empty());
        assert!(packet.telemetry_coverage_delta.is_empty());
    }
}
