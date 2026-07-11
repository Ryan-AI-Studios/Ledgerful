//! Shared web DTOs and helpers used by both `server` and `api` modules.

#![allow(dead_code)]

use crate::commands::web::git_meta::lookup_git_meta;
use crate::impact::packet::Hotspot;
use crate::ledger::types::LedgerEntry;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[cfg(any(test, feature = "openapi", feature = "web"))]
use utoipa::{IntoParams, ToSchema};

#[derive(Debug, serde::Serialize)]
#[cfg_attr(any(test, feature = "openapi", feature = "web"), derive(ToSchema))]
#[serde(rename_all = "camelCase")]
pub(crate) struct UserSession {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) email: String,
    pub(crate) role: String,
}

#[derive(Serialize)]
#[cfg_attr(any(test, feature = "openapi", feature = "web"), derive(ToSchema))]
pub(crate) struct SnapshotResponse {
    pub(crate) project_id: String,
    pub(crate) overall_risk: String,
    pub(crate) pending_transactions: usize,
    pub(crate) unaudited_drift: usize,
    pub(crate) indexed_documents: usize,
    pub(crate) graph_nodes: usize,
    pub(crate) graph_edges: usize,
    pub(crate) last_audit: Option<String>,
    pub(crate) top_hotspots: Vec<HotspotResponse>,
    pub(crate) recent_changes: Vec<ChangeResponse>,
}

#[derive(Serialize)]
#[cfg_attr(any(test, feature = "openapi", feature = "web"), derive(ToSchema))]
pub(crate) struct StatusResponse {
    pub(crate) index_ready: bool,
    pub(crate) graph_ready: bool,
    pub(crate) pending_transactions: usize,
    pub(crate) unaudited_drift: usize,
    pub(crate) embedding_model_reachable: bool,
    pub(crate) completion_model_reachable: bool,
    /// `true` when the repo was created by `ledgerful demo` (DEMO_MARKER
    /// present). The dashboard renders a DEMO banner so synthetic repos
    /// self-identify in the UI. Additive; non-demo repos serialize `false`.
    #[serde(skip_serializing_if = "is_false")]
    pub(crate) is_demo: bool,
}

fn is_false(v: &bool) -> bool {
    !v
}

#[derive(Serialize)]
#[cfg_attr(any(test, feature = "openapi", feature = "web"), derive(ToSchema))]
pub(crate) struct ProjectResponse {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) path: String,
    pub(crate) status: String,
    pub(crate) last_scan_at: Option<String>,
    pub(crate) health_score: u8,
    /// TA31 R1: per-sibling validation warnings (e.g. an empty ledger
    /// `entity`). Always present (empty for the local/root project and
    /// for fully-valid siblings) so the frontend can render a "needs
    /// attention" badge without an `Option` round-trip. This is
    /// additive/non-breaking relative to the pre-TA31 DTO shape.
    pub(crate) validation_warnings: Vec<String>,
}

/// `SyncStatusResponse` DTO — local M0 sync state.
///
/// Deliberately **not** gated on `#[cfg(feature = "sync")]` so the OpenAPI
/// schema can document the route in all builds (schema/runtime consistency,
/// track 0013 DoD-1). Only the sync-specific *logic* (reading `SyncState`
/// from the ledger DB) is feature-gated; the DTO and the route are always
/// present. When built without `sync`, the handler returns a clean
/// `501 Not Implemented` (see `sync_status_handler`).
#[derive(Serialize)]
#[cfg_attr(any(test, feature = "openapi", feature = "web"), derive(ToSchema))]
pub(crate) struct SyncStatusResponse {
    pub(crate) device_id: Option<String>,
    pub(crate) last_extract_at: Option<String>,
    pub(crate) last_apply_at: Option<String>,
    pub(crate) last_run_at: Option<String>,
}

/// TA31 R1 frontend-safety fallback: an empty `entity` (the AI-Brains
/// real-world case — a federated ledger entry with no entity recorded)
/// must not render as blank/`undefined` in the dashboard. This is
/// applied only at JSON-serialization time in API DTOs; it does NOT
/// change what is stored in SQLite or in schema.json on disk.
pub(crate) fn display_entity(entity: &str) -> String {
    if entity.trim().is_empty() {
        "(uncategorized)".to_string()
    } else {
        entity.to_string()
    }
}

