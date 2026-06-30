//! Additional read-only JSON API handlers for the Ledgerful web dashboard.
//!
//! These endpoints back the remaining SPA screens that were not covered by the
//! core handler set in `server.rs`: report downloads, hotspot trends, contract
//! impact, security boundaries, and the CozoDB knowledge-graph subgraph.

use crate::commands::helpers::load_ledger_config;
use crate::commands::web::error::WebError;
use crate::commands::web::git_meta::lookup_git_meta;
use crate::commands::web::server::display_entity;
use crate::commands::web::state::AppState;
use crate::config::load::load_config;
use crate::contracts::AffectedContract;
use crate::git::repo::open_repo;
use crate::git::status::get_repo_status;
use crate::impact::hotspots::query_file_complexities;
use crate::impact::packet::Hotspot;
use crate::ledger::db::LedgerDb;
use crate::ledger::types::LedgerEntry;
use crate::state::layout::Layout;
use crate::state::storage::StorageManager;
use axum::Json;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use chrono::NaiveDate;
use miette::{IntoDiagnostic, Result, miette};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::{BTreeMap, HashMap, HashSet};

#[cfg(any(test, feature = "openapi", feature = "web"))]
use utoipa::{IntoParams, OpenApi, ToSchema};

#[cfg(any(test, feature = "openapi", feature = "web"))]
use super::server::{
    __path_changes_handler, __path_config_handler, __path_health_handler, __path_hotspots_handler,
    __path_ledger_handler, __path_ledger_search_handler, __path_ledger_tx_handler,
    __path_projects_handler, __path_session_handler, __path_snapshot_handler,
    __path_status_handler, ChangeResponse, ChangedFileResponse, ChangesQuery, ConfigResponse,
    HotspotsQueryParams, LedgerDetailResponse, LedgerEntryResponse, LedgerListQuery,
    LedgerSearchQuery, ProjectResponse, SnapshotResponse, StatusResponse, UserSession,
};

#[cfg(any(test, feature = "openapi", feature = "web"))]
#[derive(OpenApi)]
#[openapi(
    info(
        title = "Ledgerful Daemon API",
        version = "0.1.6",
        description = "Machine-readable OpenAPI contract for the Ledgerful daemon `/api/*` endpoints. Generated from the Rust DTOs via utoipa."
    ),
    paths(
        health_handler,
        session_handler,
        snapshot_handler,
        status_handler,
        projects_handler,
        ledger_handler,
        ledger_search_handler,
        ledger_tx_handler,
        changes_handler,
        hotspots_handler,
        config_handler,
        hotspots_trend_handler,
        latest_impact_handler,
        latest_verify_handler,
        verify_health_handler,
        verify_history_handler,
        verify_steps_handler,
        compliance_summary_handler,
        compliance_signatures_handler,
        compliance_export_handler,
        endpoints_changed_handler,
        security_boundaries_handler,
        knowledge_graph_handler
    ),
    components(schemas(
        UserSession,
        SnapshotResponse,
        StatusResponse,
        ProjectResponse,
        LedgerEntryResponse,
        LedgerDetailResponse,
        ChangedFileResponse,
        LedgerListQuery,
        LedgerSearchQuery,
        ChangesQuery,
        ChangeResponse,
        HotspotsQueryParams,
        ConfigResponse,
        HotspotResponse,
        HotspotTrendQuery,
        HotspotTrendResponse,
        HotspotTrendSeries,
        VerificationHealthResponse,
        VerifyHistoryQuery,
        VerificationTrendPoint,
        VerificationStepResponse,
        ComplianceSummaryResponse,
        ComplianceSignatureEntry,
        AffectedContract,
        SecurityBoundariesResponse,
        KnowledgeGraphQuery,
        KnowledgeGraphResponse,
        KgNode,
        KgEdge
    )),
    tags(
        (name = "health", description = "Daemon liveness"),
        (name = "session", description = "Current user session"),
        (name = "snapshot", description = "Summary metrics"),
        (name = "status", description = "Daemon health status"),
        (name = "projects", description = "Project list"),
        (name = "ledger", description = "Ledger transactions"),
        (name = "changes", description = "Recent changes"),
        (name = "hotspots", description = "Hotspot rankings and trends"),
        (name = "reports", description = "Latest impact/verify report JSON"),
        (name = "verify", description = "Verification health/history/steps"),
        (name = "compliance", description = "Compliance summary/signatures/export"),
        (name = "endpoints", description = "Affected API contracts"),
        (name = "security", description = "Security boundaries"),
        (name = "knowledge-graph", description = "CozoDB knowledge-graph subgraph"),
        (name = "config", description = "Daemon configuration"),
        (name = "sync", description = "Local M0 sync state")
    )
)]
pub struct ApiDoc;

/// Generate the canonical OpenAPI JSON string for this build.
#[cfg(any(test, feature = "openapi", feature = "web"))]
pub fn generate_openapi_json() -> String {
    use utoipa::OpenApi;
    ApiDoc::openapi().to_pretty_json().unwrap_or_else(|e| {
        tracing::error!("OpenAPI serialization failed: {e}");
        String::from("{}")
    })
}

use std::sync::Arc;
use std::time::{Duration, Instant, UNIX_EPOCH};

const KG_CACHE_TTL: Duration = Duration::from_secs(60);
const KG_MAX_LIMIT: usize = 1000;

// ---------------------------------------------------------------------------
// HotspotResponse DTO (Track TA29)
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Report endpoints
// ---------------------------------------------------------------------------

/// `GET /api/reports/latest-impact.json` — passthrough of the latest impact
/// report JSON. Returns `application/json` with an opaque object schema when
/// a report exists; 404 otherwise.
#[utoipa::path(
    get,
    path = "/api/reports/latest-impact.json",
    operation_id = "getLatestImpactReport",
    tag = "reports",
    responses(
        (status = 200, description = "Latest impact report JSON", body = Object, content_type = "application/json"),
        (status = 404, description = "Report not found")
    )
)]
pub async fn latest_impact_handler(
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, WebError> {
    serve_report(state.layout.clone(), "latest-impact.json").await
}

/// `GET /api/reports/latest-verify.json` — passthrough of the latest verify
/// report JSON. Returns `application/json` with an opaque object schema when
/// a report exists; 404 otherwise.
#[utoipa::path(
    get,
    path = "/api/reports/latest-verify.json",
    operation_id = "getLatestVerifyReport",
    tag = "reports",
    responses(
        (status = 200, description = "Latest verify report JSON", body = Object, content_type = "application/json"),
        (status = 404, description = "Report not found")
    )
)]
pub async fn latest_verify_handler(
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, WebError> {
    serve_report(state.layout.clone(), "latest-verify.json").await
}

// ---------------------------------------------------------------------------
// Verification dashboard endpoints (Track E1)
// ---------------------------------------------------------------------------

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

/// `GET /api/verify/health` — overall verification health.
#[utoipa::path(
    get,
    path = "/api/verify/health",
    operation_id = "getVerifyHealth",
    tag = "verify",
    responses(
        (status = 200, description = "Verification health", body = VerificationHealthResponse)
    )
)]
pub async fn verify_health_handler(
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, WebError> {
    let layout = state.layout.clone();
    let response = tokio::task::spawn_blocking(move || fetch_verify_health(&layout))
        .await
        .map_err(|e| WebError::Internal(format!("Background task failed: {e}")))?
        .map_err(|e| WebError::Internal(format!("Failed to read verification health: {e}")))?;
    Ok(Json(response))
}

fn fetch_verify_health(layout: &Layout) -> Result<VerificationHealthResponse> {
    let db_path = layout.state_subdir().join("ledger.db");
    if !db_path.exists() {
        return Ok(VerificationHealthResponse {
            status: "DEGRADED".to_string(),
            last_run_at: None,
            message: Some("No verification runs recorded".to_string()),
        });
    }

    let storage = StorageManager::open_read_only_sqlite_only(&layout.root)?;
    let latest = storage.get_latest_verification_run()?;

    let Some((_id, timestamp, overall_pass)) = latest else {
        return Ok(VerificationHealthResponse {
            status: "DEGRADED".to_string(),
            last_run_at: None,
            message: Some("No verification runs recorded".to_string()),
        });
    };

    if !overall_pass {
        return Ok(VerificationHealthResponse {
            status: "FAILING".to_string(),
            last_run_at: Some(timestamp),
            message: None,
        });
    }

    // Latest run passed — check staleness against the 7-day threshold.
    let stale = is_stale_timestamp(&timestamp, STALE_VERIFY_THRESHOLD_SECS);
    if stale {
        Ok(VerificationHealthResponse {
            status: "DEGRADED".to_string(),
            last_run_at: Some(timestamp),
            message: Some("Last verification run is stale (older than 7 days)".to_string()),
        })
    } else {
        Ok(VerificationHealthResponse {
            status: "HEALTHY".to_string(),
            last_run_at: Some(timestamp),
            message: None,
        })
    }
}

/// `GET /api/verify/history?days=30` — per-date pass/fail counts over the
/// last `days` days (default 30, accepts 90). Returns a bare JSON array of
/// `{ date, passed, failed }` sorted ascending by date. Dates with no runs
/// are omitted (deterministic: only dates that have at least one run appear).
#[derive(Debug, Deserialize, Default)]
#[cfg_attr(
    any(test, feature = "openapi", feature = "web"),
    derive(IntoParams, ToSchema)
)]
pub struct VerifyHistoryQuery {
    days: Option<u64>,
}

