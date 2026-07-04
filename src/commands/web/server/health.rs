//! Health-score computation for the project list endpoint.

use crate::state::layout::Layout;
use crate::state::reports::{LATEST_IMPACT_REPORT, read_latest_impact_report};

/// Compute a health score from the latest impact report and doctor state.
///
/// Formula: `100 - (risk_penalty) - (doctor_failures * 20)`, clamped to `[0, 100]`.
///
/// Inputs:
/// - `risk_penalty` (0..=60): derived from the impact report's top-level
///   `riskLevel`. Mapping: `"high" -> 60`, `"medium" -> 30`, `"low" -> 5`.
///   A **missing or unparseable** impact report is treated as a 40-point
///   penalty (M8 opencode-review M1): an unintelligible risk signal is
///   a real signal, not a clean bill of health. 40 yields
///   `health_score = 60` and `status = "warning"` — visible to the
///   dashboard, not green. This is the spec-mandated signal at
///   `conductor/trackM8/spec.md:55` ("from the latest impact report's
///   `riskLevel`"). The earlier `high_risk_files * 10` formulation was
///   abandoned because the `changes[]` array does not carry a per-file
///   `risk` field — the only available risk signal is the top-level
///   `riskLevel` enum, so we map that enum directly to a single penalty.
/// - `doctor_failures`: count of failed checks in the most recent
///   `ledgerful doctor` run, read from
///   `layout.state_subdir().join("doctor-results.json")` (written by
///   `execute_doctor` in `src/commands/doctor.rs`, per M8 opencode-review
///   H1). Returns 0 if the file is absent (`Err(NotFound)`); logs a
///   warning if the file is present but unparseable, so a future schema
///   rename is loud not silent.
///
/// The function returns `(health_score, last_scan_at)` where
/// `last_scan_at` is the `timestampUtc` of the most recent impact
/// report, or `None` if no report has been written yet.
pub(crate) fn compute_health_score(layout: &Layout) -> (u8, Option<String>) {
    let report_path = layout.reports_dir().join(LATEST_IMPACT_REPORT);
    let (risk_penalty, last_scan_at) = match read_latest_impact_report(layout) {
        Ok(Some(crate::state::reports::LatestImpactReport::Packet(packet))) => {
            let last_scan_at = Some(packet.timestamp_utc);
            let risk_penalty = match packet.risk_level {
                crate::impact::packet::RiskLevel::High => 60,
                crate::impact::packet::RiskLevel::Medium => 30,
                crate::impact::packet::RiskLevel::Low => 5,
            };
            (risk_penalty, last_scan_at)
        }
        // A clean-tree tombstone is a known-good state, not an unknown report:
        // there is nothing risky to penalize for.
        Ok(Some(crate::state::reports::LatestImpactReport::CleanTree(tombstone))) => {
            (0, Some(tombstone.timestamp_utc))
        }
        Ok(None) => {
            tracing::warn!(
                "No impact report at {}; applying 40-point missing-report penalty",
                report_path
            );
            (40, None)
        }
        Err(e) => {
            tracing::warn!(
                "Failed to read/deserialize impact report at {}: {}; \
                 applying 40-point unknown-report penalty",
                report_path,
                e
            );
            (40, None)
        }
    };

    let doctor_failures = read_doctor_failures(layout);

    let raw = 100i64 - risk_penalty - (doctor_failures as i64 * 20);
    let score = raw.clamp(0, 100) as u8;

    (score, last_scan_at)
}

/// Read `doctor_failures` from the most recent `doctor-results.json`.
///
/// File schema (per `conductor/trackM8/spec.md` DoD + the M8 review H1
/// recommendation in `output/m8-opencode-1.md`): a JSON object with a
/// `failures: u64` field written by `execute_doctor` in
/// `src/commands/doctor.rs`. The legacy `results: [{ passed: bool, ... }]`
/// array shape is also accepted for forward-compat (count of items with
/// `passed == false`) — the schema was provisional before the M8
/// resolve. The file lives in `state_subdir()` (the same directory as
/// `ledger.db`); a `NotFound` error returns 0, and a parse failure logs
/// a warning so a future schema rename surfaces instead of silently
/// reporting "all green".
fn read_doctor_failures(layout: &Layout) -> u64 {
    let path = layout.state_subdir().join("doctor-results.json");
    match std::fs::read_to_string(&path) {
        Ok(contents) => match serde_json::from_str::<serde_json::Value>(&contents) {
            Ok(json) => {
                if let Some(n) = json.get("failures").and_then(|v| v.as_u64()) {
                    return n;
                }
                if let Some(arr) = json.get("results").and_then(|v| v.as_array()) {
                    return arr
                        .iter()
                        .filter(|r| r.get("passed").and_then(|p| p.as_bool()) == Some(false))
                        .count() as u64;
                }
                tracing::warn!(
                    "doctor-results.json at {} has no `failures` or `results` field; \
                     defaulting to 0",
                    path
                );
                0
            }
            Err(e) => {
                tracing::warn!(
                    "Failed to parse doctor-results.json at {}: {}; defaulting to 0",
                    path,
                    e
                );
                0
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => 0,
        Err(e) => {
            tracing::warn!(
                "Failed to read doctor-results.json at {}: {}; defaulting to 0",
                path,
                e
            );
            0
        }
    }
}

/// Map a health score (0-100) to a project status string.
///
/// Thresholds (documented per `conductor/trackM8/spec.md:55`):
/// - `>= 80` -> `"healthy"`
/// - `>= 50` -> `"warning"`
/// - `<  50` -> `"critical"`
///
/// Rationale: a score of 100 is the "no signals" baseline; any
/// non-zero penalty moves the project out of "healthy". A score of
/// 50 is the half-way mark and signals the dashboard should warn the
/// user that the project is at risk. Anything below 50 is a critical
/// state.
pub(crate) fn project_status_from_score(score: u8) -> &'static str {
    if score >= 80 {
        "healthy"
    } else if score >= 50 {
        "warning"
    } else {
        "critical"
    }
}
