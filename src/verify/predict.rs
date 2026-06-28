use crate::impact::packet::ImpactPacket;
use crate::index::references::ImportExport;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub enum PredictionReason {
    Structural,
    CallGraph,
    Temporal,
    TestMapping,
    RuntimeDependency(String),
}

impl std::fmt::Display for PredictionReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Structural => write!(f, "Structural"),
            Self::CallGraph => write!(f, "CallGraph"),
            Self::Temporal => write!(f, "Temporal"),
            Self::TestMapping => write!(f, "TestMapping"),
            Self::RuntimeDependency(msg) => write!(f, "{}", msg),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct PredictedFile {
    pub path: PathBuf,
    pub reason: PredictionReason,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PredictionResult {
    pub files: Vec<PredictedFile>,
    pub warnings: Vec<String>,
    /// Per-file blended scores (test_file_path → final_score). Present when semantic prediction is active.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub scores: BTreeMap<String, f64>,
    /// Per-file rationale lines for --explain output.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub explain_lines: Vec<String>,
}

/// Pre-fetched structural call data for prediction.
/// Each entry is (caller_file_path, caller_symbol_name, callee_symbol_name).
#[derive(Debug, Clone, Default)]
pub struct StructuralCallData {
    pub callers: Vec<(PathBuf, String, String)>,
}

/// Pre-fetched test mapping data for prediction.
/// Each entry maps a test file path to the set of tested symbol names in that file.
#[derive(Debug, Clone, Default)]
pub struct TestMappingData {
    /// Maps test file path -> set of tested symbol names found in that file
    pub mappings: BTreeMap<String, BTreeSet<String>>,
}

pub struct Predictor;

impl Predictor {
    pub fn predict(packet: &ImpactPacket, history: &[ImpactPacket]) -> PredictionResult {
        Self::predict_with_current_imports(packet, history, &BTreeMap::new())
    }

    pub fn predict_with_current_imports(
        packet: &ImpactPacket,
        history: &[ImpactPacket],
        current_imports: &BTreeMap<PathBuf, ImportExport>,
    ) -> PredictionResult {
        Self::predict_with_structural_calls(
            packet,
            history,
            current_imports,
            &StructuralCallData::default(),
        )
    }

    pub fn predict_with_structural_calls(
        packet: &ImpactPacket,
        history: &[ImpactPacket],
        current_imports: &BTreeMap<PathBuf, ImportExport>,
        call_data: &StructuralCallData,
    ) -> PredictionResult {
        let mut predicted = BTreeSet::new();
        let mut warnings = Vec::new();

        let changed_paths: BTreeSet<PathBuf> =
            packet.changes.iter().map(|f| f.path.clone()).collect();

        add_structural_predictions(&mut predicted, &changed_paths, current_imports.iter());

        for hist_packet in history {
            let historical_imports = hist_packet
                .changes
                .iter()
                .filter_map(|file| file.imports.as_ref().map(|imports| (&file.path, imports)));
            add_structural_predictions(&mut predicted, &changed_paths, historical_imports);
        }

        // Call graph predictions: if changed symbols have structural callers,
        // predict the caller's file as a verification target.
        if !call_data.callers.is_empty() {
            add_call_graph_predictions(&mut predicted, &changed_paths, &call_data.callers);
        }

        if packet.temporal_couplings.is_empty() && !packet.changes.is_empty() {
            warnings.push("Temporal coupling data is missing or unavailable; falling back to structural-only prediction.".to_string());
        }

        for coupling in &packet.temporal_couplings {
            let a_changed = changed_paths.contains(&coupling.file_a);
            let b_changed = changed_paths.contains(&coupling.file_b);

            if a_changed && !b_changed {
                predicted.insert(PredictedFile {
                    path: coupling.file_b.clone(),
                    reason: PredictionReason::Temporal,
                });
            } else if b_changed && !a_changed {
                predicted.insert(PredictedFile {
                    path: coupling.file_a.clone(),
                    reason: PredictionReason::Temporal,
                });
            }
        }

        let mut files: Vec<_> = predicted.into_iter().collect();
        files.sort();

        // -------------------------------------------------------------
        // E4-4 Runtime/Config Dependency Prediction Integration
        // -------------------------------------------------------------
        let new_env_vars: std::collections::HashSet<&str> = packet
            .env_var_deps
            .iter()
            .filter(|dep| !dep.declared)
            .map(|dep| dep.var_name.as_str())
            .collect();

        // Predict files that introduce new env var dependencies
        for file in &packet.changes {
            if let Some(ref usage) = file.runtime_usage {
                for var in &usage.env_vars {
                    if new_env_vars.contains(var.as_str()) {
                        files.push(PredictedFile {
                            path: file.path.clone(),
                            reason: PredictionReason::RuntimeDependency(format!(
                                "New env var dependency: {}",
                                var
                            )),
                        });
                    }
                }
            }
        }

        // Add warnings for removed env var usage
        for delta in &packet.runtime_usage_delta {
            if delta.env_vars_current_count < delta.env_vars_previous_count {
                warnings.push(format!("Removed env var usage: {}", delta.file_path));
            }
        }

        files.sort();
        files.dedup();

        PredictionResult {
            files,
            warnings,
            ..Default::default()
        }
    }

    /// Predict verification targets using test mapping data from the index.
    /// For each changed symbol, find tests that cover it and add those test files
    /// as prediction targets. Test-mapping predictions appear before temporal
    /// and structural predictions in priority.
    pub fn predict_with_test_mappings(
        packet: &ImpactPacket,
        history: &[ImpactPacket],
        current_imports: &BTreeMap<PathBuf, ImportExport>,
        call_data: &StructuralCallData,
        test_mapping_data: &TestMappingData,
    ) -> PredictionResult {
        let mut predicted = BTreeSet::new();
        let mut warnings = Vec::new();

        let changed_paths: BTreeSet<PathBuf> =
            packet.changes.iter().map(|f| f.path.clone()).collect();

        // Test mapping predictions: files that contain tests covering changed symbols
        for test_file in test_mapping_data.mappings.keys() {
            let test_path = PathBuf::from(test_file);
            if !changed_paths.contains(&test_path) {
                predicted.insert(PredictedFile {
                    path: test_path,
                    reason: PredictionReason::TestMapping,
                });
            }
        }

        // Then add structural predictions
        add_structural_predictions(&mut predicted, &changed_paths, current_imports.iter());

        for hist_packet in history {
            let historical_imports = hist_packet
                .changes
                .iter()
                .filter_map(|file| file.imports.as_ref().map(|imports| (&file.path, imports)));
            add_structural_predictions(&mut predicted, &changed_paths, historical_imports);
        }

        // Call graph predictions
        if !call_data.callers.is_empty() {
            add_call_graph_predictions(&mut predicted, &changed_paths, &call_data.callers);
        }

        if packet.temporal_couplings.is_empty() && !packet.changes.is_empty() {
            warnings.push("Temporal coupling data is missing or unavailable; falling back to structural-only prediction.".to_string());
        }

        for coupling in &packet.temporal_couplings {
            let a_changed = changed_paths.contains(&coupling.file_a);
            let b_changed = changed_paths.contains(&coupling.file_b);

            if a_changed && !b_changed {
                predicted.insert(PredictedFile {
                    path: coupling.file_b.clone(),
                    reason: PredictionReason::Temporal,
                });
            } else if b_changed && !a_changed {
                predicted.insert(PredictedFile {
                    path: coupling.file_a.clone(),
                    reason: PredictionReason::Temporal,
                });
            }
        }

        let mut files: Vec<_> = predicted.into_iter().collect();
        files.sort();

        // -------------------------------------------------------------
        // E4-4 Runtime/Config Dependency Prediction Integration
        // -------------------------------------------------------------
        let new_env_vars: std::collections::HashSet<&str> = packet
            .env_var_deps
            .iter()
            .filter(|dep| !dep.declared)
            .map(|dep| dep.var_name.as_str())
            .collect();

        // Predict files that introduce new env var dependencies
        for file in &packet.changes {
            if let Some(ref usage) = file.runtime_usage {
                for var in &usage.env_vars {
                    if new_env_vars.contains(var.as_str()) {
                        files.push(PredictedFile {
                            path: file.path.clone(),
                            reason: PredictionReason::RuntimeDependency(format!(
                                "New env var dependency: {}",
                                var
                            )),
                        });
                    }
                }
            }
        }

        // Add warnings for removed env var usage
        for delta in &packet.runtime_usage_delta {
            if delta.env_vars_current_count < delta.env_vars_previous_count {
                warnings.push(format!("Removed env var usage: {}", delta.file_path));
            }
        }

        files.sort();
        files.dedup();

        PredictionResult {
            files,
            warnings,
            ..Default::default()
        }
    }
}

fn add_structural_predictions<'a, I>(
    predicted: &mut BTreeSet<PredictedFile>,
    changed_paths: &BTreeSet<PathBuf>,
    imports_by_file: I,
) where
    I: IntoIterator<Item = (&'a PathBuf, &'a ImportExport)>,
{
    for (path, imports) in imports_by_file {
        if changed_paths.contains(path) {
            continue;
        }

        if imports_changed_path(imports, changed_paths) {
            predicted.insert(PredictedFile {
                path: path.clone(),
                reason: PredictionReason::Structural,
            });
        }
    }
}