#[derive(Debug, Serialize)]
#[cfg_attr(any(test, feature = "openapi", feature = "web"), derive(ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct VerificationTrendPoint {
    pub date: String,
    pub passed: u64,
    pub failed: u64,
}

/// `GET /api/verify/history` — pass/fail trend over time.
#[utoipa::path(
    get,
    path = "/api/verify/history",
    operation_id = "getVerifyHistory",
    tag = "verify",
    params(VerifyHistoryQuery),
    responses(
        (status = 200, description = "Verification history points", body = [VerificationTrendPoint])
    )
)]
pub async fn verify_history_handler(
    State(state): State<Arc<AppState>>,
    Query(params): Query<VerifyHistoryQuery>,
) -> Result<impl IntoResponse, WebError> {
    let days = params.days.unwrap_or(30).min(365);
    let layout = state.layout.clone();
    let points = tokio::task::spawn_blocking(move || fetch_verify_history(&layout, days))
        .await
        .map_err(|e| WebError::Internal(format!("Background task failed: {e}")))?
        .map_err(|e| WebError::Internal(format!("Failed to read verification history: {e}")))?;
    Ok(Json(points))
}

fn fetch_verify_history(layout: &Layout, days: u64) -> Result<Vec<VerificationTrendPoint>> {
    let db_path = layout.state_subdir().join("ledger.db");
    if !db_path.exists() {
        return Ok(Vec::new());
    }

    let storage = StorageManager::open_read_only_sqlite_only(&layout.root)?;
    let cutoff = iso_cutoff_now(days);
    let rows = storage.get_verification_history(&cutoff)?;
    Ok(rows
        .into_iter()
        .map(|r| VerificationTrendPoint {
            date: r.date,
            passed: r.passed,
            failed: r.failed,
        })
        .collect())
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

/// `GET /api/verify/steps` — per-step verification aggregates.
#[utoipa::path(
    get,
    path = "/api/verify/steps",
    operation_id = "getVerifySteps",
    tag = "verify",
    responses(
        (status = 200, description = "Verification step metrics", body = [VerificationStepResponse])
    )
)]
pub async fn verify_steps_handler(
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, WebError> {
    let layout = state.layout.clone();
    let steps = tokio::task::spawn_blocking(move || fetch_verify_steps(&layout))
        .await
        .map_err(|e| WebError::Internal(format!("Background task failed: {e}")))?
        .map_err(|e| WebError::Internal(format!("Failed to read verification steps: {e}")))?;
    Ok(Json(steps))
}

fn fetch_verify_steps(layout: &Layout) -> Result<Vec<VerificationStepResponse>> {
    let db_path = layout.state_subdir().join("ledger.db");
    if !db_path.exists() {
        return Ok(Vec::new());
    }

    let storage = StorageManager::open_read_only_sqlite_only(&layout.root)?;
    let rows = storage.get_verification_step_stats(VERIFY_STEPS_RECENT_RUN_COUNT)?;
    // Friendly `name` lookup: command -> latest plan step `description`.
    // Falls back to `command` when no `plan_json`/parse error/no matching step.
    let descriptions =
        storage.get_verification_command_descriptions(VERIFY_STEPS_DESC_RUN_COUNT)?;
    Ok(rows
        .into_iter()
        .map(|r| {
            let total = r.total.max(1);
            let pass_rate = (r.passed as f64 / total as f64) * 100.0;
            let raw_description = descriptions.get(&r.command).cloned();
            let name = friendly_step_name(raw_description.as_deref(), &r.command);
            VerificationStepResponse {
                id: r.command.clone(),
                name,
                last_run_at: r.last_run_at,
                average_duration_ms: r.average_duration_ms,
                pass_rate_percent: (pass_rate * 100.0).round() / 100.0,
                recent_failures: r.recent_failures,
            }
        })
        .collect())
}

/// Derive a short, dashboard-friendly step `name` from a verification plan
/// step `description`.
///
/// The plan builder (`src/verify/plan.rs`) stuffs `description` with a
/// traceability blob: it starts as a friendly label (`"From rules: <cmd>"` or
/// `"Default: run project tests"`) and then ` | `-concatenates one
/// `"Predicted impact (<reason>) on <file>"` segment per predicted affected
/// file (for `verify --explain`). Surfacing that blob verbatim as the dashboard
/// step name yields a multi-thousand-character string that breaks the table
/// layout, so we keep only the first ` | `-delimited segment.
///
/// When the first segment is itself a `"Predicted impact …"` annotation (the
/// command only ever appeared via predicted-impact rules, never via an explicit
/// `"From rules:"` / `"Default:"` prefix), there is no friendly label to show,
/// so we fall back to the raw command string.
fn friendly_step_name(description: Option<&str>, command: &str) -> String {
    let Some(description) = description else {
        return command.to_string();
    };
    let first_segment = description
        .split(" | ")
        .next()
        .unwrap_or(description)
        .trim();
    if first_segment.is_empty() || first_segment.starts_with("Predicted impact") {
        command.to_string()
    } else {
        first_segment.to_string()
    }
}

/// Seconds before now that counts as a "stale" latest verification run.
const STALE_VERIFY_THRESHOLD_SECS: i64 = 7 * 24 * 60 * 60;

/// Number of most-recent verification runs to scan for `recentFailures` on
/// `/api/verify/steps`.
const VERIFY_STEPS_RECENT_RUN_COUNT: usize = 10;

/// Number of most-recent `plan_json` rows to parse for friendly step `name`
/// (description) lookup on `/api/verify/steps`. Bounded to keep the endpoint
/// efficient — parsing failures on any single row are silently skipped.
const VERIFY_STEPS_DESC_RUN_COUNT: usize = 50;

/// Return an RFC 3339 timestamp `days` days before now, for use as a SQL
/// `timestamp >= ?` cutoff. Falls back to a far-past date if the clock is
/// unavailable so the window is effectively unbounded rather than empty.
fn iso_cutoff_now(days: u64) -> String {
    let now = chrono::Utc::now();
    let cutoff = now - chrono::Duration::days(days as i64);
    cutoff.to_rfc3339()
}

/// Whether `timestamp_rfc3339` is older than `threshold_secs` from now.
/// Returns `false` if the timestamp cannot be parsed (treat unparseable
/// timestamps as non-stale so a malformed row never masks a FAILING/HEALTHY
/// signal behind a spurious DEGRADED).
fn is_stale_timestamp(timestamp_rfc3339: &str, threshold_secs: i64) -> bool {
    let Ok(parsed) = chrono::DateTime::parse_from_rfc3339(timestamp_rfc3339) else {
        return false;
    };
    let now = chrono::Utc::now();
    let age = now.signed_duration_since(parsed.with_timezone(&chrono::Utc));
    age.num_seconds() > threshold_secs
}

async fn serve_report(
    layout: Layout,
    filename: &'static str,
) -> Result<impl IntoResponse, WebError> {
    let maybe_value = tokio::task::spawn_blocking(move || read_report_json(&layout, filename))
        .await
        .map_err(|e| WebError::Internal(format!("Background task failed: {e}")))?
        .map_err(|e| WebError::Internal(format!("Failed to read report: {e}")))?;

    match maybe_value {
        Some(value) => Ok((StatusCode::OK, Json(value))),
        None => Err(WebError::NotFound),
    }
}

fn read_report_json(layout: &Layout, filename: &str) -> Result<Option<serde_json::Value>> {
    let path = layout.reports_dir().join(filename);
    if !path.exists() {
        return Ok(None);
    }
    let content = std::fs::read_to_string(&path).into_diagnostic()?;
    let value = serde_json::from_str(&content)
        .map_err(|e| miette!("Invalid JSON in report {}: {}", path, e))?;
    Ok(Some(value))
}

// ---------------------------------------------------------------------------
// Hotspot trend endpoint
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, Default)]
#[cfg_attr(
    any(test, feature = "openapi", feature = "web"),
    derive(IntoParams, ToSchema)
)]
pub struct HotspotTrendQuery {
    days: Option<u64>,
    limit: Option<usize>,
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

/// `GET /api/hotspots/trend` — rolling hotspot trend series.
#[utoipa::path(
    get,
    path = "/api/hotspots/trend",
    operation_id = "getHotspotTrend",
    tag = "hotspots",
    params(HotspotTrendQuery),
    responses(
        (status = 200, description = "Hotspot trend data", body = HotspotTrendResponse)
    )
)]
pub async fn hotspots_trend_handler(
    State(state): State<Arc<AppState>>,
    Query(params): Query<HotspotTrendQuery>,
) -> Result<impl IntoResponse, WebError> {
    let layout = state.layout.clone();
    let days = params.days.unwrap_or(90);
    let limit = params.limit.unwrap_or(20).min(200);
    let response = tokio::task::spawn_blocking(move || fetch_hotspot_trend(&layout, days, limit))
        .await
        .map_err(|e| WebError::Internal(format!("Background task failed: {e}")))?
        .map_err(|e| WebError::Internal(format!("Failed to calculate hotspot trend: {e}")))?;
    Ok(Json(response))
}