#[derive(Serialize)]
#[cfg_attr(any(test, feature = "openapi", feature = "web"), derive(ToSchema))]
pub(crate) struct LedgerEntryResponse {
    pub(crate) id: i64,
    pub(crate) tx_id: String,
    pub(crate) category: String,
    pub(crate) entry_type: String,
    pub(crate) entity: String,
    pub(crate) entity_normalized: String,
    pub(crate) change_type: String,
    pub(crate) summary: String,
    pub(crate) reason: String,
    pub(crate) is_breaking: bool,
    pub(crate) committed_at: String,
    pub(crate) verification_status: Option<String>,
    pub(crate) verification_basis: Option<String>,
    pub(crate) outcome_notes: Option<String>,
    pub(crate) origin: String,
    pub(crate) trace_id: Option<String>,
    pub(crate) signature: Option<String>,
    pub(crate) public_key: Option<String>,
    pub(crate) risk: Option<String>,
    pub(crate) related_tickets: Option<String>,
    pub(crate) author: String,
}

impl From<LedgerEntry> for LedgerEntryResponse {
    fn from(entry: LedgerEntry) -> Self {
        Self {
            id: entry.id,
            tx_id: entry.tx_id,
            category: entry.category.to_string(),
            entry_type: format!("{:?}", entry.entry_type),
            entity: display_entity(&entry.entity),
            entity_normalized: entry.entity_normalized,
            change_type: format!("{:?}", entry.change_type),
            summary: entry.summary,
            reason: entry.reason,
            is_breaking: entry.is_breaking,
            committed_at: entry.committed_at,
            verification_status: entry.verification_status.map(|s| format!("{:?}", s)),
            verification_basis: entry.verification_basis.map(|b| format!("{:?}", b)),
            outcome_notes: entry.outcome_notes,
            origin: entry.origin,
            trace_id: entry.trace_id,
            signature: entry.signature,
            public_key: entry.public_key,
            risk: entry.risk,
            related_tickets: entry.related_tickets,
            author: entry.author,
        }
    }
}

/// Detail response for a single ledger transaction, including enriched fields.
#[derive(Serialize)]
#[cfg_attr(any(test, feature = "openapi", feature = "web"), derive(ToSchema))]
pub(crate) struct LedgerDetailResponse {
    #[serde(flatten)]
    pub(crate) base: LedgerEntryResponse,
    pub(crate) files: Vec<ChangedFileResponse>,
    pub(crate) hotspots_crossed: usize,
    pub(crate) tests_run: usize,
    pub(crate) flakes: usize,
}

#[derive(Serialize)]
#[cfg_attr(any(test, feature = "openapi", feature = "web"), derive(ToSchema))]
pub(crate) struct ChangedFileResponse {
    pub(crate) path: String,
    /// `None` for pre-m48 legacy rows or binary files whose stats are
    /// unavailable.  Serialized as `null` so the frontend can distinguish
    /// "unknown" from a real zero.
    pub(crate) additions: Option<i64>,
    /// See `additions`.
    pub(crate) deletions: Option<i64>,
    /// `true` when the file was detected as binary by git numstat (`-\t-`).
    /// Lets the frontend render "binary" instead of the generic "—" used
    /// for pre-m48 legacy rows.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(crate) is_binary: bool,
}

#[derive(Debug, Deserialize, Default)]
#[cfg_attr(
    any(test, feature = "openapi", feature = "web"),
    derive(IntoParams, ToSchema)
)]
pub(crate) struct LedgerListQuery {
    pub(crate) category: Option<String>,
    pub(crate) limit: Option<usize>,
    pub(crate) offset: Option<usize>,
}

#[derive(Debug, Deserialize, Default)]
#[cfg_attr(
    any(test, feature = "openapi", feature = "web"),
    derive(IntoParams, ToSchema)
)]
pub(crate) struct LedgerSearchQuery {
    pub(crate) q: Option<String>,
    pub(crate) days: Option<u64>,
    pub(crate) limit: Option<usize>,
    pub(crate) offset: Option<usize>,
}

#[derive(Debug, Deserialize, Default)]
#[cfg_attr(
    any(test, feature = "openapi", feature = "web"),
    derive(IntoParams, ToSchema)
)]
pub(crate) struct ChangesQuery {
    pub(crate) days: Option<u64>,
    pub(crate) working_tree: Option<bool>,
}

