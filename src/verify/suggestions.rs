use crate::state::layout::Layout;
use crate::state::storage::StorageManager;
use serde::{Deserialize, Serialize};

use super::results::VerificationReport;

// ---------------------------------------------------------------------------
// Severity
// ---------------------------------------------------------------------------

/// Severity drives sort order: `Info` < `Warning` < `ActionRequired`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SuggestionSeverity {
    Info,
    Warning,
    ActionRequired,
}

// ---------------------------------------------------------------------------
// Suggestion
// ---------------------------------------------------------------------------

/// A single actionable suggestion surfaced in `verify` output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Suggestion {
    /// Stable identifier for deduplication / suppression (e.g. "unaudited-drift-reconcile").
    pub id: String,
    /// Human-readable description of the issue.
    pub description: String,
    /// Copy-paste-ready `ledgerful` command (or commands).
    pub command: String,
    /// Severity level.
    pub severity: SuggestionSeverity,
}

// ---------------------------------------------------------------------------
// Ledger status snapshot
// ---------------------------------------------------------------------------

/// Snapshot of the ledger relevant for generating fix suggestions.
#[derive(Debug, Clone, Default)]
pub struct LedgerStatus {
    /// Number of UNAUDITED transactions.
    pub unaudited_count: usize,
    /// Whether any PENDING transaction is older than 24 hours.
    pub has_stale_pending: bool,
    /// Whether no impact packet exists (packet missing).
    pub no_impact_report: bool,
}

// ---------------------------------------------------------------------------
// Suggestion generators
// ---------------------------------------------------------------------------

/// Generate suggestions based on verification results and ledger status.
///
/// Suggestions are deterministically sorted: `severity` descending, then
/// `description` ascending.
pub fn generate_suggestions(
    report: &VerificationReport,
    ledger_status: &LedgerStatus,
) -> Vec<Suggestion> {
    let mut suggestions: Vec<Suggestion> = Vec::new();

    // --- UNAUDITED drift --------------------------------------------------
    if ledger_status.unaudited_count > 0 {
        suggestions.push(Suggestion {
            id: "unaudited-drift-reconcile".to_string(),
            description: format!(
                "{} UNAUDITED drift transaction(s) detected — reconcile the ledger",
                ledger_status.unaudited_count
            ),
            command: "ledgerful ledger reconcile --all --reason \"verify follow-up\"".to_string(),
            severity: SuggestionSeverity::ActionRequired,
        });
    }

    // --- Stale PENDING (> 24h) --------------------------------------------
    if ledger_status.has_stale_pending {
        suggestions.push(Suggestion {
            id: "stale-pending-status".to_string(),
            description: "Stale PENDING transaction(s) (>24h) — inspect then commit or rollback"
                .to_string(),
            command: concat!(
                "ledgerful ledger status   # list pending transactions\n",
                "# then:\n",
                "#   ledgerful ledger commit <tx-id> --summary \"...\"\n",
                "#   ledgerful ledger rollback <tx-id> --reason \"stale\""
            )
            .to_string(),
            severity: SuggestionSeverity::Warning,
        });
    }

    // --- Verification step non-zero exit ----------------------------------
    if !report.results.is_empty() && report.results.iter().any(|r| r.exit_code != 0) {
        suggestions.push(Suggestion {
            id: "verify-failure-impact".to_string(),
            description: "Verification command(s) failed — re-assess blast radius".to_string(),
            command: "ledgerful impact --summary".to_string(),
            severity: SuggestionSeverity::Warning,
        });
    }

    // --- No impact report (packet missing) --------------------------------
    if ledger_status.no_impact_report {
        suggestions.push(Suggestion {
            id: "missing-impact-scan".to_string(),
            description: "No impact report available — run a scan with impact analysis".to_string(),
            command: "ledgerful scan --impact".to_string(),
            severity: SuggestionSeverity::ActionRequired,
        });
    }

    // --- Prediction warnings ----------------------------------------------
    if !report.prediction_warnings.is_empty() {
        suggestions.push(Suggestion {
            id: "prediction-warnings-explain".to_string(),
            description: "Prediction warnings present — review rationale and re-assess impact"
                .to_string(),
            command: concat!("ledgerful verify --explain\n", "ledgerful impact --summary")
                .to_string(),
            severity: SuggestionSeverity::Info,
        });
    }

    // Sort: severity descending (ActionRequired first), then description ascending
    suggestions.sort_by(|a, b| {
        b.severity
            .cmp(&a.severity)
            .then_with(|| a.description.cmp(&b.description))
    });

    suggestions
}

