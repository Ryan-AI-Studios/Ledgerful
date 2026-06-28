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

    let current_dir = match std::env::current_dir() {
        Ok(d) => d,
        Err(e) => {
            tracing::debug!("Risk alert skipped: cannot get current dir: {:?}", e);
            return;
        }
    };
    let layout = Layout::new(current_dir.to_string_lossy().as_ref());
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

    #[test]
    fn test_push_risk_alert_deduplication() {
        let symbols: Vec<String> = vec!["fn_foo".to_string()];

        // First call should go through (will fail IPC in test, but logs debug).
        // Use unique paths so parallel tests don't interfere.
        push_risk_alert(
            "src/dedup_a.rs",
            "src/dedup_b.rs",
            0.95,
            &symbols,
            "Run tests for both files",
            "High",
            DEFAULT_RISK_ALERT_THRESHOLD,
        );

        // Second call with same pair (reversed order) should be deduplicated.
        push_risk_alert(
            "src/dedup_b.rs",
            "src/dedup_a.rs",
            0.95,
            &symbols,
            "Run tests for both files",
            "High",
            DEFAULT_RISK_ALERT_THRESHOLD,
        );

        // Verify the canonicalised pair exists and the reversed pair does not.
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
        let symbols: Vec<String> = vec!["fn_bar".to_string()];

        // Below default threshold of 0.90 — uses unique paths to avoid
        // interference from parallel tests that share the global dedup set.
        push_risk_alert(
            "src/below_thresh_a.rs",
            "src/below_thresh_b.rs",
            0.75,
            &symbols,
            "Remediation",
            "Medium",
            DEFAULT_RISK_ALERT_THRESHOLD,
        );

        // The pair should NOT be in the dedup set because it was rejected by threshold.
        let alerted = ALERTED_PAIRS.lock().unwrap();
        let pair = canonical_pair("src/below_thresh_a.rs", "src/below_thresh_b.rs");
        assert!(!alerted.contains(&pair));
    }

    #[test]
    fn test_push_risk_alert_different_pairs_not_deduplicated() {
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
        // Count may be > 2 when tests run in parallel sharing the global set.
        assert!(alerted.len() >= 2);
    }

    #[test]
    fn test_default_threshold_constant() {
        assert!((DEFAULT_RISK_ALERT_THRESHOLD - 0.90).abs() < 1e-6);
    }
}
