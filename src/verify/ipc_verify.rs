use crate::impact::packet::{ChangedFile, FileAnalysisStatus, ImpactPacket, RiskLevel};
use crate::state::layout::Layout;
use crate::verify::predict::{Predictor, build_rule_scores};
use miette::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Structured response for IPC predictive verification.
/// Returned to the external context provider via the bridge when Ledgerful
/// is called as an inline verification gate during the capture phase.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IpcPredictiveResult {
    /// Probability that a verification step will fail [0.0, 1.0].
    pub failure_probability: f64,
    /// Whether unaudited drift exists in the ledger.
    pub drift_detected: bool,
    /// Overall risk level for the change set.
    pub risk_level: RiskLevel,
    /// Files predicted to need verification beyond the changed set.
    pub predicted_files: Vec<String>,
    /// Warnings from the prediction engine (e.g. missing coupling data).
    pub warnings: Vec<String>,
}

/// IPC entry-point for predictive verification.
///
/// Accepts an optional scope (list of file paths or directory globs) and
/// returns a deterministic `IpcPredictiveResult` suitable for an external
/// context provider's capture gate. The function targets <500ms by skipping embedding-based
/// prediction and using rule-based scores exclusively.
///
/// # Determinism
/// Results are deterministic for the same input state because:
/// - Rule-based scoring has no external randomness
/// - The impact packet is built deterministically from the provided scope
/// - Drift detection is a snapshot query
pub fn predictive_verify(
    scope: Option<Vec<String>>,
    layout: &Layout,
) -> Result<IpcPredictiveResult> {
    let packet = build_scoped_packet(scope);
    let prediction = Predictor::predict(&packet, &[]);

    let rule_scores = build_rule_scores(&prediction.files);
    let failure_probability = compute_failure_probability(&rule_scores);

    let drift_detected = check_drift(layout);

    let risk_level = derive_risk_level(failure_probability);

    let predicted_files: Vec<String> = prediction
        .files
        .iter()
        .map(|f| f.path.to_string_lossy().to_string())
        .collect();

    Ok(IpcPredictiveResult {
        failure_probability,
        drift_detected,
        risk_level,
        predicted_files,
        warnings: prediction.warnings,
    })
}

fn build_scoped_packet(scope: Option<Vec<String>>) -> ImpactPacket {
    let mut packet = ImpactPacket::default();

    if let Some(files) = scope {
        let mut changes: Vec<ChangedFile> = files
            .into_iter()
            .map(|path| ChangedFile {
                path: PathBuf::from(path),
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
            })
            .collect();
        changes.sort_unstable();
        changes.dedup();
        packet.changes = changes;
    }

    packet.finalize();
    packet
}

fn compute_failure_probability(rule_scores: &std::collections::BTreeMap<String, f64>) -> f64 {
    if rule_scores.is_empty() {
        return 0.0;
    }

    // Use the maximum rule score as the failure probability indicator.
    // LaPlace-smooth to avoid exact 0.0 or 1.0.
    let max_score = rule_scores
        .values()
        .fold(0.0f64, |acc, &s| if s > acc { s } else { acc });

    // Clamp and smooth: P = (max_score * 0.8 + 0.1), clamped to [0.05, 0.95]
    let raw = max_score * 0.8 + 0.1;
    raw.clamp(0.05, 0.95)
}

fn derive_risk_level(failure_probability: f64) -> RiskLevel {
    if failure_probability > 0.6 {
        RiskLevel::High
    } else if failure_probability > 0.3 {
        RiskLevel::Medium
    } else {
        RiskLevel::Low
    }
}