#[derive(Debug, Serialize)]
#[cfg_attr(any(test, feature = "openapi", feature = "web"), derive(ToSchema))]
#[serde(rename_all = "camelCase")]
pub(crate) struct ChangeResponse {
    pub(crate) id: String,
    pub(crate) path: String,
    pub(crate) status: String,
    pub(crate) summary: String,
    pub(crate) author: String,
    pub(crate) time_ago: String,
    pub(crate) file_count: usize,
    pub(crate) additions: usize,
    pub(crate) deletions: usize,
    pub(crate) risk: String,
}

#[derive(Debug, Deserialize, Default)]
#[cfg_attr(
    any(test, feature = "openapi", feature = "web"),
    derive(IntoParams, ToSchema)
)]
pub(crate) struct HotspotsQueryParams {
    pub(crate) limit: Option<usize>,
    pub(crate) days: Option<u64>,
}

#[derive(Serialize, Debug, Clone)]
#[cfg_attr(any(test, feature = "openapi", feature = "web"), derive(ToSchema))]
pub(crate) struct ConfigResponse {
    pub(crate) project: String,
    pub(crate) repo_path: String,
    pub(crate) ledger_path: String,
    pub(crate) graph_path: String,
    pub(crate) signing_key: String,
    pub(crate) llm_backend: String,
    pub(crate) polling_interval: String,
    pub(crate) telemetry: String,
    pub(crate) version: String,
}

/// Frontend-facing hotspot DTO decoupled from the internal `Hotspot` domain
/// struct. The internal struct (`intelligence::Hotspot`) is coupled to the
/// impact math module; this DTO gives the API a stable contract that the
/// frontend can rely on even if the internal struct changes.
///
/// Backward-compat fields (`displayScore`, `score`, `complexity`, `frequency`,
/// `centrality`) are included temporarily until the frontend PR merges. They
/// will be removed once the frontend consumes the new fields directly.
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(any(test, feature = "openapi", feature = "web"), derive(ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct HotspotResponse {
    // Frontend-facing fields (the contract):
    pub id: String,
    pub file_path: String,
    pub risk_level: String,
    pub risk_score: f32,
    pub last_touched_at: Option<String>,
    pub contributor: Option<String>,
    pub change_count: u32,
    pub rank: usize,

    // Backward-compat fields (remove after frontend PR merges):
    pub display_score: f32,
    pub score: f32,
    pub complexity: i32,
    pub frequency: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub centrality: Option<usize>,
}

/// Derive a human-readable risk level from `display_score`.
///
/// `display_score` is `ln(1 + score * 1000)` (log scale, typically 0-5), so
/// the thresholds use the log scale, NOT a 0-100 percentage:
/// - `>= 4.0` → `"CRITICAL"` (≈ raw 0.054)
/// - `>= 3.0` → `"HIGH"` (≈ raw 0.020)
/// - `>= 2.0` → `"MEDIUM"` (≈ raw 0.0072)
/// - `< 2.0`  → `"LOW"`
pub fn risk_level_from_display_score(display_score: f32) -> String {
    if display_score >= 4.0 {
        "CRITICAL".to_string()
    } else if display_score >= 3.0 {
        "HIGH".to_string()
    } else if display_score >= 2.0 {
        "MEDIUM".to_string()
    } else {
        "LOW".to_string()
    }
}

/// Map internal `Hotspot` structs to `HotspotResponse` DTOs, enriching each
/// with git metadata (`last_touched_at`, `contributor`) from the provided map.
///
/// The input `hotspots` must already be sorted by `display_score` descending
/// (the order produced by `calculate_hotspots`). `rank` is the 1-based index.
/// `change_count` is `frequency.round().max(1.0) as u32` — floor at 1 so any
/// file in the hotspots list shows at least 1 change.
pub fn map_hotspots_to_responses(
    hotspots: &[Hotspot],
    git_meta: &HashMap<String, (String, String)>,
) -> Vec<HotspotResponse> {
    hotspots
        .iter()
        .enumerate()
        .map(|(idx, h)| {
            let path_str = h.path.to_string_lossy().to_string();
            let (last_touched_at, contributor) = match lookup_git_meta(git_meta, &path_str) {
                Some((ts, author)) => (Some(ts.clone()), Some(author.clone())),
                None => (None, None),
            };
            let change_count = (h.frequency.max(1.0)).round() as u32;
            HotspotResponse {
                id: path_str.clone(),
                file_path: path_str,
                risk_level: risk_level_from_display_score(h.display_score),
                risk_score: h.display_score,
                last_touched_at,
                contributor,
                change_count,
                rank: idx + 1,
                display_score: h.display_score,
                score: h.score,
                complexity: h.complexity,
                frequency: h.frequency,
                centrality: h.centrality,
            }
        })
        .collect()
}

/// `GET /api/verify/health` — overall verification health derived from the
/// latest `verification_runs` row.
///
/// Status mapping (documented for the reviewer):
/// - No runs recorded → `DEGRADED` with `message = "No verification runs
///   recorded"` and `lastRunAt = null`. This is NOT an error (200, not 404)
///   so the dashboard can render its empty state.
/// - Latest run `overall_pass = false` → `FAILING`.
/// - Latest run `overall_pass = true` but older than 7 days → `DEGRADED`
///   with `message = "Last verification run is stale (older than 7 days)"`.
///   (A stale failing run stays `FAILING` — failure takes precedence over
///   staleness.)
/// - Otherwise → `HEALTHY`.
#[derive(Debug, Serialize)]
#[cfg_attr(any(test, feature = "openapi", feature = "web"), derive(ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct VerificationHealthResponse {
    pub status: String,
    // NOTE: `last_run_at` is intentionally NOT `skip_serializing_if`-gated —
    // the frontend contract is `lastRunAt: string | null`, so the field MUST
    // be present in the JSON as `null` when there are no verification runs
    // (the dashboard empty state), not omitted.
    pub last_run_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
#[cfg_attr(
    any(test, feature = "openapi", feature = "web"),
    derive(IntoParams, ToSchema)
)]
pub struct VerifyHistoryQuery {
    pub(crate) days: Option<u64>,
}