fn fetch_hotspot_trend(layout: &Layout, days: u64, limit: usize) -> Result<HotspotTrendResponse> {
    let repo = match open_repo(layout.root.as_std_path()) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("No git repository available for /api/hotspots/trend: {e}");
            return Ok(empty_trend_response());
        }
    };

    let config = load_ledger_config(layout).unwrap_or_default();
    let storage = StorageManager::open_read_only_sqlite_only(&layout.root)?;

    let half_life = config.hotspots.decay_half_life as f64;
    let max_commits = config.hotspots.max_commits;

    let commits = collect_recent_commits(&repo, days, max_commits)?;
    if commits.is_empty() {
        return Ok(empty_trend_response());
    }

    let (labels, bucket_count, start_date) = build_date_buckets(days);

    // Collect the distinct file paths touched in the window.
    let mut all_paths_set = HashSet::new();
    for (_, files) in &commits {
        for file in files {
            all_paths_set.insert(file.clone());
        }
    }
    let all_paths: Vec<String> = all_paths_set.into_iter().collect();

    // Look up complexities once and derive a global maximum for normalization.
    let complexities = query_file_complexities(&storage, &all_paths)?;
    let max_complexity = complexities.values().copied().max().unwrap_or(0).max(1) as f32;

    // Initialise per-file cumulative frequency buckets.
    let mut file_frequencies: HashMap<String, Vec<f64>> = HashMap::from_iter(
        all_paths
            .iter()
            .map(|p| (p.clone(), vec![0.0; bucket_count])),
    );

    let date_to_bucket = |time: u64| -> usize {
        chrono::DateTime::from_timestamp(time as i64, 0)
            .map(|dt| dt.naive_utc().date())
            .map(|date| {
                let idx = date.signed_duration_since(start_date).num_days() as usize;
                idx.min(bucket_count.saturating_sub(1))
            })
            .unwrap_or(0)
    };

    // Commits are returned newest-first, so index 0 is the most recent.
    for (commit_idx, (time, files)) in commits.iter().enumerate() {
        let weight = if half_life > 0.0 {
            (2.0_f64).powf(-(commit_idx as f64) / half_life)
        } else {
            1.0
        };
        let bucket = date_to_bucket(*time);
        for file in files {
            if let Some(freqs) = file_frequencies.get_mut(file.as_str()) {
                for freq in freqs.iter_mut().take(bucket_count).skip(bucket) {
                    *freq += weight;
                }
            }
        }
    }

    // Compute a per-bucket max frequency for normalization.
    let mut max_freqs = vec![0.0_f64; bucket_count];
    for freqs in file_frequencies.values() {
        for (b, freq) in freqs.iter().enumerate() {
            if *freq > max_freqs[b] {
                max_freqs[b] = *freq;
            }
        }
    }
    for max in &mut max_freqs {
        if *max < 1.0 {
            *max = 1.0;
        }
    }

    // Score each file at every bucket and rank by its most recent score.
    let mut ranked: Vec<(String, f32, Vec<f32>)> = Vec::with_capacity(all_paths.len());
    for (path, freqs) in file_frequencies {
        let complexity = complexities.get(&path).copied().unwrap_or(0) as f32;
        let mut scores = Vec::with_capacity(bucket_count);
        let mut latest_score = 0.0_f32;
        for b in 0..bucket_count {
            let f_norm = (freqs[b] as f32) / (max_freqs[b] as f32);
            let c_norm = complexity / max_complexity;
            let score = f_norm * c_norm;
            scores.push(score);
            latest_score = score;
        }
        ranked.push((path, latest_score, scores));
    }

    ranked.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(&b.0))
    });

    let truncated = ranked.len() > limit;
    ranked.truncate(limit);

    let series = ranked
        .into_iter()
        .map(|(path, _, scores)| HotspotTrendSeries { path, scores })
        .collect();

    Ok(HotspotTrendResponse {
        labels,
        series,
        truncated,
    })
}

fn empty_trend_response() -> HotspotTrendResponse {
    HotspotTrendResponse {
        labels: Vec::new(),
        series: Vec::new(),
        truncated: false,
    }
}

fn build_date_buckets(days: u64) -> (Vec<String>, usize, NaiveDate) {
    let now = std::time::SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let end_date = chrono::DateTime::from_timestamp(now as i64, 0)
        .map(|dt| dt.naive_utc().date())
        .unwrap_or_else(|| NaiveDate::from_ymd_opt(2020, 1, 1).unwrap_or(NaiveDate::MIN));
    let start_date = end_date
        .checked_sub_signed(chrono::Duration::days(days.saturating_sub(1) as i64))
        .unwrap_or(end_date);

    let mut labels = Vec::new();
    let mut date = start_date;
    while date <= end_date {
        labels.push(date.format("%Y-%m-%d").to_string());
        date = date.succ_opt().unwrap_or(date);
    }
    let bucket_count = labels.len();
    (labels, bucket_count, start_date)
}

