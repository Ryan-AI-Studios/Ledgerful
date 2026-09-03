use crate::impact::packet::ImpactPacket;
use crate::state::layout::Layout;
use crate::state::storage::StorageManager;
use crate::state::storage::packets::{PREDICTOR_SNAPSHOT_HISTORY_CAP, PacketHistory};
use crate::verify::engine::VerificationContext;
use crate::verify::predict::{
    PredictionResult, Predictor, StructuralCallData, TestMappingData, enrich_with_semantic,
};
use crate::verify::semantic_predictor;
use miette::Result;
use std::collections::{BTreeMap, BTreeSet};

use std::path::{Path, PathBuf};
use tracing::warn;

pub struct OutcomePredictor;

impl OutcomePredictor {
    pub fn predict(ctx: &mut VerificationContext) -> Result<PredictionResult> {
        if ctx.no_predict {
            return Ok(PredictionResult::default());
        }

        // Scope the mutable borrow of packet
        {
            let packet = match &mut ctx.packet {
                Some(p) => p,
                None => return Ok(PredictionResult::default()),
            };

            if packet.changes.is_empty() {
                return Ok(PredictionResult::default());
            }

            Self::recompute_temporal_if_missing(
                packet,
                &ctx.current_dir,
                &ctx.layout,
                &mut ctx.warnings,
            );
        }

        let history = match &ctx.storage {
            Some(storage) => match storage.get_recent_packets(PREDICTOR_SNAPSHOT_HISTORY_CAP) {
                Ok(PacketHistory {
                    packets,
                    truncated,
                    total_count,
                }) => {
                    if truncated {
                        ctx.add_warning(format!(
                            "packet history truncated to {N} of {M} snapshots",
                            N = PREDICTOR_SNAPSHOT_HISTORY_CAP,
                            M = total_count,
                        ));
                    }
                    packets
                }
                Err(err) => {
                    let warning = format!(
                        "Historical prediction degraded: failed to load packet history: {err}"
                    );
                    warn!("{warning}");
                    ctx.add_warning(warning);
                    Vec::new()
                }
            },
            None => Vec::new(),
        };

        // Performance fix (CG-F7): We no longer scan all files in the workspace dynamically
        // via `scan_current_imports` because parsing the entire tree synchronously on every
        // `verify --dry-run` is an unbounded O(N) operation (140+ CPU seconds on large repos).
        // Structural impact is primarily handled via the `structural_edges` DB (call_data).
        let current_imports = BTreeMap::new();

        let Some(packet) = ctx.packet.clone() else {
            return Ok(PredictionResult::default());
        };

        let call_data = match &ctx.storage {
            Some(storage) => Self::fetch_structural_call_data(&packet, storage, &mut ctx.warnings),
            None => StructuralCallData::default(),
        };

        let test_mapping_data = match &ctx.storage {
            Some(storage) => Self::fetch_test_mapping_data(&packet, storage, &mut ctx.warnings),
            None => TestMappingData::default(),
        };

        let mut prediction = Predictor::predict_with_test_mappings(
            &packet,
            &history,
            &current_imports,
            &call_data,
            &test_mapping_data,
        );

        for warning in &prediction.warnings {
            warn!("{}", warning);
            ctx.add_warning(warning.clone());
        }

        // Semantic prediction enrichment
        let semantic_weight = ctx.config.verify.semantic_weight;
        if semantic_weight > 0.0
            && let Some(storage) = &ctx.storage
        {
            let diff_text = semantic_predictor::build_diff_text(&packet);
            let mut embed_config = ctx.config.local_model.clone();
            embed_config.timeout_secs = 6;
            let conn = storage.get_connection();
            let history_count = crate::verify::predict::count_history_rows(conn).unwrap_or(0);

            if !embed_config.base_url.is_empty() && !diff_text.is_empty() {
                let mut semantic_warnings = Vec::new();
                use crate::verify::predict::SEMANTIC_COLD_START_THRESHOLD;
                let cold_start = history_count < SEMANTIC_COLD_START_THRESHOLD;
                if cold_start {
                    let msg = format!(
                        "Semantic prediction: warming up ({history_count}/{SEMANTIC_COLD_START_THRESHOLD} history records)"
                    );
                    warn!("{msg}");
                    semantic_warnings.push(msg);
                }

                if !cold_start {
                    match semantic_predictor::query_similar_outcomes(
                        conn,
                        &embed_config,
                        &diff_text,
                        30,
                    ) {
                        Ok(similar_outcomes) => {
                            let semantic_scores =
                                semantic_predictor::compute_semantic_scores(&similar_outcomes);
                            prediction = enrich_with_semantic(
                                prediction,
                                &semantic_scores,
                                semantic_weight,
                                &similar_outcomes,
                                history_count,
                            );
                        }
                        Err(e) => {
                            let warning = format!(
                                "Semantic prediction degraded: failed to query outcomes: {}",
                                e
                            );
                            warn!("{warning}");
                            semantic_warnings.push(warning);
                        }
                    }
                }
                for w in semantic_warnings {
                    ctx.add_warning(w);
                }
            }
        }

        // CI prediction enrichment
        if semantic_weight > 0.0
            && let Some(storage) = &ctx.storage
        {
            let diff_text = semantic_predictor::build_diff_text(&packet);
            let mut embed_config = ctx.config.local_model.clone();
            embed_config.timeout_secs = 6;

            if !embed_config.base_url.is_empty() && !diff_text.is_empty() {
                let conn = storage.get_connection();
                match crate::verify::ci_predictor::query_similar_ci_outcomes(
                    conn,
                    &embed_config,
                    &diff_text,
                    10,
                ) {
                    Ok(similar_ci) => {
                        // Human table/println! must not run under machine mode
                        // (`verify --json` sets suppress_human_output) — 0093 F1.
                        if !similar_ci.is_empty() && !ctx.suppress_human_output {
                            crate::output::verification::VerificationReporter::print_ci_predictions(
                                &similar_ci,
                                ctx.explain,
                                &embed_config,
                                &diff_text,
                            );
                        }
                    }
                    Err(e) => warn!("CI prediction failed: {e}"),
                }
            }
        }

        if ctx.explain && !prediction.explain_lines.is_empty() && !ctx.suppress_human_output {
            for line in &prediction.explain_lines {
                println!("{line}");
            }
        }

        Ok(prediction)
    }