#[derive(Debug, Serialize)]
#[cfg_attr(any(test, feature = "openapi", feature = "web"), derive(ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct VerificationTrendPoint {
    pub date: String,
    pub passed: u64,
    pub failed: u64,
}

/// `GET /api/verify/steps` — per-step aggregates across all history.
///
/// Returns a bare JSON array of `{ id, name, lastRunAt, averageDurationMs,
/// passRatePercent, recentFailures }` sorted ascending by step command.
///
/// `id` is the step's stable identifier (its `command` — the verify plan step
/// `src/verify/plan.rs::VerificationStep` has no separate id field, and
/// `verification_results` only stores `command`). `name` is the friendly
/// label: the step's `description` from the most recent `verification_runs.
/// plan_json` that contains a step with that command, falling back to
/// `command` when no plan_json is available, fails to parse, or has no
/// matching step. `plan_json` is a serialized `VerificationPlan` (see
/// `verify::engine::persist_verify_report`); parse errors on any single row
/// are skipped so one malformed run cannot fail the whole endpoint.
///
/// `recentFailures` counts failures within the last 10 verification runs
/// (by `verification_runs.id DESC`). Flagged as an assumption.
#[derive(Debug, Serialize)]
#[cfg_attr(any(test, feature = "openapi", feature = "web"), derive(ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct VerificationStepResponse {
    pub id: String,
    pub name: String,
    pub last_run_at: String,
    pub average_duration_ms: f64,
    pub pass_rate_percent: f64,
    pub recent_failures: u64,
}

#[derive(Debug, Deserialize, Default)]
#[cfg_attr(
    any(test, feature = "openapi", feature = "web"),
    derive(IntoParams, ToSchema)
)]
pub struct HotspotTrendQuery {
    pub(crate) days: Option<u64>,
    pub(crate) limit: Option<usize>,
}

#[derive(Serialize)]
#[cfg_attr(any(test, feature = "openapi", feature = "web"), derive(ToSchema))]
pub struct HotspotTrendResponse {
    pub labels: Vec<String>,
    pub series: Vec<HotspotTrendSeries>,
    pub truncated: bool,
}

#[derive(Serialize)]
#[cfg_attr(any(test, feature = "openapi", feature = "web"), derive(ToSchema))]
pub struct HotspotTrendSeries {
    pub path: String,
    pub scores: Vec<f32>,
}