fn collect_recent_commits(
    repo: &gix::Repository,
    days: u64,
    max_commits: usize,
) -> Result<Vec<(u64, Vec<String>)>> {
    let head = repo
        .head_commit()
        .map_err(|e| miette!("Failed to read HEAD: {e}"))?;
    let walk = head
        .id()
        .ancestors()
        .first_parent_only()
        .all()
        .map_err(|e| miette!("Failed to walk git history: {e}"))?;

    let now = std::time::SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let cutoff = now.saturating_sub(days * 86400);

    let mut commits = Vec::new();
    for res in walk {
        if commits.len() >= max_commits {
            break;
        }
        let info = match res {
            Ok(info) => info,
            Err(e) => {
                tracing::warn!("Failed to retrieve commit info during trend walk: {e}");
                continue;
            }
        };

        let commit = match info.id().object().map(|obj| obj.into_commit()) {
            Ok(commit) => commit,
            Err(e) => {
                tracing::warn!("Failed to retrieve commit object for {}: {e}", info.id());
                continue;
            }
        };

        let commit_time = commit
            .time()
            .map_err(|e| miette!("Failed to read commit time for {}: {e}", info.id()))?
            .seconds as u64;
        if commit_time < cutoff {
            break;
        }

        if commit.parent_ids().count() > 1 {
            continue; // skip merge commits
        }

        let current_tree = match commit.tree() {
            Ok(tree) => tree,
            Err(e) => {
                tracing::warn!("Failed to retrieve tree for {}: {e}", info.id());
                continue;
            }
        };
        let parent_tree = match commit.parent_ids().next() {
            Some(p_id) => match p_id.object().map(|obj| obj.into_commit().tree()) {
                Ok(Ok(tree)) => tree,
                _ => {
                    tracing::warn!(
                        "Failed to retrieve parent tree for {}; using empty tree",
                        info.id()
                    );
                    repo.empty_tree()
                }
            },
            None => repo.empty_tree(),
        };

        let changes = match repo.diff_tree_to_tree(Some(&parent_tree), Some(&current_tree), None) {
            Ok(changes) => changes,
            Err(e) => {
                tracing::warn!("Failed to diff tree for {}: {e}", info.id());
                continue;
            }
        };

        let mut files = Vec::new();
        for change in changes {
            let path = match change {
                gix::object::tree::diff::ChangeDetached::Addition { location, .. }
                | gix::object::tree::diff::ChangeDetached::Deletion { location, .. }
                | gix::object::tree::diff::ChangeDetached::Modification { location, .. } => {
                    String::from_utf8_lossy(&location).replace('\\', "/")
                }
                gix::object::tree::diff::ChangeDetached::Rewrite {
                    location,
                    source_location,
                    ..
                } => {
                    let src = String::from_utf8_lossy(&source_location).replace('\\', "/");
                    let dst = String::from_utf8_lossy(&location).replace('\\', "/");
                    files.push(src);
                    dst
                }
            };
            if !path.is_empty() {
                files.push(path);
            }
        }

        if !files.is_empty() {
            commits.push((commit_time, files));
        }
    }

    Ok(commits)
}

// ---------------------------------------------------------------------------
// Affected API endpoints endpoint
// ---------------------------------------------------------------------------

/// `GET /api/endpoints/changed` — API contracts affected by changed files.
#[utoipa::path(
    get,
    path = "/api/endpoints/changed",
    operation_id = "getEndpointsChanged",
    tag = "endpoints",
    responses(
        (status = 200, description = "Affected API contracts", body = [AffectedContract])
    )
)]
pub async fn endpoints_changed_handler(
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, WebError> {
    let layout = state.layout.clone();
    let contracts = tokio::task::spawn_blocking(move || fetch_endpoints_changed(&layout))
        .await
        .map_err(|e| WebError::Internal(format!("Background task failed: {e}")))?
        .map_err(|e| WebError::Internal(format!("Failed to match endpoints: {e}")))?;
    Ok(Json(contracts))
}

fn fetch_endpoints_changed(layout: &Layout) -> Result<Vec<AffectedContract>> {
    let repo = match open_repo(layout.root.as_std_path()) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("No git repository available for /api/endpoints/changed: {e}");
            return Ok(Vec::new());
        }
    };

    let file_changes = get_repo_status(&repo)
        .map_err(|e| miette!("Failed to get repository status for endpoint matching: {e}"))?;
    let changed: Vec<String> = file_changes
        .iter()
        .map(|c| c.path.to_string_lossy().replace('\\', "/"))
        .collect();

    if changed.is_empty() {
        return Ok(Vec::new());
    }

    let config = load_ledger_config(layout).unwrap_or_default();
    let storage = match StorageManager::open_read_only_sqlite_only(&layout.root) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("Storage not available for /api/endpoints/changed: {e}");
            return Ok(Vec::new());
        }
    };

    let conn = storage.get_connection();
    crate::contracts::matcher::match_changed_files(
        &config.contracts,
        conn,
        &config.local_model,
        &changed,
    )
    .map_err(|e| miette!("Endpoint matching failed: {e}"))
}

// ---------------------------------------------------------------------------
// Security boundaries endpoint
// ---------------------------------------------------------------------------

#[derive(Serialize)]
#[cfg_attr(any(test, feature = "openapi", feature = "web"), derive(ToSchema))]
pub struct SecurityBoundariesResponse {
    pub meta: serde_json::Value,
    pub boundaries: serde_json::Value,
}

/// `GET /api/security/boundaries` — security boundary counts and edges.
#[utoipa::path(
    get,
    path = "/api/security/boundaries",
    operation_id = "getSecurityBoundaries",
    tag = "security",
    responses(
        (status = 200, description = "Security boundaries", body = SecurityBoundariesResponse)
    )
)]
pub async fn security_boundaries_handler(
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, WebError> {
    let layout = state.layout.clone();
    let response = tokio::task::spawn_blocking(move || fetch_security_boundaries(&layout))
        .await
        .map_err(|e| WebError::Internal(format!("Background task failed: {e}")))?
        .map_err(|e| WebError::Internal(format!("Failed to query security boundaries: {e}")))?;
    Ok(Json(response))
}

fn fetch_security_boundaries(layout: &Layout) -> Result<SecurityBoundariesResponse> {
    let storage = match StorageManager::open_read_only(&layout.root) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("Storage not available for /api/security/boundaries: {e}");
            return Ok(empty_boundaries_response());
        }
    };

    let cozo = match storage.cozo {
        Some(c) => c,
        None => {
            tracing::warn!("CozoDB not available for /api/security/boundaries");
            return Ok(empty_boundaries_response());
        }
    };

    // Authorisation nodes: policy, principal, action, resource.
    let auth_res = cozo.run_script(
        "?[id, label, category] := *node{id, label, category}, \
         category in ['policy', 'principal', 'action', 'resource']",
    )?;

    let mut counts: HashMap<String, usize> = HashMap::new();
    let mut auth_nodes = Vec::new();
    for row in &auth_res.rows {
        if let (
            Some(cozo::DataValue::Str(id)),
            Some(cozo::DataValue::Str(label)),
            Some(cozo::DataValue::Str(cat)),
        ) = (row.first(), row.get(1), row.get(2))
        {
            *counts.entry(cat.to_string()).or_insert(0) += 1;
            auth_nodes.push(json!({
                "id": id.to_string(),
                "label": label.to_string(),
                "category": cat.to_string(),
            }));
        }
    }

    // Cross-surface boundary edges: policy -> protected entity.
    let boundary_res = cozo.run_script(
        "?[policy_id, policy_label, relation, target_id, target_label, target_cat] := \
         *node{id: policy_id, label: policy_label, category: 'policy'}, \
         *edge{source: policy_id, target: target_id, relation: rel}, \
         *node{id: target_id, label: target_label, category: target_cat}, \
         target_cat in ['service', 'endpoint', 'config_key', 'deploy_surface', 'adr'], \
         relation = rel",
    )?;

    let mut boundary_edges = Vec::new();
    for row in &boundary_res.rows {
        if let (
            Some(cozo::DataValue::Str(pid)),
            Some(cozo::DataValue::Str(plabel)),
            Some(cozo::DataValue::Str(rel)),
            Some(cozo::DataValue::Str(tid)),
            Some(cozo::DataValue::Str(tlabel)),
            Some(cozo::DataValue::Str(tcat)),
        ) = (
            row.first(),
            row.get(1),
            row.get(2),
            row.get(3),
            row.get(4),
            row.get(5),
        ) {
            boundary_edges.push(json!({
                "policy_id": pid.to_string(),
                "policy_label": plabel.to_string(),
                "relation": rel.to_string(),
                "target_id": tid.to_string(),
                "target_label": tlabel.to_string(),
                "target_category": tcat.to_string(),
            }));
        }
    }

    Ok(SecurityBoundariesResponse {
        meta: json!({ "counts": counts }),
        boundaries: json!({
            "auth_nodes": auth_nodes,
            "boundary_edges": boundary_edges,
        }),
    })
}