    fn recompute_temporal_if_missing(
        packet: &mut ImpactPacket,
        current_dir: &Path,
        layout: &Layout,
        warnings: &mut Vec<String>,
    ) {
        if !packet.temporal_couplings.is_empty() || packet.changes.is_empty() {
            return;
        }

        let repo = match crate::git::repo::open_repo(current_dir) {
            Ok(repo) => repo,
            Err(err) => {
                let warning =
                    format!("Temporal prediction degraded: failed to open repository: {err}");
                warn!("{warning}");
                warnings.push(warning);
                return;
            }
        };

        let config = match crate::config::load::load_config(layout) {
            Ok(config) => config,
            Err(err) => {
                let warning = format!("Temporal prediction degraded: failed to load config: {err}");
                warn!("{warning}");
                warnings.push(warning);
                return;
            }
        };

        let provider = crate::impact::temporal::GixHistoryProvider::new(&repo);
        let engine = crate::impact::temporal::TemporalEngine::new(provider, config.temporal);

        match engine.calculate_couplings() {
            Ok(couplings) => {
                packet.temporal_couplings = couplings;
            }
            Err(err) => {
                let warning = format!("Temporal prediction degraded: {err}");
                warn!("{warning}");
                warnings.push(warning);
            }
        }
    }

    fn fetch_structural_call_data(
        packet: &ImpactPacket,
        storage: &StorageManager,
        _warnings: &mut Vec<String>,
    ) -> StructuralCallData {
        use rusqlite::OptionalExtension;

        let conn = storage.get_connection();

        // Check if structural_edges table exists and has data
        let has_edges: Option<i64> = match conn
            .query_row("SELECT count(*) FROM structural_edges LIMIT 1", [], |row| {
                row.get::<_, i64>(0)
            })
            .optional()
        {
            Ok(Some(count)) if count > 0 => Some(count),
            Ok(_) => None, // Table exists but is empty
            Err(_) => {
                // Table doesn't exist — graceful degradation
                return StructuralCallData::default();
            }
        };

        if has_edges.is_none() {
            return StructuralCallData::default();
        }

        // Collect changed symbol names
        let changed_symbols: Vec<String> = packet
            .changes
            .iter()
            .filter_map(|f| f.symbols.as_ref())
            .flat_map(|symbols| symbols.iter().map(|s| s.name.clone()))
            .collect();

        if changed_symbols.is_empty() {
            return StructuralCallData::default();
        }

        let mut callers = Vec::new();

        for callee_name in &changed_symbols {
            // Resolved edges
            if let Ok(mut stmt) = conn.prepare(
                "SELECT pf_caller.file_path, ps_caller.symbol_name
             FROM structural_edges se
             JOIN project_symbols ps_caller ON se.caller_symbol_id = ps_caller.id
             JOIN project_files pf_caller ON se.caller_file_id = pf_caller.id
             JOIN project_symbols ps_callee ON se.callee_symbol_id = ps_callee.id
             WHERE ps_callee.symbol_name = ?1
             AND se.callee_symbol_id IS NOT NULL",
            ) && let Ok(rows) = stmt.query_map([callee_name], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            }) {
                for row in rows.flatten() {
                    callers.push((PathBuf::from(row.0), row.1, callee_name.clone()));
                }
            }

            // Unresolved edges
            if let Ok(mut stmt) = conn.prepare(
                "SELECT pf_caller.file_path, ps_caller.symbol_name
             FROM structural_edges se
             JOIN project_symbols ps_caller ON se.caller_symbol_id = ps_caller.id
             JOIN project_files pf_caller ON se.caller_file_id = pf_caller.id
             WHERE se.unresolved_callee = ?1",
            ) && let Ok(rows) = stmt.query_map([callee_name], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            }) {
                for row in rows.flatten() {
                    // Deduplicate with resolved edges
                    let already_exists = callers.iter().any(|(path, sym, callee)| {
                        path == row.0.as_str() && sym == &row.1 && callee == callee_name
                    });
                    if !already_exists {
                        callers.push((PathBuf::from(&row.0), row.1, callee_name.clone()));
                    }
                }
            }
        }

        if callers.is_empty() {
            return StructuralCallData::default();
        }

        StructuralCallData { callers }
    }

    fn fetch_test_mapping_data(
        packet: &ImpactPacket,
        storage: &StorageManager,
        _warnings: &mut Vec<String>,
    ) -> TestMappingData {
        use rusqlite::OptionalExtension;

        let conn = storage.get_connection();

        // Gracefully skip if test_mapping table doesn't exist or is empty
        let has_mappings: Option<i64> = match conn
            .query_row("SELECT count(*) FROM test_mapping LIMIT 1", [], |row| {
                row.get::<_, i64>(0)
            })
            .optional()
        {
            Ok(Some(count)) if count > 0 => Some(count),
            Ok(_) => None,                               // Table exists but is empty
            Err(_) => return TestMappingData::default(), // Table doesn't exist
        };

        if has_mappings.is_none() {
            return TestMappingData::default();
        }

        // Collect changed symbol names
        let changed_symbols: Vec<String> = packet
            .changes
            .iter()
            .filter_map(|f| f.symbols.as_ref())
            .flat_map(|symbols| symbols.iter().map(|s| s.name.clone()))
            .collect();

        if changed_symbols.is_empty() {
            return TestMappingData::default();
        }

        // For each changed symbol, find test files that cover it
        let mut mappings: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();

        for symbol_name in &changed_symbols {
            // Query test_mapping joined with project_symbols and project_files
            // to find test files that cover this symbol
            if let Ok(mut stmt) = conn.prepare(
                "SELECT DISTINCT pf_test.file_path, ps_test.symbol_name
             FROM test_mapping tm
             JOIN project_symbols ps_test ON tm.test_symbol_id = ps_test.id
             JOIN project_files pf_test ON tm.test_file_id = pf_test.id
             JOIN project_symbols ps_tested ON tm.tested_symbol_id = ps_tested.id
             WHERE ps_tested.symbol_name = ?1",
            ) && let Ok(rows) = stmt.query_map([symbol_name], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            }) {
                for row in rows.flatten() {
                    mappings.entry(row.0).or_default().insert(row.1);
                }
            }
        }

        TestMappingData { mappings }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::model::Config;
    use crate::impact::packet::{ChangedFile, TemporalCoupling};
    use crate::state::storage::connection::in_memory_storage;
    use std::path::PathBuf;

    fn packet_with_change() -> ImpactPacket {
        ImpactPacket {
            changes: vec![ChangedFile {
                path: PathBuf::from("src/a.rs"),
                status: "Modified".to_string(),
                ..Default::default()
            }],
            temporal_couplings: vec![TemporalCoupling {
                file_a: PathBuf::from("src/a.rs"),
                file_b: PathBuf::from("src/b.rs"),
                score: 0.9,
            }],
            ..Default::default()
        }
    }

    fn predict_ctx(storage: StorageManager) -> VerificationContext {
        let tmp = tempfile::tempdir().unwrap();
        let root = camino::Utf8Path::from_path(tmp.path()).unwrap();
        VerificationContext {
            layout: Layout::new(root),
            current_dir: tmp.path().to_path_buf(),
            config: Config::default(),
            packet: Some(packet_with_change()),
            storage: Some(storage),
            no_predict: false,
            explain: false,
            health: false,
            warnings: Vec::new(),
            suppress_human_output: true,
            verbose: false,
        }
    }

    fn is_truncate_warning(warning: &str) -> bool {
        warning.contains("packet history truncated")
    }

    #[test]
    fn predictor_packet_history_two_snapshots_no_truncate_warning() {
        let storage = in_memory_storage();
        storage
            .save_packet(&ImpactPacket {
                head_hash: Some("h0".to_string()),
                ..Default::default()
            })
            .unwrap();
        storage
            .save_packet(&ImpactPacket {
                head_hash: Some("h1".to_string()),
                ..Default::default()
            })
            .unwrap();

        let mut ctx = predict_ctx(storage);
        OutcomePredictor::predict(&mut ctx).unwrap();
        assert!(
            ctx.warnings.iter().all(|w| !is_truncate_warning(w)),
            "unexpected truncation warning(s): {:?}",
            ctx.warnings
        );
    }

    #[test]
    fn predictor_packet_history_sixty_five_snapshots_emits_truncate_warning() {
        let storage = in_memory_storage();
        for i in 0..65 {
            storage
                .save_packet(&ImpactPacket {
                    head_hash: Some(format!("h{i}")),
                    ..Default::default()
                })
                .unwrap();
        }

        let mut ctx = predict_ctx(storage);
        OutcomePredictor::predict(&mut ctx).unwrap();
        let expected = format!(
            "packet history truncated to {N} of {M} snapshots",
            N = PREDICTOR_SNAPSHOT_HISTORY_CAP,
            M = 65
        );
        assert!(
            ctx.warnings.iter().any(|w| w == &expected),
            "missing exact truncation warning {expected:?} in {:?}",
            ctx.warnings
        );
    }

    #[test]
    fn predictor_storage_some_packet_none_returns_default_without_panic() {
        let storage = in_memory_storage();
        let tmp = tempfile::tempdir().unwrap();
        let root = camino::Utf8Path::from_path(tmp.path()).unwrap();
        let mut ctx = VerificationContext {
            layout: Layout::new(root),
            current_dir: tmp.path().to_path_buf(),
            config: Config::default(),
            packet: None,
            storage: Some(storage),
            no_predict: false,
            explain: false,
            health: false,
            warnings: Vec::new(),
            suppress_human_output: true,
            verbose: false,
        };
        let result = OutcomePredictor::predict(&mut ctx).unwrap();
        assert!(result.files.is_empty());
        assert!(result.warnings.is_empty());
    }

    #[test]
    fn predictor_packet_history_load_err_degraded_without_truncate_warning() {
        let storage = in_memory_storage();
        storage
            .get_connection()
            .execute_batch("PRAGMA foreign_keys = OFF; DROP TABLE snapshots;")
            .unwrap();

        let mut ctx = predict_ctx(storage);
        OutcomePredictor::predict(&mut ctx).unwrap();
        assert!(
            ctx.warnings
                .iter()
                .any(|w| w.contains("Historical prediction degraded")),
            "missing degraded warning in {:?}",
            ctx.warnings
        );
        assert!(
            ctx.warnings.iter().all(|w| !is_truncate_warning(w)),
            "truncation warning must not fire on load error: {:?}",
            ctx.warnings
        );
    }
}
