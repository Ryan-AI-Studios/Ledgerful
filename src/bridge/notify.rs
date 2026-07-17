use crate::bridge::ipc::IpcClient;
use crate::bridge::model::{BridgeDirection, BridgePayload, BridgeRecord, BridgeVerifyOutcome};
use crate::state::layout::Layout;
use std::collections::HashSet;
use std::sync::LazyLock;
use std::sync::Mutex;
use std::thread;
use std::time::Duration;

/// Default coupling threshold above which risk alerts are emitted.
pub const DEFAULT_RISK_ALERT_THRESHOLD: f64 = 0.90;

/// Per-session deduplication set for risk alerts.
/// Keys are (file_a, file_b) pairs, sorted lexicographically so
/// (a, b) and (b, a) map to the same entry.
static ALERTED_PAIRS: LazyLock<Mutex<HashSet<(String, String)>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));

pub fn push_verify_results(results: Vec<BridgeVerifyOutcome>) {
    let current_dir = match std::env::current_dir() {
        Ok(d) => d,
        Err(_) => return,
    };
    let layout = Layout::new(current_dir.to_string_lossy().as_ref());
    if !crate::bridge::client::is_bridge_enabled(&layout) {
        return;
    }
    let project_id = layout.get_project_id();

    let records: Vec<BridgeRecord> = results
        .into_iter()
        .map(|outcome| {
            BridgeRecord::new(
                BridgeDirection::Outbound,
                project_id.clone(),
                "verify_outcome",
                BridgePayload::VerifyOutcome(outcome),
            )
        })
        .collect();

    // Fire and forget in a separate thread to avoid delaying CLI exit
    thread::spawn(move || {
        if let Ok(mut client) = IpcClient::connect_with_timeout(Duration::from_millis(100)) {
            for record in records {
                let _ = client.send_record(&record);
            }
        }
    });
}