/// Generate health-related suggestions even on a clean verify pass.
///
/// This is intended for use with the `--health` flag.
pub fn generate_health_suggestions(ledger_status: &LedgerStatus) -> Vec<Suggestion> {
    let mut suggestions: Vec<Suggestion> = Vec::new();

    if ledger_status.has_stale_pending {
        suggestions.push(Suggestion {
            id: "stale-pending-status".to_string(),
            description: "Stale PENDING transaction(s) (>24h) — inspect then commit or rollback"
                .to_string(),
            command: concat!(
                "ledgerful ledger status   # list pending transactions\n",
                "# then:\n",
                "#   ledgerful ledger commit <tx-id> --summary \"...\"\n",
                "#   ledgerful ledger rollback <tx-id> --reason \"stale\""
            )
            .to_string(),
            severity: SuggestionSeverity::Warning,
        });
    }

    if ledger_status.unaudited_count > 0 {
        suggestions.push(Suggestion {
            id: "unaudited-drift-reconcile".to_string(),
            description: format!(
                "{} UNAUDITED drift transaction(s) detected — reconcile the ledger",
                ledger_status.unaudited_count
            ),
            command: "ledgerful ledger reconcile --all --reason \"verify follow-up\"".to_string(),
            severity: SuggestionSeverity::ActionRequired,
        });
    }

    // Sort: severity descending, then description ascending
    suggestions.sort_by(|a, b| {
        b.severity
            .cmp(&a.severity)
            .then_with(|| a.description.cmp(&b.description))
    });

    suggestions
}

/// Query the ledger database for the status snapshot needed by the suggestion engine.
pub fn query_ledger_status(layout: &Layout) -> LedgerStatus {
    let db_path = layout.state_subdir().join("ledger.db");
    let storage = match StorageManager::init(db_path.as_std_path()) {
        Ok(s) => s,
        Err(_) => {
            // No db means no packets — flag it
            return LedgerStatus {
                no_impact_report: true,
                ..Default::default()
            };
        }
    };

    let conn = storage.get_connection();
    let ledger = crate::ledger::db::LedgerDb::new(conn);

    let unaudited_count = ledger.get_all_unaudited().map(|v| v.len()).unwrap_or(0);

    let has_stale_pending = ledger
        .get_all_pending()
        .map(|pending| {
            pending.iter().any(|tx| {
                // Check if pending since > 24h
                is_stale_pending(&tx.started_at)
            })
        })
        .unwrap_or(false);

    let no_impact_report = storage.get_latest_packet().ok().flatten().is_none();

    LedgerStatus {
        unaudited_count,
        has_stale_pending,
        no_impact_report,
    }
}