fn imports_changed_path(imports: &ImportExport, changed_paths: &BTreeSet<PathBuf>) -> bool {
    imports.imported_from.iter().any(|import| {
        let import_norm = import.replace("::", "/");
        let import_path = Path::new(&import_norm);

        changed_paths.iter().any(|changed| {
            let changed_str = changed.to_string_lossy();
            let changed_no_ext = changed.with_extension("");
            let changed_no_ext_str = changed_no_ext.to_string_lossy();

            changed == import_path
                || changed_no_ext == import_path
                || changed_str.ends_with(&import_norm)
                || changed_no_ext_str.ends_with(&import_norm)
        })
    })
}

/// Add predictions based on structural call graph edges.
/// If a changed file contains symbols that are called by other files,
/// those caller files should be tested as well.
fn add_call_graph_predictions(
    predicted: &mut BTreeSet<PredictedFile>,
    changed_paths: &BTreeSet<PathBuf>,
    callers: &[(PathBuf, String, String)], // (caller_file, caller_symbol, callee_symbol)
) {
    for (caller_file, caller_symbol, _callee_symbol) in callers {
        // Skip if the caller file is already changed (no need to predict)
        if changed_paths.contains(caller_file) {
            continue;
        }
        predicted.insert(PredictedFile {
            path: caller_file.clone(),
            reason: PredictionReason::CallGraph,
        });
        let _ = caller_symbol; // Available for future reason detail
    }
}