fn check_drift(layout: &Layout) -> bool {
    let db_path = layout.state_subdir().join("ledger.db");
    if !db_path.exists() {
        return false;
    }

    match rusqlite::Connection::open(db_path.as_std_path()) {
        Ok(conn) => {
            let count: Result<i64, _> = conn.query_row(
                "SELECT COUNT(*) FROM transactions WHERE status = 'UNAUDITED'",
                [],
                |row| row.get(0),
            );
            match count {
                Ok(n) => n > 0,
                Err(_) => false,
            }
        }
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn test_compute_failure_probability_empty() {
        let scores = BTreeMap::new();
        let prob = compute_failure_probability(&scores);
        assert!((prob - 0.0).abs() < 1e-6);
    }

    #[test]
    fn test_compute_failure_probability_bounded() {
        let scores: BTreeMap<String, f64> = [("tests/a.rs".to_string(), 0.95)].into();
        let prob = compute_failure_probability(&scores);
        // raw = 0.95 * 0.8 + 0.1 = 0.86, clamped to 0.95 max => 0.86
        assert!((prob - 0.86).abs() < 1e-2);
    }

    #[test]
    fn test_compute_failure_probability_max_capped() {
        let scores: BTreeMap<String, f64> = [("tests/a.rs".to_string(), 1.0)].into();
        let prob = compute_failure_probability(&scores);
        // raw = 1.0 * 0.8 + 0.1 = 0.9, clamped to max 0.95 => 0.9
        assert!(prob <= 0.95);
        assert!(prob > 0.0);
    }

    #[test]
    fn test_derive_risk_level() {
        assert_eq!(derive_risk_level(0.1), RiskLevel::Low);
        assert_eq!(derive_risk_level(0.3), RiskLevel::Low);
        assert_eq!(derive_risk_level(0.31), RiskLevel::Medium);
        assert_eq!(derive_risk_level(0.6), RiskLevel::Medium);
        assert_eq!(derive_risk_level(0.61), RiskLevel::High);
        assert_eq!(derive_risk_level(0.9), RiskLevel::High);
    }

    #[test]
    fn test_build_scoped_packet_empty() {
        let packet = build_scoped_packet(None);
        assert!(packet.changes.is_empty());
        assert_eq!(packet.risk_level, RiskLevel::Medium);
    }

    #[test]
    fn test_build_scoped_packet_with_files() {
        let scope = Some(vec!["src/main.rs".to_string(), "src/lib.rs".to_string()]);
        let packet = build_scoped_packet(scope);
        assert_eq!(packet.changes.len(), 2);
        assert_eq!(
            packet.changes[0].path.to_string_lossy().as_ref(),
            "src/lib.rs"
        );
        assert_eq!(
            packet.changes[1].path.to_string_lossy().as_ref(),
            "src/main.rs"
        );
    }

    #[test]
    fn test_build_scoped_packet_deduplicates() {
        let scope = Some(vec!["src/main.rs".to_string(), "src/main.rs".to_string()]);
        let packet = build_scoped_packet(scope);
        assert_eq!(packet.changes.len(), 1);
    }

    #[test]
    fn test_predictive_verify_deterministic() {
        let tmp = tempfile::tempdir().unwrap();
        let layout_path = camino::Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        let layout = Layout::new(layout_path.as_str());

        let scope = Some(vec![
            "src/verify/mod.rs".to_string(),
            "src/bridge/notify.rs".to_string(),
        ]);

        let result1 = predictive_verify(scope.clone(), &layout).unwrap();
        let result2 = predictive_verify(scope, &layout).unwrap();

        assert_eq!(result1.failure_probability, result2.failure_probability);
        assert_eq!(result1.drift_detected, result2.drift_detected);
        assert_eq!(result1.risk_level, result2.risk_level);
        assert_eq!(result1.predicted_files, result2.predicted_files);
        assert_eq!(result1.warnings, result2.warnings);
    }

    #[test]
    fn test_predictive_verify_returns_structured_result() {
        let tmp = tempfile::tempdir().unwrap();
        let layout_path = camino::Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        let layout = Layout::new(layout_path.as_str());

        let scope = Some(vec!["src/main.rs".to_string()]);
        let result = predictive_verify(scope, &layout).unwrap();

        // All fields should be populated
        assert!(result.failure_probability >= 0.0);
        assert!(result.failure_probability <= 1.0);
        assert!(!result.drift_detected); // No ledger DB in temp dir
        // risk_level can be Low/Medium/High
        // predicted_files should contain any structurally predicted files
        // warnings should be present (temporal coupling data missing)
        assert!(
            result
                .warnings
                .iter()
                .any(|w| w.contains("Temporal coupling"))
        );
    }

    #[test]
    fn test_check_drift_no_db() {
        let tmp = tempfile::tempdir().unwrap();
        let layout_path = camino::Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        let layout = Layout::new(layout_path.as_str());
        assert!(!check_drift(&layout));
    }

    #[test]
    fn test_ipc_predictive_result_serialization() {
        let result = IpcPredictiveResult {
            failure_probability: 0.42,
            drift_detected: true,
            risk_level: RiskLevel::Medium,
            predicted_files: vec!["tests/a.rs".to_string()],
            warnings: vec!["Something".to_string()],
        };

        let json = serde_json::to_string_pretty(&result).unwrap();
        assert!(json.contains("failureProbability"));
        assert!(json.contains("driftDetected"));
        assert!(json.contains("riskLevel"));
        assert!(json.contains("predictedFiles"));
        assert!(json.contains("warnings"));
    }
}
