use crate::impact::packet::ImpactPacket;
use crate::state::layout::Layout;
use crate::state::storage::StorageManager;
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
            Some(storage) => match storage.get_all_packets() {
                Ok(history) => history,
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

        let call_data = match &ctx.storage {
            Some(storage) => {
                let packet = ctx.packet.as_ref().unwrap();
                Self::fetch_structural_call_data(packet, storage, &mut ctx.warnings)
            }
            None => StructuralCallData::default(),
        };

        let test_mapping_data = match &ctx.storage {
            Some(storage) => {
                let packet = ctx.packet.as_ref().unwrap();
                Self::fetch_test_mapping_data(packet, storage, &mut ctx.warnings)
            }
            None => TestMappingData::default(),
        };

        let mut prediction = {
            let packet = ctx.packet.as_ref().unwrap();
            Predictor::predict_with_test_mappings(
                packet,
                &history,
                &current_imports,
                &call_data,
                &test_mapping_data,
            )
        };

        for warning in &prediction.warnings {
            warn!("{}", warning);
            ctx.add_warning(warning.clone());
        }

        // Semantic prediction enrichment
        let semantic_weight = ctx.config.verify.semantic_weight;
        if semantic_weight > 0.0
            && let Some(storage) = &ctx.storage
        {
            let diff_text = semantic_predictor::build_diff_text(ctx.packet.as_ref().unwrap());
            let mut embed_config = ctx.config.local_model.clone();
            embed_config.timeout_secs = 6;
            let conn = storage.get_connection();
            let history_count = crate::verify::predict::count_history_rows(conn).unwrap_or(0);

            if !embed_config.base_url.is_empty() && !diff_text.is_empty() {
                let mut semantic_warnings = Vec::new();
                let cold_start = history_count < 5;
                if cold_start {
                    let msg = format!(
                        "Semantic prediction: warming up ({history_count}/50 history records)"
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
            let diff_text = semantic_predictor::build_diff_text(ctx.packet.as_ref().unwrap());
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
                        if !similar_ci.is_empty() {
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

        if ctx.explain && !prediction.explain_lines.is_empty() {
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