fn empty_boundaries_response() -> SecurityBoundariesResponse {
    SecurityBoundariesResponse {
        meta: json!({ "counts": {} }),
        boundaries: json!({
            "auth_nodes": [],
            "boundary_edges": [],
        }),
    }
}

// ---------------------------------------------------------------------------
// Knowledge-graph subgraph endpoint
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, Default)]
#[cfg_attr(
    any(test, feature = "openapi", feature = "web"),
    derive(IntoParams, ToSchema)
)]
pub struct KnowledgeGraphQuery {
    limit: Option<usize>,
    focus: Option<String>,
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

/// `GET /api/knowledge-graph` — CozoDB knowledge-graph subgraph.
#[utoipa::path(
    get,
    path = "/api/knowledge-graph",
    operation_id = "getKnowledgeGraph",
    tag = "knowledge-graph",
    params(KnowledgeGraphQuery),
    responses(
        (status = 200, description = "Knowledge graph nodes and edges", body = KnowledgeGraphResponse)
    )
)]
pub async fn knowledge_graph_handler(
    State(state): State<Arc<AppState>>,
    Query(params): Query<KnowledgeGraphQuery>,
) -> Result<impl IntoResponse, WebError> {
    let limit = params.limit.unwrap_or(200).min(KG_MAX_LIMIT);
    let focus_changed = params.focus.as_deref() == Some("changed");
    let cache_key = (limit, focus_changed);

    {
        let guard = state.kg_cache.lock().await;
        if let Some((cached_at, cached_key, ref response)) = *guard
            && cached_key == cache_key
            && cached_at.elapsed() < KG_CACHE_TTL
        {
            return Ok(Json(response.clone()));
        }
    }

    let layout = state.layout.clone();
    let response =
        tokio::task::spawn_blocking(move || fetch_knowledge_graph(&layout, limit, focus_changed))
            .await
            .map_err(|e| WebError::Internal(format!("Background task failed: {e}")))?
            .map_err(|e| WebError::Internal(format!("Failed to query knowledge graph: {e}")))?;

    {
        let mut guard = state.kg_cache.lock().await;
        *guard = Some((Instant::now(), cache_key, response.clone()));
    }

    Ok(Json(response))
}

fn fetch_knowledge_graph(
    layout: &Layout,
    limit: usize,
    focus_changed: bool,
) -> Result<KnowledgeGraphResponse> {
    let storage = match StorageManager::open_read_only(&layout.root) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("Storage not available for /api/knowledge-graph: {e}");
            return Ok(empty_kg_response());
        }
    };

    let cozo = match storage.cozo {
        Some(c) => c,
        None => {
            tracing::warn!("CozoDB not available for /api/knowledge-graph");
            return Ok(empty_kg_response());
        }
    };

    let mut node_ids: Vec<String> = Vec::new();
    let mut truncated = false;

    if focus_changed {
        let changed_files = collect_changed_file_paths(layout);
        if !changed_files.is_empty() {
            let params = id_list_param("files", &changed_files);
            let changed_res = cozo.run_script_with_params(
                "?[id] := *node{id, metadata: meta}, \
                 source_file = get(meta, 'source_file'), \
                 source_file in $files",
                params,
                cozo::ScriptMutability::Immutable,
            );

            let mut seed_ids = HashSet::new();
            if let Ok(res) = changed_res {
                for row in &res.rows {
                    if let Some(cozo::DataValue::Str(id)) = row.first() {
                        seed_ids.insert(id.to_string());
                    }
                }
            }

            if !seed_ids.is_empty() {
                let mut ids_within_two_hops =
                    expand_two_hops(&cozo, &seed_ids, limit.saturating_mul(2))?;
                if ids_within_two_hops.len() > limit {
                    truncated = true;
                    ids_within_two_hops.truncate(limit);
                }
                node_ids = ids_within_two_hops.into_iter().collect();
            }
        }
    }

    if node_ids.is_empty() {
        // Fallback: highest-risk nodes when there are no recent changes or focus is off.
        node_ids = fetch_top_risk_nodes(&cozo, limit)?;
    }

    if node_ids.is_empty() {
        return Ok(empty_kg_response());
    }

    let mut nodes = fetch_node_details(&cozo, &node_ids)?;
    let edges = fetch_edges_among(&cozo, &node_ids)?;

    enrich_kg_nodes(layout, &mut nodes);

    Ok(KnowledgeGraphResponse {
        nodes,
        edges,
        truncated,
    })
}

/// Enrich knowledge-graph nodes with SQLite-derived file paths and complexities.
/// This closes the gap between the backend node shape and the frontend graph
/// table, which expects top-level `file_path` and `complexity` fields.
fn enrich_kg_nodes(layout: &Layout, nodes: &mut [KgNode]) {
    let storage = match StorageManager::open_read_only_sqlite_only(&layout.root) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("Storage not available for knowledge-graph enrichment: {e}");
            return;
        }
    };

    let file_paths: Vec<String> = nodes
        .iter()
        .filter(|n| n.category == "file")
        .filter_map(|n| {
            n.id.strip_prefix("urn:ledgerful:file:")
                .map(|s| s.to_string())
        })
        .collect();
    let symbol_qns: Vec<String> = nodes
        .iter()
        .filter(|n| n.category == "symbol")
        .filter_map(|n| {
            n.id.strip_prefix("urn:ledgerful:symbol:")
                .map(|s| s.to_string())
        })
        .collect();

    // File nodes: complexity from the hotspots table, file path from the URN.
    if let Ok(complexities) = query_file_complexities(&storage, &file_paths) {
        for n in nodes.iter_mut().filter(|n| n.category == "file") {
            if let Some(p) = n.id.strip_prefix("urn:ledgerful:file:") {
                n.complexity = complexities.get(p).copied().unwrap_or(0);
            }
        }
    }

    // Symbol nodes: file path and cognitive complexity from project_symbols.
    if !symbol_qns.is_empty() {
        let placeholders = symbol_qns.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!(
            "SELECT ps.qualified_name, pf.file_path, ps.cognitive_complexity \
             FROM project_symbols ps \
             JOIN project_files pf ON ps.file_id = pf.id \
             WHERE ps.qualified_name IN ({})",
            placeholders
        );
        let conn = storage.get_connection();
        if let Ok(mut stmt) = conn.prepare(&sql) {
            let rows = stmt.query_map(rusqlite::params_from_iter(&symbol_qns), |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i32>(2)?,
                ))
            });
            if let Ok(rows) = rows {
                let mut lookup: HashMap<String, (String, i32)> = HashMap::new();
                for (qn, file_path, complexity) in rows.flatten() {
                    lookup.insert(qn, (file_path, complexity));
                }
                for n in nodes.iter_mut() {
                    if n.category == "symbol"
                        && let Some(qn) = n.id.strip_prefix("urn:ledgerful:symbol:")
                        && let Some((file_path, complexity)) = lookup.get(qn)
                    {
                        if n.file_path.is_empty() {
                            n.file_path.clone_from(file_path);
                        }
                        n.complexity = *complexity;
                    }
                }
            }
        }
    }
}