#[derive(Serialize)]
#[cfg_attr(any(test, feature = "openapi", feature = "web"), derive(ToSchema))]
pub struct SecurityBoundariesResponse {
    pub meta: serde_json::Value,
    pub boundaries: serde_json::Value,
}

#[derive(Debug, Deserialize, Default)]
#[cfg_attr(
    any(test, feature = "openapi", feature = "web"),
    derive(IntoParams, ToSchema)
)]
pub struct TrendsQuery {
    pub(crate) days: Option<u64>,
}

#[derive(Debug, Serialize, Clone)]
#[cfg_attr(any(test, feature = "openapi", feature = "web"), derive(ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct TrendPointDto {
    pub(crate) date: String,
    pub(crate) score: f64,
    pub(crate) changes: i64,
    pub(crate) high_risk_count: i64,
}

#[derive(Debug, Serialize, Clone)]
#[cfg_attr(any(test, feature = "openapi", feature = "web"), derive(ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct TrendsResponse {
    pub(crate) data: Vec<TrendPointDto>,
}

#[derive(Debug, Deserialize, Default)]
#[cfg_attr(
    any(test, feature = "openapi", feature = "web"),
    derive(IntoParams, ToSchema)
)]
pub struct KnowledgeGraphQuery {
    pub(crate) limit: Option<usize>,
    pub(crate) focus: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[cfg_attr(any(test, feature = "openapi", feature = "web"), derive(ToSchema))]
pub struct KnowledgeGraphResponse {
    pub nodes: Vec<KgNode>,
    pub edges: Vec<KgEdge>,
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize)]
#[cfg_attr(any(test, feature = "openapi", feature = "web"), derive(ToSchema))]
pub struct KgNode {
    pub id: String,
    pub label: String,
    pub category: String,
    pub risk_score: f64,
    pub file_path: String,
    pub complexity: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize)]
#[cfg_attr(any(test, feature = "openapi", feature = "web"), derive(ToSchema))]
pub struct KgEdge {
    pub source: String,
    pub target: String,
    pub relation: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provenance_id: Option<String>,
}

/// `GET /api/compliance/summary` — aggregate cryptographic validity and audit
/// completion rates over the ledger, plus the recent hotspot delta.
///
/// Response (camelCase):
/// - `totalSigned`: count of ledger entries with BOTH `signature` and
///   `public_key` present (i.e. signed rows, regardless of whether the
///   signature currently verifies).
/// - `validityPercent`: `valid_count / total_entries * 100.0` rounded to 2dp,
///   where `valid_count` is the number of entries whose signature verifies
///   against the stored public key (or, for unsigned rows when
///   `intent.require_signing` is false, the row is NOT counted as valid —
///   only cryptographically VALID rows contribute). `0.0` when there are no
///   entries.
/// - `lastAuditAt`: the most recent `committed_at` among entries classified
///   VALID (the last verifiably-signed event). `null` when there are no
///   VALID entries / no entries. This field is intentionally NOT
///   `skip_serializing_if`-gated — the frontend contract is
///   `lastAuditAt: string | null`, so it MUST serialize as `null` in the
///   empty state (matches E1's `lastRunAt` contract).
/// - `hotspotDeltaPercent`: see `fetch_hotspot_delta_percent`.
///
/// No-DB path (ledger.db absent): returns
/// `{ totalSigned: 0, validityPercent: 0.0, lastAuditAt: null,
/// hotspotDeltaPercent: 0.0 }` with HTTP 200 — the dashboard empty state,
/// NOT an error.
#[derive(Debug, Serialize)]
#[cfg_attr(any(test, feature = "openapi", feature = "web"), derive(ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct ComplianceSummaryResponse {
    pub total_signed: u64,
    pub validity_percent: f64,
    // NOTE: intentionally NOT `skip_serializing_if`-gated — the frontend
    // contract is `lastAuditAt: string | null`, so the field MUST be present
    // as `null` in the empty state, not omitted.
    pub last_audit_at: Option<String>,
    pub hotspot_delta_percent: f64,
}