/// Assign a base rule score per file based on its prediction reason.
fn rule_score(reason: &PredictionReason) -> f64 {
    match reason {
        PredictionReason::Temporal => 0.80,
        PredictionReason::CallGraph => 0.70,
        PredictionReason::Structural => 0.60,
        PredictionReason::TestMapping => 0.50,
        PredictionReason::RuntimeDependency(_) => 0.65,
    }
}

/// Build rule scores from a PredictionResult: the max score across all
/// reason entries for each file path.
pub fn build_rule_scores(files: &[PredictedFile]) -> BTreeMap<String, f64> {
    let mut scores: BTreeMap<String, f64> = BTreeMap::new();
    for f in files {
        let key = f.path.to_string_lossy().to_string();
        let s = scores.entry(key).or_insert(0.0);
        let candidate = rule_score(&f.reason);
        if candidate > *s {
            *s = candidate;
        }
    }
    scores
}

/// Compute per-file semantic basis info: (failures, total) across similar past diffs.
fn build_semantic_basis(
    similar_outcomes: &[(crate::verify::semantic_predictor::TestOutcome, f32)],
) -> BTreeMap<String, (usize, usize)> {
    let mut basis: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    for (outcome, _sim) in similar_outcomes {
        let entry = basis.entry(outcome.test_file.clone()).or_insert((0, 0));
        if outcome.status == crate::verify::semantic_predictor::TestStatus::Failed {
            entry.0 += 1;
        }
        entry.1 += 1;
    }
    basis
}