/// Emit a `BridgeRecord::RiskAlert` when the watcher detects temporal coupling
/// above a configurable threshold.
///
/// This is fire-and-forget: IPC failures are trapped at `tracing::debug!` level
/// and never crash the watcher. Deduplication ensures each coupling pair only
/// triggers one alert per session.
///
/// # Arguments
/// * `file_a`, `file_b` - The coupled file paths (order is normalised internally).
/// * `coupling_score` - The temporal coupling score [0.0, 1.0].
/// * `affected_symbols` - Symbols from the changed files involved in the coupling.
/// * `suggested_remediation` - Human-readable remediation scope suggestion.
/// * `risk_level` - The derived risk level string.
/// * `threshold` - Coupling score threshold; alerts only fire when `coupling_score >= threshold`.
pub fn push_risk_alert(
    file_a: &str,
    file_b: &str,
    coupling_score: f64,
    affected_symbols: &[String],
    suggested_remediation: &str,
    risk_level: &str,
    threshold: f64,
) {
    let current_dir = match std::env::current_dir() {
        Ok(d) => d,
        Err(e) => {
            tracing::debug!("Risk alert skipped: cannot get current dir: {:?}", e);
            return;
        }
    };
    let layout = Layout::new(current_dir.to_string_lossy().as_ref());
    if !crate::bridge::client::is_bridge_enabled(&layout) {
        return;
    }

    // Deduplication: canonicalise the pair so (a,b) == (b,a)
    let pair = if file_a <= file_b {
        (file_a.to_string(), file_b.to_string())
    } else {
        (file_b.to_string(), file_a.to_string())
    };

    // Threshold check first: below-threshold pairs never enter the dedup set.
    if coupling_score < threshold {
        tracing::debug!(
            "Risk alert suppressed (below threshold {}): {} <-> {} score={:.4}",
            threshold,
            pair.0,
            pair.1,
            coupling_score
        );
        return;
    }

    {
        let mut alerted = match ALERTED_PAIRS.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        if !alerted.insert(pair.clone()) {
            tracing::debug!(
                "Risk alert suppressed (duplicate pair): {} <-> {}",
                pair.0,
                pair.1
            );
            return;
        }
    }

    let project_id = layout.get_project_id();

    let record = BridgeRecord::new(
        BridgeDirection::Outbound,
        project_id,
        "risk_alert",
        BridgePayload::RiskAlert {
            coupled_file_a: pair.0.clone(),
            coupled_file_b: pair.1.clone(),
            coupling_score,
            affected_symbols: affected_symbols.to_vec(),
            suggested_remediation: suggested_remediation.to_string(),
            risk_level: risk_level.to_string(),
        },
    );

    // Fire-and-forget in a separate thread so IPC failures never block or crash the watcher.
    thread::spawn(
        move || match IpcClient::connect_with_timeout(Duration::from_millis(100)) {
            Ok(mut client) => {
                if let Err(e) = client.send_record(&record) {
                    tracing::debug!("Failed to send risk alert via IPC: {:?}", e);
                }
            }
            Err(e) => {
                tracing::debug!("Failed to connect IPC for risk alert: {:?}", e);
            }
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper to canonicalise a pair the same way push_risk_alert does.
    fn canonical_pair(a: &str, b: &str) -> (String, String) {
        if a <= b {
            (a.to_string(), b.to_string())
        } else {
            (b.to_string(), a.to_string())
        }
    }

    /// Create a tempdir with bridge enabled so the dedup/threshold paths are reached.
    fn enabled_bridge_tmpdir() -> (tempfile::TempDir, crate::tests::DirGuard) {
        let tmp = tempfile::tempdir().unwrap();
        let root = camino::Utf8Path::from_path(tmp.path()).unwrap();
        let layout = Layout::new(root);
        layout.ensure_state_dir().unwrap();
        let config_path = layout.config_file();
        std::fs::write(config_path, "[bridge]\nenabled = true\n").unwrap();
        let guard = crate::tests::DirGuard::new(tmp.path());
        (tmp, guard)
    }

    #[test]
    fn test_push_risk_alert_deduplication() {
        let (_tmp, _guard) = enabled_bridge_tmpdir();
        let symbols: Vec<String> = vec!["fn_foo".to_string()];

        push_risk_alert(
            "src/dedup_a.rs",
            "src/dedup_b.rs",
            0.95,
            &symbols,
            "Run tests for both files",
            "High",
            DEFAULT_RISK_ALERT_THRESHOLD,
        );

        push_risk_alert(
            "src/dedup_b.rs",
            "src/dedup_a.rs",
            0.95,
            &symbols,
            "Run tests for both files",
            "High",
            DEFAULT_RISK_ALERT_THRESHOLD,
        );

        let alerted = ALERTED_PAIRS.lock().unwrap();
        let pair_canon = canonical_pair("src/dedup_a.rs", "src/dedup_b.rs");
        let pair_rev = if pair_canon.0 == "src/dedup_a.rs" {
            ("src/dedup_b.rs".to_string(), "src/dedup_a.rs".to_string())
        } else {
            ("src/dedup_a.rs".to_string(), "src/dedup_b.rs".to_string())
        };
        assert!(alerted.contains(&pair_canon));
        assert!(!alerted.contains(&pair_rev));
    }

    #[test]
    fn test_push_risk_alert_below_threshold() {
        let (_tmp, _guard) = enabled_bridge_tmpdir();
        let symbols: Vec<String> = vec!["fn_bar".to_string()];

        push_risk_alert(
            "src/below_thresh_a.rs",
            "src/below_thresh_b.rs",
            0.75,
            &symbols,
            "Remediation",
            "Medium",
            DEFAULT_RISK_ALERT_THRESHOLD,
        );

        let alerted = ALERTED_PAIRS.lock().unwrap();
        let pair = canonical_pair("src/below_thresh_a.rs", "src/below_thresh_b.rs");
        assert!(!alerted.contains(&pair));
    }

    #[test]
    fn test_push_risk_alert_different_pairs_not_deduplicated() {
        let (_tmp, _guard) = enabled_bridge_tmpdir();
        let symbols: Vec<String> = vec!["fn_a".to_string()];

        push_risk_alert(
            "src/notdedup_one.rs",
            "src/notdedup_two.rs",
            0.92,
            &symbols,
            "Remediation 1",
            "High",
            DEFAULT_RISK_ALERT_THRESHOLD,
        );

        push_risk_alert(
            "src/notdedup_three.rs",
            "src/notdedup_four.rs",
            0.93,
            &symbols,
            "Remediation 2",
            "High",
            DEFAULT_RISK_ALERT_THRESHOLD,
        );

        let alerted = ALERTED_PAIRS.lock().unwrap();
        let pair1 = canonical_pair("src/notdedup_one.rs", "src/notdedup_two.rs");
        let pair2 = canonical_pair("src/notdedup_three.rs", "src/notdedup_four.rs");
        assert!(alerted.contains(&pair1));
        assert!(alerted.contains(&pair2));
        assert!(alerted.len() >= 2);
    }

    #[test]
    fn test_default_threshold_constant() {
        assert!((DEFAULT_RISK_ALERT_THRESHOLD - 0.90).abs() < 1e-6);
    }

    #[test]
    fn push_verify_results_disabled_does_not_spawn() {
        let tmp = tempfile::tempdir().unwrap();
        let root = camino::Utf8Path::from_path(tmp.path()).unwrap();
        let layout = Layout::new(root);
        layout.ensure_state_dir().unwrap();
        let config_path = layout.config_file();
        std::fs::write(config_path, "[bridge]\nenabled = false\n").unwrap();

        let _guard = crate::tests::DirGuard::new(tmp.path());

        let outcomes = vec![BridgeVerifyOutcome {
            success: true,
            command: "cargo test".to_string(),
            error_snippet: None,
        }];

        // When disabled, the function returns before spawning a thread.
        push_verify_results(outcomes);
    }

    #[test]
    fn push_risk_alert_disabled_does_not_spawn() {
        let tmp = tempfile::tempdir().unwrap();
        let root = camino::Utf8Path::from_path(tmp.path()).unwrap();
        let layout = Layout::new(root);
        layout.ensure_state_dir().unwrap();
        let config_path = layout.config_file();
        std::fs::write(config_path, "[bridge]\nenabled = false\n").unwrap();

        let _guard = crate::tests::DirGuard::new(tmp.path());

        // Use a unique pair so a parallel test that inserts into the global
        // dedup set cannot make this assertion falsely pass.
        let unique_a = "src/disabled_a_0065.rs";
        let unique_b = "src/disabled_b_0065.rs";

        push_risk_alert(
            unique_a,
            unique_b,
            0.95,
            &["fn_a".to_string()],
            "Remediation",
            "High",
            DEFAULT_RISK_ALERT_THRESHOLD,
        );

        // No IPC thread should have been spawned; the global dedup set should
        // not contain the pair because the gate returned before insertion.
        let alerted = ALERTED_PAIRS.lock().unwrap();
        let pair = canonical_pair(unique_a, unique_b);
        assert!(!alerted.contains(&pair));
    }
}