fn empty_kg_response() -> KnowledgeGraphResponse {
    KnowledgeGraphResponse {
        nodes: Vec::new(),
        edges: Vec::new(),
        truncated: false,
    }
}

fn collect_changed_file_paths(layout: &Layout) -> Vec<String> {
    let mut paths = HashSet::new();

    // Current working-tree changes.
    if let Ok(repo) = open_repo(layout.root.as_std_path())
        && let Ok(changes) = get_repo_status(&repo)
    {
        for change in changes {
            paths.insert(change.path.to_string_lossy().replace('\\', "/"));
        }
    }

    // Recent commits (last 7 days) for broader context.
    if let Ok(repo) = open_repo(layout.root.as_std_path())
        && let Ok(commits) = collect_recent_commits(&repo, 7, 1000)
    {
        for (_, files) in commits {
            for file in files {
                paths.insert(file);
            }
        }
    }

    paths.into_iter().collect()
}

fn id_list_param(key: &str, ids: &[String]) -> BTreeMap<String, cozo::DataValue> {
    let mut params = BTreeMap::new();
    let list: Vec<cozo::DataValue> = ids
        .iter()
        .map(|id| cozo::DataValue::Str(id.clone().into()))
        .collect();
    params.insert(key.to_string(), cozo::DataValue::List(Box::new(list)));
    params
}

/// Return all node IDs within two undirected hops of `seed_ids`, capped at `cap`.
fn expand_two_hops(
    cozo: &crate::state::storage_cozo::CozoStorage,
    seed_ids: &HashSet<String>,
    cap: usize,
) -> Result<Vec<String>> {
    let mut current: HashSet<String> = seed_ids.clone();
    let seed_vec: Vec<String> = seed_ids.iter().cloned().collect();

    // First hop.
    let params = id_list_param("ids", &seed_vec);
    let res = cozo.run_script_with_params(
        "?[nid] := *edge{source: s, target: nid}, s in $ids \\n\
         ?[nid] := *edge{source: nid, target: s}, s in $ids",
        params,
        cozo::ScriptMutability::Immutable,
    )?;
    for row in &res.rows {
        if let Some(cozo::DataValue::Str(id)) = row.first() {
            current.insert(id.to_string());
            if current.len() >= cap {
                break;
            }
        }
    }

    // Second hop, using the expanded first-hop set.
    let first_hop: Vec<String> = current.iter().cloned().collect();
    let params = id_list_param("ids", &first_hop);
    let res = cozo.run_script_with_params(
        "?[nid] := *edge{source: s, target: nid}, s in $ids \\n\
         ?[nid] := *edge{source: nid, target: s}, s in $ids",
        params,
        cozo::ScriptMutability::Immutable,
    )?;
    for row in &res.rows {
        if let Some(cozo::DataValue::Str(id)) = row.first() {
            current.insert(id.to_string());
            if current.len() >= cap {
                break;
            }
        }
    }

    let mut ids: Vec<String> = current.into_iter().collect();
    ids.sort_unstable();
    Ok(ids)
}

fn fetch_top_risk_nodes(
    cozo: &crate::state::storage_cozo::CozoStorage,
    limit: usize,
) -> Result<Vec<String>> {
    let query = format!(
        "?[id, risk_score] := *node{{id, risk_score}} \\n         :order -risk_score \\n         :limit {}",
        limit
    );
    let res = cozo.run_script(&query)?;
    let mut ids = Vec::new();
    for row in &res.rows {
        if let Some(cozo::DataValue::Str(id)) = row.first() {
            ids.push(id.to_string());
            if ids.len() >= limit {
                break;
            }
        }
    }
    Ok(ids)
}

fn fetch_node_details(
    cozo: &crate::state::storage_cozo::CozoStorage,
    ids: &[String],
) -> Result<Vec<KgNode>> {
    let params = id_list_param("ids", ids);
    let res = cozo.run_script_with_params(
        "?[id, label, category, risk_score, metadata] := \
         *node{id, label, category, risk_score, metadata}, id in $ids",
        params,
        cozo::ScriptMutability::Immutable,
    )?;

    let mut nodes = Vec::with_capacity(res.rows.len());
    for row in &res.rows {
        if let (
            Some(cozo::DataValue::Str(id)),
            Some(cozo::DataValue::Str(label)),
            Some(cozo::DataValue::Str(category)),
            Some(cozo::DataValue::Num(cozo::Num::Float(risk_score))),
            maybe_meta,
        ) = (row.first(), row.get(1), row.get(2), row.get(3), row.get(4))
        {
            let metadata = maybe_meta.and_then(|v| match v {
                cozo::DataValue::Json(val) => serde_json::to_value(val).ok(),
                _ => None,
            });
            let file_path = node_file_path(category, id, label, &metadata);
            let complexity = node_complexity(&metadata);
            nodes.push(KgNode {
                id: id.to_string(),
                label: label.to_string(),
                category: category.to_string(),
                risk_score: *risk_score,
                file_path,
                complexity,
                metadata,
            });
        }
    }
    Ok(nodes)
}

/// Derive a displayable file path for a knowledge-graph node from its metadata
/// or URN. File nodes use their identifier as the path; everything else falls
/// back to any `source_file` recorded in metadata.
fn node_file_path(
    category: &str,
    id: &str,
    _label: &str,
    metadata: &Option<serde_json::Value>,
) -> String {
    if let Some(m) = metadata
        && let Some(s) = m.get("source_file").and_then(|v| v.as_str())
    {
        return s.to_string();
    }
    if category == "file"
        && let Some(suffix) = id.strip_prefix("urn:ledgerful:file:")
    {
        return suffix.to_string();
    }
    String::new()
}

fn node_complexity(metadata: &Option<serde_json::Value>) -> i32 {
    metadata
        .as_ref()
        .and_then(|m| m.get("complexity").and_then(|v| v.as_i64()))
        .unwrap_or(0) as i32
}

fn fetch_edges_among(
    cozo: &crate::state::storage_cozo::CozoStorage,
    ids: &[String],
) -> Result<Vec<KgEdge>> {
    let params = id_list_param("ids", ids);
    let res = cozo.run_script_with_params(
        "?[source, target, relation, confidence, provenance_id] := \
         *edge{source, target, relation, confidence, provenance_id}, \
         source in $ids, target in $ids",
        params,
        cozo::ScriptMutability::Immutable,
    )?;

    let mut edges = Vec::with_capacity(res.rows.len());
    for row in &res.rows {
        if let (
            Some(cozo::DataValue::Str(source)),
            Some(cozo::DataValue::Str(target)),
            Some(cozo::DataValue::Str(relation)),
            confidence,
            provenance,
        ) = (row.first(), row.get(1), row.get(2), row.get(3), row.get(4))
        {
            let confidence = confidence.and_then(|v| match v {
                cozo::DataValue::Num(cozo::Num::Float(f)) => Some(*f),
                _ => None,
            });
            let provenance_id = provenance.and_then(|v| match v {
                cozo::DataValue::Str(s) => Some(s.to_string()),
                _ => None,
            });
            edges.push(KgEdge {
                source: source.to_string(),
                target: target.to_string(),
                relation: relation.to_string(),
                confidence,
                provenance_id,
            });
        }
    }
    Ok(edges)
}