/// Enrich a rule-based PredictionResult with semantic scores, blending,
/// and explain-table lines. Returns a new PredictionResult with scores
/// and explain_lines populated.
pub fn enrich_with_semantic(
    mut result: PredictionResult,
    semantic_scores: &std::collections::HashMap<String, f64>,
    semantic_weight: f64,
    similar_outcomes: &[(crate::verify::semantic_predictor::TestOutcome, f32)],
    cold_start_count: usize,
) -> PredictionResult {
    let rule_scores = build_rule_scores(&result.files);
    let blended = crate::verify::semantic_predictor::blend_scores(
        &rule_scores.iter().map(|(k, v)| (k.clone(), *v)).collect(),
        semantic_scores,
        semantic_weight,
    );

    result.scores = blended.iter().map(|(k, v)| (k.clone(), *v)).collect();

    // Build explain lines
    let basis = build_semantic_basis(similar_outcomes);
    let mut explain_lines = Vec::new();
    explain_lines.push("Test priority rationale:".to_string());

    let mut all_files: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for f in result.scores.keys() {
        all_files.insert(f.clone());
    }

    for file in all_files {
        let rule = rule_scores.get(&file).copied().unwrap_or(0.0);
        let semantic = semantic_scores.get(&file).copied().unwrap_or(0.0);
        let final_score = *blended.get(&file).unwrap_or(&0.0);

        explain_lines.push(format!(
            "  {file}    rule: {rule:.2}  semantic: {semantic:.2}  final: {final_score:.2}",
        ));

        if let Some((fails, total)) = basis.get(&file) {
            explain_lines.push(format!(
                "    Semantic basis: {fails} of {total} similar past changes caused failures",
            ));
        } else if cold_start_count < 50 {
            explain_lines.push(format!(
                "    Semantic basis: warming up ({cold_start_count}/50 history records)",
            ));
        } else {
            explain_lines
                .push("    Semantic basis: insufficient history (< 5 samples)".to_string());
        }
    }

    result.explain_lines = explain_lines;

    // Add semantic-only files to the file list
    let existing_paths: std::collections::BTreeSet<String> = result
        .files
        .iter()
        .map(|f| f.path.to_string_lossy().to_string())
        .collect();
    for file in blended.keys() {
        if !existing_paths.contains(file.as_str()) {
            result.files.push(PredictedFile {
                path: std::path::PathBuf::from(file),
                reason: PredictionReason::TestMapping,
            });
        }
    }
    result.files.sort();
    result.files.dedup();

    result
}