/// `GET /api/compliance/signatures` — per-entry signature status for the
/// recent ledger.
///
/// Returns a bare JSON array of `{ txId, entity, summary, committedAt,
/// status, category }` (camelCase) sorted by `committed_at` DESC, bounded to
/// the most recent `COMPLIANCE_SIGNATURES_LIMIT` (100) entries.
///
/// `status` is the string `VALID` | `INVALID` | `SKIPPED`:
/// - `(Some(sig), Some(pub))` → `verify_signature(...)` → `VALID` if true,
///   else `INVALID`.
/// - otherwise (no signature) → `INVALID` when `intent.require_signing` is
///   true (the CLI labels this `UNSIGNED`; the dashboard status set is
///   `{VALID,INVALID,SKIPPED}` per the E2 spec, so `UNSIGNED` maps to
///   `INVALID`), else `SKIPPED`.
///
/// `category` is the `Category` enum's `Display` form (e.g. `"FEATURE"`).
///
/// No-DB path → empty array `[]` (200, not an error).
#[derive(Debug, Serialize)]
#[cfg_attr(any(test, feature = "openapi", feature = "web"), derive(ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct ComplianceSignatureEntry {
    pub tx_id: String,
    pub entity: String,
    pub summary: String,
    pub committed_at: String,
    pub status: String,
    pub category: String,
}

#[cfg(test)]
mod hotspot_dto_tests {
    use super::*;
    use crate::impact::packet::Hotspot;
    use camino::Utf8PathBuf;
    use std::path::PathBuf;

    fn make_hotspot(path: &str, score: f32, display_score: f32, frequency: f64) -> Hotspot {
        Hotspot {
            path: PathBuf::from(path),
            score,
            display_score,
            complexity: 10,
            frequency,
            centrality: None,
        }
    }

    #[test]
    fn risk_level_critical_at_threshold_4() {
        assert_eq!(risk_level_from_display_score(4.0), "CRITICAL");
    }

    #[test]
    fn risk_level_high_just_below_critical() {
        assert_eq!(risk_level_from_display_score(3.99), "HIGH");
    }

    #[test]
    fn risk_level_high_at_threshold_3() {
        assert_eq!(risk_level_from_display_score(3.0), "HIGH");
    }

    #[test]
    fn risk_level_medium_at_threshold_2() {
        assert_eq!(risk_level_from_display_score(2.0), "MEDIUM");
    }

    #[test]
    fn risk_level_low_just_below_medium() {
        assert_eq!(risk_level_from_display_score(1.99), "LOW");
    }

    #[test]
    fn risk_level_low_at_zero() {
        assert_eq!(risk_level_from_display_score(0.0), "LOW");
    }

    #[test]
    fn change_count_floors_at_1() {
        let hotspots = vec![make_hotspot("src/main.rs", 0.5, 3.0, 0.4)];
        let git_meta = HashMap::new();
        let responses = map_hotspots_to_responses(&hotspots, &git_meta);
        assert_eq!(responses[0].change_count, 1);
    }

    #[test]
    fn change_count_rounds_frequency() {
        let hotspots = vec![make_hotspot("src/main.rs", 0.5, 3.0, 2.6)];
        let git_meta = HashMap::new();
        let responses = map_hotspots_to_responses(&hotspots, &git_meta);
        assert_eq!(responses[0].change_count, 3);
    }

    #[test]
    fn rank_is_1_based() {
        let hotspots = vec![
            make_hotspot("src/a.rs", 0.9, 4.5, 5.0),
            make_hotspot("src/b.rs", 0.7, 3.5, 3.0),
            make_hotspot("src/c.rs", 0.5, 2.5, 1.0),
        ];
        let git_meta = HashMap::new();
        let responses = map_hotspots_to_responses(&hotspots, &git_meta);
        assert_eq!(responses[0].rank, 1);
        assert_eq!(responses[1].rank, 2);
        assert_eq!(responses[2].rank, 3);
    }

    #[test]
    fn git_meta_null_for_unknown_file() {
        let hotspots = vec![make_hotspot("src/unknown.rs", 0.5, 3.0, 1.0)];
        let git_meta = HashMap::new();
        let responses = map_hotspots_to_responses(&hotspots, &git_meta);
        assert!(responses[0].last_touched_at.is_none());
        assert!(responses[0].contributor.is_none());
    }