// ---------------------------------------------------------------------------
// Compliance dashboard endpoints (Track E2)
// ---------------------------------------------------------------------------

/// Maximum number of signature rows returned by `/api/compliance/signatures`.
/// Bound to keep the payload bounded for the dashboard table; the ledger is
/// ordered `committed_at DESC` so this is the most recent 100 entries.
const COMPLIANCE_SIGNATURES_LIMIT: usize = 100;

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

/// `GET /api/compliance/summary` — aggregate compliance summary.
#[utoipa::path(
    get,
    path = "/api/compliance/summary",
    operation_id = "getComplianceSummary",
    tag = "compliance",
    responses(
        (status = 200, description = "Compliance summary", body = ComplianceSummaryResponse)
    )
)]
pub async fn compliance_summary_handler(
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, WebError> {
    let layout = state.layout.clone();
    let response = tokio::task::spawn_blocking(move || fetch_compliance_summary(&layout))
        .await
        .map_err(|e| WebError::Internal(format!("Background task failed: {e}")))?
        .map_err(|e| WebError::Internal(format!("Failed to read compliance summary: {e}")))?;
    Ok(Json(response))
}

fn fetch_compliance_summary(layout: &Layout) -> Result<ComplianceSummaryResponse> {
    let db_path = layout.state_subdir().join("ledger.db");
    if !db_path.exists() {
        return Ok(ComplianceSummaryResponse {
            total_signed: 0,
            validity_percent: 0.0,
            last_audit_at: None,
            hotspot_delta_percent: 0.0,
        });
    }

    let storage = StorageManager::open_read_only_sqlite_only(&layout.root)?;
    let conn = storage.get_connection();

    // Load config to determine whether signing is required (mirrors
    // `verify_ledger_signatures` in `src/commands/verify.rs`).
    let require_signing = load_config(layout)
        .unwrap_or_default()
        .intent
        .require_signing;

    // `get_all_committed_ledger_entries` returns entries ordered
    // `committed_at ASC` (see `src/ledger/db/transactions.rs:349`). Sort
    // defensively into DESC order so `last_audit_at` (the most recent VALID
    // entry's `committed_at`) and downstream consumers are deterministic.
    let mut entries = {
        let db = LedgerDb::new(conn);
        db.get_all_committed_ledger_entries()
            .map_err(|e| miette!("Failed to read ledger entries: {e}"))?
    };
    entries.sort_by(|a, b| b.committed_at.cmp(&a.committed_at));

    let mut total_signed: u64 = 0;
    let mut valid_count: u64 = 0;
    let mut last_audit_at: Option<String> = None;

    for entry in &entries {
        let status = classify_signature(entry, require_signing);
        if entry.signature.is_some() && entry.public_key.is_some() {
            total_signed += 1;
        }
        // Entries are now DESC, so the first VALID entry is the most recent
        // verifiably-signed event.
        if status == SignatureStatus::Valid && last_audit_at.is_none() {
            last_audit_at = Some(entry.committed_at.clone());
        }
        if status == SignatureStatus::Valid {
            valid_count += 1;
        }
    }

    let total_entries = entries.len() as u64;
    let validity_percent = if total_entries == 0 {
        0.0
    } else {
        round_2dp((valid_count as f64 / total_entries as f64) * 100.0)
    };

    let hotspot_delta_percent = fetch_hotspot_delta_percent(&storage)?;

    Ok(ComplianceSummaryResponse {
        total_signed,
        validity_percent,
        last_audit_at,
        hotspot_delta_percent,
    })
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

/// `GET /api/compliance/signatures` — recent signature status entries.
#[utoipa::path(
    get,
    path = "/api/compliance/signatures",
    operation_id = "getComplianceSignatures",
    tag = "compliance",
    responses(
        (status = 200, description = "Compliance signature entries", body = [ComplianceSignatureEntry])
    )
)]
pub async fn compliance_signatures_handler(
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, WebError> {
    let layout = state.layout.clone();
    let entries = tokio::task::spawn_blocking(move || fetch_compliance_signatures(&layout))
        .await
        .map_err(|e| WebError::Internal(format!("Background task failed: {e}")))?
        .map_err(|e| WebError::Internal(format!("Failed to read compliance signatures: {e}")))?;
    Ok(Json(entries))
}

fn fetch_compliance_signatures(layout: &Layout) -> Result<Vec<ComplianceSignatureEntry>> {
    let db_path = layout.state_subdir().join("ledger.db");
    if !db_path.exists() {
        return Ok(Vec::new());
    }

    let storage = StorageManager::open_read_only_sqlite_only(&layout.root)?;
    let conn = storage.get_connection();
    let require_signing = load_config(layout)
        .unwrap_or_default()
        .intent
        .require_signing;

    // `get_all_committed_ledger_entries` returns `committed_at ASC`
    // (`src/ledger/db/transactions.rs:349`). Sort into DESC order so the
    // most recent entries come first, then bound to the most recent
    // `COMPLIANCE_SIGNATURES_LIMIT` (100). `.take(100)` preserves this DESC
    // order, so no re-sort is needed after mapping into the response shape.
    let mut entries = {
        let db = LedgerDb::new(conn);
        db.get_all_committed_ledger_entries()
            .map_err(|e| miette!("Failed to read ledger entries: {e}"))?
    };
    entries.sort_by(|a, b| b.committed_at.cmp(&a.committed_at));

    let out: Vec<ComplianceSignatureEntry> = entries
        .into_iter()
        .take(COMPLIANCE_SIGNATURES_LIMIT)
        .map(|entry| {
            let status = classify_signature(&entry, require_signing);
            ComplianceSignatureEntry {
                tx_id: entry.tx_id,
                entity: display_entity(&entry.entity),
                summary: entry.summary,
                committed_at: entry.committed_at,
                status: status.as_str().to_string(),
                category: entry.category.to_string(),
            }
        })
        .collect();
    Ok(out)
}

/// Classification of a ledger entry's signature status, mirroring
/// `verify_ledger_signatures` in `src/commands/verify.rs:43-91`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SignatureStatus {
    Valid,
    Invalid,
    Skipped,
}

impl SignatureStatus {
    fn as_str(&self) -> &'static str {
        match self {
            SignatureStatus::Valid => "VALID",
            SignatureStatus::Invalid => "INVALID",
            SignatureStatus::Skipped => "SKIPPED",
        }
    }
}

fn classify_signature(entry: &LedgerEntry, require_signing: bool) -> SignatureStatus {
    match (&entry.signature, &entry.public_key) {
        (Some(sig), Some(pub_key)) => {
            if crate::ledger::crypto::verify_signature(
                &entry.tx_id,
                &entry.category.to_string(),
                &entry.summary,
                &entry.reason,
                &entry.committed_at,
                sig,
                pub_key,
            ) {
                SignatureStatus::Valid
            } else {
                SignatureStatus::Invalid
            }
        }
        _ => {
            if require_signing {
                SignatureStatus::Invalid
            } else {
                SignatureStatus::Skipped
            }
        }
    }
}