/// Returns true when the given ISO-8601 start timestamp is > 24 hours ago.
fn is_stale_pending(started_at: &str) -> bool {
    use chrono::{DateTime, Duration, Utc};
    let Ok(start) = DateTime::parse_from_rfc3339(started_at) else {
        return false;
    };
    let now = Utc::now();
    let since = now.signed_duration_since(start.with_timezone(&Utc));
    since > Duration::hours(24)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::verify::results::{VerificationReport, VerificationResult};

    // ------------------------------------------------------------------
    // Helpers
    // ------------------------------------------------------------------

    fn clean_report() -> VerificationReport {
        VerificationReport::new(None, vec![])
    }

    fn report_with_results(results: Vec<VerificationResult>) -> VerificationReport {
        VerificationReport::new(None, results)
    }

    fn clean_ledger() -> LedgerStatus {
        LedgerStatus::default()
    }

    fn unaudited_ledger(count: usize) -> LedgerStatus {
        LedgerStatus {
            unaudited_count: count,
            ..LedgerStatus::default()
        }
    }

    fn stale_ledger() -> LedgerStatus {
        LedgerStatus {
            has_stale_pending: true,
            ..LedgerStatus::default()
        }
    }

    fn no_packet_ledger() -> LedgerStatus {
        LedgerStatus {
            no_impact_report: true,
            ..LedgerStatus::default()
        }
    }

    fn failing_result() -> VerificationResult {
        VerificationResult {
            command: "cargo test".to_string(),
            exit_code: 1,
            duration_ms: 100,
            stdout_summary: "FAILED".to_string(),
            stderr_summary: "error".to_string(),
            truncated: false,
            timestamp: "2026-01-01T00:00:00Z".to_string(),
        }
    }

    fn passing_result() -> VerificationResult {
        VerificationResult {
            command: "cargo test".to_string(),
            exit_code: 0,
            duration_ms: 100,
            stdout_summary: "ok".to_string(),
            stderr_summary: "".to_string(),
            truncated: false,
            timestamp: "2026-01-01T00:00:00Z".to_string(),
        }
    }

    // ------------------------------------------------------------------
    // Failure-pattern tests
    // ------------------------------------------------------------------

    #[test]
    fn test_suggestion_unaudited_drift() {
        let report = clean_report();
        let ledger = unaudited_ledger(3);
        let suggestions = generate_suggestions(&report, &ledger);

        let reconcile = suggestions
            .iter()
            .find(|s| s.id == "unaudited-drift-reconcile")
            .expect("should emit unaudited-drift-reconcile suggestion");
        assert_eq!(reconcile.severity, SuggestionSeverity::ActionRequired);
        assert!(reconcile.command.contains("ledger reconcile"));
    }

    #[test]
    fn test_suggestion_stale_pending() {
        let report = clean_report();
        let ledger = stale_ledger();
        let suggestions = generate_suggestions(&report, &ledger);

        let stale = suggestions
            .iter()
            .find(|s| s.id == "stale-pending-status")
            .expect("should emit stale-pending-status suggestion");
        assert_eq!(stale.severity, SuggestionSeverity::Warning);
        assert!(stale.command.contains("ledger status"));
    }

    #[test]
    fn test_suggestion_verify_failure() {
        let report = report_with_results(vec![failing_result()]);
        let ledger = clean_ledger();
        let suggestions = generate_suggestions(&report, &ledger);

        let verify = suggestions
            .iter()
            .find(|s| s.id == "verify-failure-impact")
            .expect("should emit verify-failure-impact suggestion");
        assert_eq!(verify.severity, SuggestionSeverity::Warning);
        assert!(verify.command.contains("impact --summary"));
    }

    #[test]
    fn test_suggestion_prediction_warnings() {
        let mut report = clean_report();
        report.prediction_warnings = vec!["Semantic prediction degraded".to_string()];
        let ledger = clean_ledger();
        let suggestions = generate_suggestions(&report, &ledger);

        let pw = suggestions
            .iter()
            .find(|s| s.id == "prediction-warnings-explain")
            .expect("should emit prediction-warnings-explain suggestion");
        assert_eq!(pw.severity, SuggestionSeverity::Info);
        assert!(pw.command.contains("verify --explain"));
        assert!(pw.command.contains("impact --summary"));
    }

    #[test]
    fn test_suggestion_missing_impact_report() {
        let report = clean_report();
        let ledger = no_packet_ledger();
        let suggestions = generate_suggestions(&report, &ledger);

        let missing = suggestions
            .iter()
            .find(|s| s.id == "missing-impact-scan")
            .expect("should emit missing-impact-scan suggestion");
        assert_eq!(missing.severity, SuggestionSeverity::ActionRequired);
        assert!(missing.command.contains("scan --impact"));
    }

    // ------------------------------------------------------------------
    // Clean pass
    // ------------------------------------------------------------------

    #[test]
    fn test_no_suggestions_on_clean_pass() {
        let report = report_with_results(vec![passing_result()]);
        let ledger = clean_ledger();
        let suggestions = generate_suggestions(&report, &ledger);
        assert!(
            suggestions.is_empty(),
            "expected empty suggestions on clean pass, got {suggestions:?}"
        );
    }

    // ------------------------------------------------------------------
    // Health suggestions
    // ------------------------------------------------------------------

    #[test]
    fn test_health_suggestions_on_clean_pass() {
        let ledger = stale_ledger();
        let health = generate_health_suggestions(&ledger);
        assert!(!health.is_empty(), "health mode should surface warnings");
        assert!(
            health.iter().any(|s| s.id == "stale-pending-status"),
            "should include stale-pending-status"
        );
    }

    #[test]
    fn test_health_suggestions_no_issues() {
        let ledger = clean_ledger();
        let health = generate_health_suggestions(&ledger);
        assert!(health.is_empty());
    }

    // ------------------------------------------------------------------
    // Property-based safety invariants
    // ------------------------------------------------------------------

    #[test]
    fn test_no_force_in_suggestions() {
        let ledger = LedgerStatus {
            unaudited_count: 5,
            has_stale_pending: true,
            no_impact_report: true,
        };
        let mut report_with_failures = report_with_results(vec![failing_result()]);
        report_with_failures.prediction_warnings = vec!["warning".to_string()];

        let suggestions = generate_suggestions(&report_with_failures, &ledger);

        for s in &suggestions {
            assert!(
                !s.command.contains("--force"),
                "suggestion {} contains --force: {}",
                s.id,
                s.command
            );
        }

        // Also check health suggestions
        let health = generate_health_suggestions(&ledger);
        for s in &health {
            assert!(
                !s.command.contains("--force"),
                "health suggestion {} contains --force: {}",
                s.id,
                s.command
            );
        }
    }

    #[test]
    fn test_no_empty_commands() {
        let ledger = LedgerStatus {
            unaudited_count: 5,
            has_stale_pending: true,
            no_impact_report: true,
        };
        let mut report_with_failures = report_with_results(vec![failing_result()]);
        report_with_failures.prediction_warnings = vec!["warning".to_string()];

        let suggestions = generate_suggestions(&report_with_failures, &ledger);

        for s in &suggestions {
            assert!(
                !s.command.is_empty(),
                "suggestion {} has empty command",
                s.id
            );
        }

        let health = generate_health_suggestions(&ledger);
        for s in &health {
            assert!(
                !s.command.is_empty(),
                "health suggestion {} has empty command",
                s.id
            );
        }
    }

    #[test]
    fn test_deterministic_sorting() {
        let ledger = LedgerStatus {
            unaudited_count: 3,
            has_stale_pending: true,
            no_impact_report: true,
        };
        let mut report_with_failures = report_with_results(vec![failing_result()]);
        report_with_failures.prediction_warnings = vec!["w".to_string()];

        // Generate multiple times and verify identical output
        let first = generate_suggestions(&report_with_failures, &ledger);
        for _ in 0..10 {
            let again = generate_suggestions(&report_with_failures, &ledger);
            assert_eq!(first, again, "suggestions are not deterministically sorted");
        }

        // Verify sort order: ActionRequired < Warning < Info (i.e. ActionRequired comes first)
        // After sorting, severity should be non-increasing
        let severities: Vec<SuggestionSeverity> = first.iter().map(|s| s.severity).collect();
        for window in severities.windows(2) {
            // Later element must not be more severe than earlier element
            assert!(
                window[0] >= window[1],
                "sort order violated: {:?} before {:?}",
                window[0],
                window[1]
            );
        }
    }

    // ------------------------------------------------------------------
    // Combined scenarios
    // ------------------------------------------------------------------

    #[test]
    fn test_multiple_suggestions_combined() {
        let mut report = report_with_results(vec![failing_result()]);
        report.prediction_warnings = vec!["degraded".to_string()];
        let ledger = LedgerStatus {
            unaudited_count: 2,
            has_stale_pending: true,
            no_impact_report: true,
        };

        let suggestions = generate_suggestions(&report, &ledger);

        // Should have 5 unique suggestions
        let ids: Vec<&str> = suggestions.iter().map(|s| s.id.as_str()).collect();
        assert!(ids.contains(&"unaudited-drift-reconcile"));
        assert!(ids.contains(&"stale-pending-status"));
        assert!(ids.contains(&"verify-failure-impact"));
        assert!(ids.contains(&"missing-impact-scan"));
        assert!(ids.contains(&"prediction-warnings-explain"));
    }

    #[test]
    fn test_verify_failure_not_emitted_on_clean_steps() {
        // All steps pass, but ledger has issues — verify-failure-impact should NOT appear
        let report = report_with_results(vec![passing_result(), passing_result()]);
        let ledger = LedgerStatus {
            unaudited_count: 5,
            ..LedgerStatus::default()
        };

        let suggestions = generate_suggestions(&report, &ledger);
        assert!(
            !suggestions.iter().any(|s| s.id == "verify-failure-impact"),
            "should not emit verify-failure-impact on clean steps"
        );
        // But unasudited drift should still be there
        assert!(
            suggestions
                .iter()
                .any(|s| s.id == "unaudited-drift-reconcile")
        );
    }

    #[test]
    fn test_no_prediction_warnings_empty_vec() {
        // Ensure we don't emit prediction-warnings suggestion when warnings vec is empty
        let mut report = clean_report();
        report.prediction_warnings = vec![];
        let ledger = clean_ledger();
        let suggestions = generate_suggestions(&report, &ledger);
        assert!(
            !suggestions
                .iter()
                .any(|s| s.id == "prediction-warnings-explain"),
            "should not emit prediction warnings for empty vec"
        );
    }
}