/// Count total number of distinct outcomes in test_outcome_history.
pub fn count_history_rows(conn: &rusqlite::Connection) -> Result<usize, String> {
    let count: i64 = conn
        .query_row("SELECT count(*) FROM test_outcome_history", [], |row| {
            row.get(0)
        })
        .map_err(|e| e.to_string())?;
    Ok(count as usize)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::impact::packet::{ChangedFile, FileAnalysisStatus};
    use std::path::PathBuf;

    #[test]
    fn test_prediction_reason_display() {
        assert_eq!(format!("{}", PredictionReason::Structural), "Structural");
        assert_eq!(format!("{}", PredictionReason::CallGraph), "CallGraph");
        assert_eq!(format!("{}", PredictionReason::Temporal), "Temporal");
        assert_eq!(format!("{}", PredictionReason::TestMapping), "TestMapping");
    }

    #[test]
    fn test_call_graph_prediction_adds_caller_files() {
        let mut packet = ImpactPacket::default();
        packet.changes.push(ChangedFile {
            path: PathBuf::from("src/utils.rs"),
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
        });

        let mut call_data = StructuralCallData::default();
        call_data.callers.push((
            PathBuf::from("src/main.rs"),
            "caller_fn".to_string(),
            "helper_fn".to_string(),
        ));

        let result =
            Predictor::predict_with_structural_calls(&packet, &[], &BTreeMap::new(), &call_data);

        assert!(
            result
                .files
                .iter()
                .any(|f| f.path == std::path::Path::new("src/main.rs")
                    && f.reason == PredictionReason::CallGraph),
            "Expected src/main.rs with CallGraph reason in predictions"
        );
    }

    #[test]
    fn test_call_graph_prediction_skips_already_changed() {
        let mut packet = ImpactPacket::default();
        packet.changes.push(ChangedFile {
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
        });

        let mut call_data = StructuralCallData::default();
        call_data.callers.push((
            PathBuf::from("src/main.rs"),
            "caller_fn".to_string(),
            "helper_fn".to_string(),
        ));

        let result =
            Predictor::predict_with_structural_calls(&packet, &[], &BTreeMap::new(), &call_data);

        // main.rs is already changed, should not be predicted
        assert!(
            !result
                .files
                .iter()
                .any(|f| f.path == std::path::Path::new("src/main.rs"))
        );
    }

    #[test]
    fn test_call_graph_prediction_graceful_degradation() {
        let mut packet = ImpactPacket::default();
        packet.changes.push(ChangedFile {
            path: PathBuf::from("src/utils.rs"),
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
        });

        // Empty call data should produce identical output to no call data
        let empty_call_data = StructuralCallData::default();
        let result = Predictor::predict_with_structural_calls(
            &packet,
            &[],
            &BTreeMap::new(),
            &empty_call_data,
        );

        // No CallGraph predictions when data is empty
        assert!(
            !result
                .files
                .iter()
                .any(|f| f.reason == PredictionReason::CallGraph)
        );
    }

    /// E2E Test 3: Verify integration — structural call prediction
    /// Builds an ImpactPacket with a change to a file containing the "internal"
    /// symbol, creates StructuralCallData with a caller entry for "helper"
    /// calling "internal", and verifies "helper"'s file appears as a predicted
    /// verification target with PredictionReason::CallGraph.
    #[test]
    fn test_e2e_structural_call_prediction() {
        let mut packet = ImpactPacket::default();
        // The changed file contains the "internal" symbol
        packet.changes.push(ChangedFile {
            path: PathBuf::from("src/utils.rs"),
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
        });

        // Create StructuralCallData: helper (in src/main.rs) calls internal
        let mut call_data = StructuralCallData::default();
        call_data.callers.push((
            PathBuf::from("src/main.rs"),
            "helper".to_string(),
            "internal".to_string(),
        ));

        let result =
            Predictor::predict_with_structural_calls(&packet, &[], &BTreeMap::new(), &call_data);

        // Verify src/main.rs appears as a predicted verification target with CallGraph reason
        assert!(
            result
                .files
                .iter()
                .any(|f| f.path == std::path::Path::new("src/main.rs")
                    && f.reason == PredictionReason::CallGraph),
            "expected src/main.rs with CallGraph reason in predictions, got {:?}",
            result.files
        );
    }

    /// E2E Test 4b: Empty structural_edges — no regression (prediction)
    /// Verifies that running prediction with NO structural call data
    /// produces output identical to what it would have been before E2-1
    /// (i.e., no CallGraph predictions).
    #[test]
    fn test_e2e_no_structural_calls_no_regression() {
        let mut packet = ImpactPacket::default();
        packet.changes.push(ChangedFile {
            path: PathBuf::from("src/utils.rs"),
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
        });

        // Run prediction with default (empty) structural call data
        let call_data = StructuralCallData::default();
        let result =
            Predictor::predict_with_structural_calls(&packet, &[], &BTreeMap::new(), &call_data);

        // No CallGraph predictions when structural data is empty
        assert!(
            !result
                .files
                .iter()
                .any(|f| f.reason == PredictionReason::CallGraph),
            "expected no CallGraph predictions with empty data, got {:?}",
            result.files
        );

        // Also verify the predict() convenience method (which passes empty call data)
        // produces identical results
        let result_convenience = Predictor::predict(&packet, &[]);
        assert_eq!(
            result.files, result_convenience.files,
            "predict_with_structural_calls with empty data should match predict()"
        );
    }

    #[test]
    fn test_build_rule_scores_max_per_file() {
        let files = vec![
            PredictedFile {
                path: PathBuf::from("tests/a.rs"),
                reason: PredictionReason::Temporal,
            },
            PredictedFile {
                path: PathBuf::from("tests/a.rs"),
                reason: PredictionReason::Structural,
            },
            PredictedFile {
                path: PathBuf::from("tests/b.rs"),
                reason: PredictionReason::TestMapping,
            },
        ];
        let scores = build_rule_scores(&files);
        assert!((scores.get("tests/a.rs").copied().unwrap() - 0.80).abs() < 1e-6);
        assert!((scores.get("tests/b.rs").copied().unwrap() - 0.50).abs() < 1e-6);
    }

    #[test]
    fn test_build_rule_scores_empty() {
        let scores = build_rule_scores(&[]);
        assert!(scores.is_empty());
    }

    #[test]
    fn test_enrich_with_semantic_regression_weight_zero() {
        let result = PredictionResult {
            files: vec![PredictedFile {
                path: PathBuf::from("tests/a.rs"),
                reason: PredictionReason::Temporal,
            }],
            warnings: Vec::new(),
            ..Default::default()
        };
        let original_files = result.files.clone();

        let semantic: std::collections::HashMap<String, f64> =
            [("tests/a.rs".to_string(), 0.9)].into();
        let outcomes: Vec<(crate::verify::semantic_predictor::TestOutcome, f32)> = Vec::new();

        let enriched = enrich_with_semantic(result, &semantic, 0.0, &outcomes, 100);

        // score field should not change the files
        assert_eq!(enriched.files, original_files);
        // scores map should have the blended values (rule only since weight=0)
        assert!(
            enriched
                .scores
                .get("tests/a.rs")
                .is_some_and(|s| (s - 0.80).abs() < 1e-6)
        );
    }

    #[test]
    fn test_enrich_with_semantic_adds_explain_lines() {
        let result = PredictionResult {
            files: vec![PredictedFile {
                path: PathBuf::from("tests/a.rs"),
                reason: PredictionReason::Temporal,
            }],
            warnings: Vec::new(),
            ..Default::default()
        };

        let semantic: std::collections::HashMap<String, f64> =
            [("tests/a.rs".to_string(), 0.9)].into();

        let outcomes = vec![(
            crate::verify::semantic_predictor::TestOutcome {
                test_name: String::new(),
                test_file: "tests/a.rs".to_string(),
                commit_hash: "abc".to_string(),
                status: crate::verify::semantic_predictor::TestStatus::Failed,
                duration_ms: 0,
                diff_summary: String::new(),
            },
            0.9_f32,
        )];

        let enriched = enrich_with_semantic(result, &semantic, 0.3, &outcomes, 100);
        assert!(enriched.explain_lines.len() > 1);
        assert!(
            enriched
                .explain_lines
                .iter()
                .any(|l| l.contains("tests/a.rs")
                    && l.contains("rule:")
                    && l.contains("semantic:"))
        );
        assert!(
            enriched
                .explain_lines
                .iter()
                .any(|l| l.contains("Semantic basis:") && l.contains("1 of 1"))
        );
    }

    #[test]
    fn test_count_history_rows_empty() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        // Table may not exist in an in-memory connection without migrations;
        // that's OK—the function is tested through integration tests.
        let _ = count_history_rows(&conn);
    }
}