    #[test]
    fn git_meta_populated_for_known_file() {
        let hotspots = vec![make_hotspot("src/main.rs", 0.5, 3.0, 1.0)];
        let mut git_meta = HashMap::new();
        git_meta.insert(
            "src/main.rs".to_string(),
            ("2024-06-01T12:00:00+00:00".to_string(), "Alice".to_string()),
        );
        let responses = map_hotspots_to_responses(&hotspots, &git_meta);
        assert_eq!(
            responses[0].last_touched_at.as_deref(),
            Some("2024-06-01T12:00:00+00:00")
        );
        assert_eq!(responses[0].contributor.as_deref(), Some("Alice"));
    }

    #[test]
    fn git_meta_lookup_normalizes_backslashes() {
        let hotspots = vec![make_hotspot("src\\main.rs", 0.5, 3.0, 1.0)];
        let mut git_meta = HashMap::new();
        git_meta.insert(
            "src/main.rs".to_string(),
            ("2024-06-01T12:00:00+00:00".to_string(), "Bob".to_string()),
        );
        let responses = map_hotspots_to_responses(&hotspots, &git_meta);
        assert_eq!(responses[0].contributor.as_deref(), Some("Bob"));
    }

    #[test]
    fn backward_compat_fields_preserved() {
        let hotspots = vec![make_hotspot("src/main.rs", 0.42, 3.5, 7.5)];
        let git_meta = HashMap::new();
        let responses = map_hotspots_to_responses(&hotspots, &git_meta);
        let r = &responses[0];
        assert!((r.display_score - 3.5).abs() < 1e-6);
        assert!((r.score - 0.42).abs() < 1e-6);
        assert_eq!(r.complexity, 10);
        assert!((r.frequency - 7.5).abs() < 1e-6);
        assert!(r.centrality.is_none());
    }

    #[test]
    fn id_equals_file_path() {
        let hotspots = vec![make_hotspot("src/main.rs", 0.5, 3.0, 1.0)];
        let git_meta = HashMap::new();
        let responses = map_hotspots_to_responses(&hotspots, &git_meta);
        assert_eq!(responses[0].id, "src/main.rs");
        assert_eq!(responses[0].file_path, "src/main.rs");
    }

    #[test]
    fn risk_score_equals_display_score() {
        let hotspots = vec![make_hotspot("src/main.rs", 0.5, 3.72, 1.0)];
        let git_meta = HashMap::new();
        let responses = map_hotspots_to_responses(&hotspots, &git_meta);
        assert!((responses[0].risk_score - 3.72).abs() < 1e-6);
    }

    #[test]
    fn empty_hotspots_produces_empty_response() {
        let hotspots: Vec<Hotspot> = Vec::new();
        let git_meta = HashMap::new();
        let responses = map_hotspots_to_responses(&hotspots, &git_meta);
        assert!(responses.is_empty());
    }

    #[test]
    fn centrality_copied_when_present() {
        let mut h = make_hotspot("src/main.rs", 0.5, 3.0, 1.0);
        h.centrality = Some(42);
        let hotspots = vec![h];
        let git_meta = HashMap::new();
        let responses = map_hotspots_to_responses(&hotspots, &git_meta);
        assert_eq!(responses[0].centrality, Some(42));
    }

    #[test]
    fn utf8_path_preserved() {
        let hotspots = vec![make_hotspot("src/h\u{e9}llo.rs", 0.5, 3.0, 1.0)];
        let git_meta = HashMap::new();
        let responses = map_hotspots_to_responses(&hotspots, &git_meta);
        assert_eq!(responses[0].file_path, "src/h\u{e9}llo.rs");
    }

    // Suppress unused import warning - Utf8PathBuf is used in production code
    // outside tests but we import it here for completeness.
    #[test]
    fn _utf8_path_buf_import_marker() {
        let _ = Utf8PathBuf::from("marker");
    }

    #[test]
    fn kg_node_serializes_file_path_and_complexity() {
        let node = KgNode {
            id: "urn:ledgerful:file:src/main.rs".to_string(),
            label: "src/main.rs".to_string(),
            category: "file".to_string(),
            risk_score: 3.5,
            file_path: "src/main.rs".to_string(),
            complexity: 7,
            metadata: None,
        };
        let json = serde_json::to_value(node).unwrap();
        assert_eq!(json["file_path"].as_str(), Some("src/main.rs"));
        assert_eq!(json["complexity"].as_i64(), Some(7));
    }
}