/// `hotspotDeltaPercent` — the percent change in total hotspot count between
/// the two most recent `hotspot_history` snapshots.
///
/// Definition: a "snapshot" is the set of `hotspot_history` rows sharing a
/// `timestamp` value (the `hotspot_history` migration at
/// `src/state/migrations/m38_hotspot_history.rs` writes one row per file per
/// scan, all rows in a scan share the scan's timestamp). The "total hotspot
/// count" for a snapshot is `COUNT(*)` of rows with that timestamp — i.e. the
/// number of files flagged as hotspots in that scan.
///
/// Computation: `((newer_total - older_total) / older_total) * 100`, rounded
/// to 2 decimal places, where `newer_total` and `older_total` are the totals
/// for the two most recent distinct timestamps (`ORDER BY timestamp DESC`).
///
/// Persistence is owned by `state/`: the raw `hotspot_history` SQL lives in
/// `StorageManager::get_latest_hotspot_snapshot_totals`, which returns the
/// per-snapshot totals newest-first. This handler does only the percent math.
///
/// Guards:
/// - fewer than 2 distinct snapshots → `0.0` (no trend to report).
/// - `older_total == 0` → `100.0` if `newer_total > 0` else `0.0` (avoids
///   division by zero; a transition from "no hotspots" to "some hotspots" is
///   reported as a 100% increase). This branch is structurally unreachable —
///   a `DISTINCT timestamp` always has `COUNT(*) >= 1` — but is retained as
///   defensive code for future schema changes.
fn fetch_hotspot_delta_percent(storage: &StorageManager) -> Result<f64> {
    let totals = storage.get_latest_hotspot_snapshot_totals(2)?;

    if totals.len() < 2 {
        return Ok(0.0);
    }

    let newer = totals[0];
    let older = totals[1];

    let pct = if older == 0 {
        if newer > 0 { 100.0 } else { 0.0 }
    } else {
        ((newer as f64 - older as f64) / older as f64) * 100.0
    };
    Ok(round_2dp(pct))
}

/// Round a float to 2 decimal places (banker's rounding not required for
/// dashboard aggregates; standard round-half-away-from-zero via `f64::round`).
fn round_2dp(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

// ---------------------------------------------------------------------------
// Track E3: SOC2 Evidence Export
// ---------------------------------------------------------------------------

/// `GET /api/compliance/export` — generate a tamper-evident `.zip` of SOC2
/// evidence on the fly and return it as a binary download.
///
/// The zip contains `manifest.json` (SHA-256 + size of every other file),
/// `manifest.sig` + `manifest.pub` (Ed25519 signature over the manifest
/// bytes and the verifying key, reusing the repo's keypair so the existing
/// offline verifier can validate the export), `ledger.csv` (all committed
/// provenance records), `verification_history.csv` (CI gate pass/fail
/// records), and `adr/*.md` (generated MADR-format ADRs tied to the ledger).
///
/// All SQLite + zip + SHA-256 + Ed25519 work runs inside
/// `tokio::task::spawn_blocking`; the handler only attaches the
/// `application/zip` + `Content-Disposition: attachment` headers to the
/// returned `Response`. Empty / no-DB state still yields a valid zip
/// (header-only CSVs, no `adr/` files, a manifest over the files that
/// exist, and a signature over that manifest) — 200, not an error.
/// `GET /api/compliance/export` — download a tamper-evident SOC2 evidence
/// `.zip`. Returns `application/zip` with `Content-Disposition: attachment`.
#[utoipa::path(
    get,
    path = "/api/compliance/export",
    operation_id = "exportComplianceEvidence",
    tag = "compliance",
    responses(
        (status = 200, description = "SOC2 evidence ZIP", content_type = "application/zip")
    )
)]
pub async fn compliance_export_handler(
    State(state): State<Arc<AppState>>,
) -> Result<axum::response::Response, WebError> {
    let layout = state.layout.clone();
    let zip_bytes =
        tokio::task::spawn_blocking(move || crate::export::soc2::generate_soc2_export(&layout))
            .await
            .map_err(|e| WebError::Internal(format!("Background task failed: {e}")))?
            .map_err(|e| WebError::Internal(format!("Failed to generate SOC2 export: {e}")))?;

    let mut response = axum::response::Response::new(axum::body::Body::from(zip_bytes));
    response.headers_mut().insert(
        axum::http::header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("application/zip"),
    );
    response.headers_mut().insert(
        axum::http::header::CONTENT_DISPOSITION,
        axum::http::HeaderValue::from_static(
            "attachment; filename=\"ledgerful-soc2-evidence.zip\"",
        ),
    );
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn node_file_path_prefers_metadata_source_file() {
        let meta = Some(json!({ "source_file": "policies/auth.cedar" }));
        assert_eq!(
            node_file_path("policy", "urn:ledgerful:policy:x", "auth", &meta),
            "policies/auth.cedar"
        );
    }

    #[test]
    fn node_file_path_derives_from_file_urn() {
        assert_eq!(
            node_file_path("file", "urn:ledgerful:file:src/lib.rs", "src/lib.rs", &None),
            "src/lib.rs"
        );
    }

    #[test]
    fn node_file_path_returns_empty_when_unavailable() {
        assert_eq!(
            node_file_path("service", "urn:ledgerful:service:svc", "svc", &None),
            ""
        );
    }

    #[test]
    fn node_complexity_reads_metadata_integer() {
        let meta = Some(json!({ "complexity": 42 }));
        assert_eq!(node_complexity(&meta), 42);
    }

    #[test]
    fn node_complexity_defaults_to_zero() {
        assert_eq!(node_complexity(&None), 0);
    }

    #[test]
    fn friendly_step_name_none_falls_back_to_command() {
        assert_eq!(friendly_step_name(None, "cargo test"), "cargo test");
    }

    #[test]
    fn friendly_step_name_clean_description_kept_verbatim() {
        // Real clean labels produced by the plan builder have no " | " separator.
        assert_eq!(
            friendly_step_name(Some("Default: run project tests"), "cargo test"),
            "Default: run project tests"
        );
        assert_eq!(
            friendly_step_name(Some("From rules: cargo build"), "cargo build"),
            "From rules: cargo build"
        );
    }

    #[test]
    fn friendly_step_name_strips_predicted_impact_annotations() {
        // The plan builder ` | `-concatenates one "Predicted impact (...) on
        // <file>" segment per predicted affected file onto a friendly label.
        // Only the first (friendly) segment should reach the dashboard.
        let blob = "From rules: cargo clippy --all-targets --all-features -- -D warnings \
            | Predicted impact (CallGraph) on src/commands/ask.rs \
            | Predicted impact (Temporal) on src/bridge/mod.rs";
        assert_eq!(
            friendly_step_name(
                Some(blob),
                "cargo clippy --all-targets --all-features -- -D warnings"
            ),
            "From rules: cargo clippy --all-targets --all-features -- -D warnings"
        );
    }

    #[test]
    fn friendly_step_name_predicted_only_falls_back_to_command() {
        // A command that only ever appeared via predicted-impact rules has no
        // friendly prefix at all — its description STARTS with "Predicted
        // impact", so the first segment is not a usable label.
        let blob = "Predicted impact (Temporal) on src/bridge/export.rs \
            | Predicted impact (Temporal) on src/bridge/mod.rs";
        assert_eq!(
            friendly_step_name(
                Some(blob),
                "cargo test --test integration -- --test-threads=1"
            ),
            "cargo test --test integration -- --test-threads=1"
        );
    }

    #[test]
    fn friendly_step_name_empty_description_falls_back_to_command() {
        assert_eq!(
            friendly_step_name(Some("   "), "cargo fmt --check"),
            "cargo fmt --check"
        );
    }
}

// ---------------------------------------------------------------------------
// HotspotResponse DTO unit tests (Track TA29)
// ---------------------------------------------------------------------------

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
        let hotspots = vec![make_hotspot("src/héllo.rs", 0.5, 3.0, 1.0)];
        let git_meta = HashMap::new();
        let responses = map_hotspots_to_responses(&hotspots, &git_meta);
        assert_eq!(responses[0].file_path, "src/héllo.rs");
    }

    // Suppress unused import warning — Utf8PathBuf is used in production code
    // outside tests but we import it here for completeness.
    #[test]
    fn _utf8_path_buf_import_marker() {
        let _ = Utf8PathBuf::from("marker");
    }
}
