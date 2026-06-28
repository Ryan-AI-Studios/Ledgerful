//! Web dashboard axum router and server.

use crate::commands::helpers::load_ledger_config;
use crate::commands::web::api;
use crate::commands::web::api::{HotspotResponse, map_hotspots_to_responses};
use crate::commands::web::auth::{extract_token_header, extract_token_query, validate_token};
use crate::commands::web::error::WebError;
use crate::commands::web::git_meta::{build_git_metadata_map, git_meta_cache_needs_refresh};
use crate::commands::web::state::AppState;
use crate::config::model::Config;
use crate::git::repo::open_repo;
use crate::impact::hotspots::{HotspotQuery, calculate_hotspots};
use crate::impact::packet::Hotspot;
use crate::impact::temporal::GixHistoryProvider;
use crate::ledger::db::LedgerDb;
use crate::ledger::error::LedgerError;
use crate::ledger::types::LedgerEntry;
use crate::state::layout::Layout;
use crate::state::reports::LATEST_IMPACT_REPORT;
use crate::state::storage::StorageManager;
use axum::extract::{ConnectInfo, Path, Query, Request, State};
use axum::http::{HeaderValue, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use miette::{IntoDiagnostic, Result, miette};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::net::TcpListener;
use tower_http::cors::{AllowOrigin, CorsLayer};
use tower_http::services::{ServeDir, ServeFile};
use tower_http::trace::{DefaultMakeSpan, DefaultOnResponse, TraceLayer};
use tracing::Level;

/// Build the axum router for the Ledgerful web dashboard.
pub fn router(state: Arc<AppState>) -> Router {
    let api_router = Router::new()
        .route("/session", get(session_handler))
        .route("/snapshot", get(snapshot_handler))
        .route("/status", get(status_handler))
        .route("/projects", get(projects_handler))
        .route("/ledger", get(ledger_handler))
        .route("/ledger/search", get(ledger_search_handler))
        .route("/ledger/{tx_id}", get(ledger_tx_handler))
        .route("/changes", get(changes_handler))
        .route("/hotspots", get(hotspots_handler))
        .route("/hotspots/trend", get(api::hotspots_trend_handler))
        .route(
            "/reports/latest-impact.json",
            get(api::latest_impact_handler),
        )
        .route(
            "/reports/latest-verify.json",
            get(api::latest_verify_handler),
        )
        .route("/verify/health", get(api::verify_health_handler))
        .route("/verify/history", get(api::verify_history_handler))
        .route("/verify/steps", get(api::verify_steps_handler))
        .route("/compliance/summary", get(api::compliance_summary_handler))
        .route(
            "/compliance/signatures",
            get(api::compliance_signatures_handler),
        )
        .route("/compliance/export", get(api::compliance_export_handler))
        .route("/endpoints/changed", get(api::endpoints_changed_handler))
        .route(
            "/security/boundaries",
            get(api::security_boundaries_handler),
        )
        .route("/knowledge-graph", get(api::knowledge_graph_handler))
        .route("/config", get(config_handler))
        .route_layer(middleware::from_fn_with_state(state.clone(), token_layer));

    #[cfg(feature = "sync")]
    let api_router = api_router.route("/sync/status", get(sync_status_handler));

    let mut app = Router::new()
        .route("/health", get(health_handler))
        .nest("/api", api_router);

    if let Some(spa_dir) = &state.spa_dir {
        let fallback = ServeFile::new(spa_dir.join("index.html").as_std_path());
        app = app.fallback_service(ServeDir::new(spa_dir.as_std_path()).fallback(fallback));
    } else {
        app = app.fallback(get(embedded_spa_handler));
    }

    app.layer(middleware::from_fn(server_header_middleware))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            rate_limit_layer,
        ))
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(
                    DefaultMakeSpan::new()
                        .level(Level::INFO)
                        .include_headers(false),
                )
                .on_response(
                    DefaultOnResponse::new()
                        .level(Level::INFO)
                        .include_headers(false),
                ),
        )
        .layer(local_cors())
        .with_state(state)
}

/// Restrict CORS to local dashboard origins. The production SPA is served from
/// the same origin, so this primarily supports the Next.js dev server on
/// http://localhost:3001 / http://127.0.0.1:3001 and manual local testing.
fn local_cors() -> CorsLayer {
    CorsLayer::new().allow_origin(AllowOrigin::predicate(
        |origin: &HeaderValue, _parts: &axum::http::request::Parts| {
            let bytes = origin.as_bytes();
            bytes.starts_with(b"http://localhost:")
                || bytes.starts_with(b"http://127.0.0.1:")
                || bytes.starts_with(b"https://localhost:")
                || bytes.starts_with(b"https://127.0.0.1:")
        },
    ))
}

/// Layer that requires a valid session token for all nested routes.
async fn token_layer(
    State(state): State<Arc<AppState>>,
    request: Request,
    next: Next,
) -> Result<Response, WebError> {
    let parts = request.into_parts();
    let provided = extract_token_query(&parts.0).or_else(|| extract_token_header(&parts.0));
    validate_token(provided, &state.token)?;

    let request = Request::from_parts(parts.0, parts.1);
    Ok(next.run(request).await)
}

async fn health_handler() -> impl IntoResponse {
    Json(json!({ "status": "ok" }))
}

#[derive(Serialize)]
struct SnapshotResponse {
    project_id: String,
    overall_risk: String,
    pending_transactions: usize,
    unaudited_drift: usize,
    indexed_documents: usize,
    graph_nodes: usize,
    graph_edges: usize,
    last_audit: Option<String>,
    top_hotspots: Vec<serde_json::Value>,
    recent_changes: Vec<serde_json::Value>,
}

async fn snapshot_handler(
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, WebError> {
    let layout = state.layout.clone();
    let snapshot = tokio::task::spawn_blocking(move || compute_snapshot(&layout))
        .await
        .map_err(|e| WebError::Internal(format!("Background task failed: {}", e)))?
        .unwrap_or_else(|e| {
            tracing::warn!("Failed to compute snapshot: {}", e);
            empty_snapshot(&state.layout)
        });

    Ok(Json(snapshot))
}

fn empty_snapshot(layout: &Layout) -> SnapshotResponse {
    SnapshotResponse {
        project_id: layout.get_project_id(),
        overall_risk: "low".to_string(),
        pending_transactions: 0,
        unaudited_drift: 0,
        indexed_documents: 0,
        graph_nodes: 0,
        graph_edges: 0,
        last_audit: None,
        top_hotspots: Vec::new(),
        recent_changes: Vec::new(),
    }
}

fn compute_snapshot(layout: &Layout) -> Result<SnapshotResponse> {
    let pending_transactions = count_pending_transactions(layout).unwrap_or(0);
    let unaudited_drift = count_unaudited_transactions(layout).unwrap_or(0);
    let indexed_documents = count_indexed_documents(layout);

    let recent_changes: Vec<serde_json::Value> = fetch_changes(layout, 7, false)
        .unwrap_or_default()
        .into_iter()
        .take(10)
        .map(|c| {
            serde_json::json!({
                "path": c.path,
                "status": c.status,
                "summary": c.summary,
                "author": c.author,
                "timeAgo": c.time_ago,
                "fileCount": c.file_count,
                "risk": c.risk,
            })
        })
        .collect();

    let top_hotspots: Vec<serde_json::Value> = fetch_hotspots(layout, Some(10), None)
        .unwrap_or_default()
        .into_iter()
        .map(|h| {
            serde_json::json!({
                "filePath": h.path.to_string_lossy(),
                "score": h.display_score,
                "complexity": h.complexity,
                "frequency": h.frequency,
            })
        })
        .collect();

    let overall_risk = if unaudited_drift > 0 || pending_transactions > 5 {
        "high"
    } else if pending_transactions > 0 {
        "medium"
    } else {
        "low"
    };

    Ok(SnapshotResponse {
        project_id: layout.get_project_id(),
        overall_risk: overall_risk.to_string(),
        pending_transactions,
        unaudited_drift,
        indexed_documents,
        graph_nodes: 0,
        graph_edges: 0,
        last_audit: None,
        top_hotspots,
        recent_changes,
    })
}

fn count_indexed_documents(layout: &Layout) -> usize {
    layout
        .search_index_dir()
        .read_dir()
        .map(|entries| entries.filter_map(|e| e.ok()).count())
        .unwrap_or(0)
}

#[derive(Serialize)]
struct StatusResponse {
    index_ready: bool,
    graph_ready: bool,
    pending_transactions: usize,
    unaudited_drift: usize,
    embedding_model_reachable: bool,
    completion_model_reachable: bool,
}

async fn status_handler(State(state): State<Arc<AppState>>) -> Result<impl IntoResponse, WebError> {
    let layout = state.layout.clone();
    let status = tokio::task::spawn_blocking(move || compute_status(&layout))
        .await
        .map_err(|e| WebError::Internal(format!("Background task failed: {}", e)))?
        .map_err(|e| WebError::Internal(format!("Failed to compute status: {}", e)))?;
    Ok(Json(status))
}

fn compute_status(layout: &Layout) -> Result<StatusResponse> {
    let pending_transactions = count_pending_transactions(layout)?;
    let unaudited_drift = count_unaudited_transactions(layout)?;
    let config = load_ledger_config(layout).unwrap_or_default();

    let index_ready = layout
        .search_index_dir()
        .read_dir()
        .map(|mut d| d.next().is_some())
        .unwrap_or(false);
    let graph_ready = layout.state_subdir().join("ledger.cozo").exists();

    Ok(StatusResponse {
        index_ready,
        graph_ready,
        pending_transactions,
        unaudited_drift,
        embedding_model_reachable: model_reachable(
            config
                .local_model
                .embedding_url
                .as_deref()
                .or_else(|| config.local_model.base_url.as_str().non_empty()),
        ),
        completion_model_reachable: model_reachable(
            config
                .local_model
                .generation_url
                .as_deref()
                .or_else(|| config.local_model.base_url.as_str().non_empty()),
        ),
    })
}

fn model_reachable(url: Option<&str>) -> bool {
    match url {
        Some(u) if !u.is_empty() => {
            crate::util::network::is_url_reachable(u, std::time::Duration::from_millis(500))
        }
        _ => true,
    }
}

trait NonEmptyStr {
    fn non_empty(&self) -> Option<&str>;
}

impl NonEmptyStr for str {
    fn non_empty(&self) -> Option<&str> {
        if self.is_empty() { None } else { Some(self) }
    }
}

fn count_pending_transactions(layout: &Layout) -> Result<usize> {
    let (pending, _) = drift_status(layout)?;
    Ok(pending)
}

fn count_unaudited_transactions(layout: &Layout) -> Result<usize> {
    let (_, unaudited) = drift_status(layout)?;
    Ok(unaudited)
}

fn drift_status(layout: &Layout) -> Result<(usize, usize)> {
    let db_path = layout.state_subdir().join("ledger.db");
    if !db_path.exists() {
        return Ok((0, 0));
    }

    let conn = open_ledger_connection(db_path.as_std_path())?;
    let db = LedgerDb::new(&conn);
    db.drift_status_counts()
        .map_err(|e| miette!("Failed to query drift status: {}", e))
}

#[derive(Serialize)]
struct ProjectResponse {
    id: String,
    name: String,
    path: String,
    status: String,
    last_scan_at: Option<String>,
    health_score: u8,
    /// TA31 R1: per-sibling validation warnings (e.g. an empty ledger
    /// `entity`). Always present (empty for the local/root project and
    /// for fully-valid siblings) so the frontend can render a "needs
    /// attention" badge without an `Option` round-trip. This is
    /// additive/non-breaking relative to the pre-TA31 DTO shape.
    validation_warnings: Vec<String>,
}

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
fn compute_health_score(layout: &Layout) -> (u8, Option<String>) {
    let report_path = layout.reports_dir().join(LATEST_IMPACT_REPORT);
    let (risk_penalty, last_scan_at) =
        match crate::state::reports::read_latest_impact_report(layout) {
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
fn project_status_from_score(score: u8) -> &'static str {
    if score >= 80 {
        "healthy"
    } else if score >= 50 {
        "warning"
    } else {
        "critical"
    }
}

async fn projects_handler(
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, WebError> {
    let layout = state.layout.clone();
    let projects = tokio::task::spawn_blocking(move || {
        let (health_score, last_scan_at) = compute_health_score(&layout);
        let status = project_status_from_score(health_score).to_string();

        let mut projects = Vec::new();
        projects.push(ProjectResponse {
            id: layout.get_project_id(),
            name: layout
                .root
                .file_name()
                .map_or_else(|| "unknown".to_string(), |n| n.to_string()),
            path: layout.root.to_string(),
            status,
            last_scan_at,
            health_score,
            validation_warnings: Vec::new(),
        });

        // Optionally discover sibling projects via federation. The scan is bounded
        // and failures are ignored so this read-only endpoint never fails because
        // of slow or broken sibling discovery.
        //
        // Sibling health is intentionally not derivable in M8: we do not have
        // an impact report for them and we do not run a doctor probe against
        // their repos. Reporting `status: "healthy", health_score: 100` would
        // be the exact hardcoded defaults the M8 spec called out as the
        // problem this track is meant to fix. Instead, siblings emit
        // `status: "unknown", health_score: 0, last_scan_at: None` so the
        // dashboard can render "health not available" rather than "healthy"
        // — a future track (FU-1 in `conductor/trackM8/spec.md`) can wire
        // up real sibling health probes when the protocol supports it.
        let scanner =
            crate::federated::scanner::FederatedScanner::new(layout.root.clone()).with_limit(5);
        if let Ok((siblings, _warnings)) = scanner.scan_siblings() {
            for (path, schema, validation_warnings) in siblings {
                projects.push(ProjectResponse {
                    id: schema.repo_name.clone(),
                    name: schema.repo_name,
                    path: path.to_string(),
                    status: "unknown".to_string(),
                    last_scan_at: None,
                    health_score: 0,
                    validation_warnings,
                });
            }
        }
        projects
    })
    .await
    .map_err(|e| WebError::Internal(format!("Background task failed: {}", e)))?;

    Ok(Json(projects))
}

#[cfg(feature = "sync")]
#[derive(Serialize)]
struct SyncStatusResponse {
    device_id: Option<String>,
    last_extract_at: Option<String>,
    last_apply_at: Option<String>,
    last_run_at: Option<String>,
}

#[cfg(feature = "sync")]
async fn sync_status_handler(
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, WebError> {
    let layout = state.layout.clone();
    let status = tokio::task::spawn_blocking(move || {
        let db_path = layout.state_subdir().join("ledger.db");
        if !db_path.exists() {
            return empty_sync_status();
        }

        // Open the ledger SQLite read-only via StorageManager. WAL mode
        // (set by `StorageManager::init` and `open_read_only_*` via
        // `PRAGMA journal_mode = WAL`) plus `PRAGMA busy_timeout = 5000`
        // give us safe read concurrency with concurrent `ledger commit`
        // writers — a separate `Connection` per request is fine under WAL.
        // We don't reuse a long-lived connection from the request scope
        // because SQLite WAL allows many concurrent readers cheaply and
        // a per-request connection is the simplest correct model.
        let storage = match StorageManager::open_read_only_sqlite_only(&layout.root) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(
                    "/api/sync/status: cannot open read-only storage for sync state: {}",
                    e
                );
                return empty_sync_status();
            }
        };
        let conn = storage.get_connection();
        match crate::sync::state::SyncState::load(conn) {
            Ok(Some(sync_state)) => SyncStatusResponse {
                device_id: if sync_state.device_id.is_empty() {
                    None
                } else {
                    Some(sync_state.device_id)
                },
                last_extract_at: hlc_to_iso8601(sync_state.last_extract_hlc.as_ref()),
                last_apply_at: hlc_to_iso8601(sync_state.last_apply_hlc.as_ref()),
                last_run_at: sync_state.last_run_at.map(|dt| dt.to_rfc3339()),
            },
            Ok(None) => empty_sync_status(),
            Err(e) => {
                tracing::warn!("/api/sync/status: SyncState::load failed: {}", e);
                empty_sync_status()
            }
        }
    })
    .await
    .map_err(|e| WebError::Internal(format!("Background task failed: {}", e)))?;

    Ok(Json(status))
}

#[cfg(feature = "sync")]
fn empty_sync_status() -> SyncStatusResponse {
    SyncStatusResponse {
        device_id: None,
        last_extract_at: None,
        last_apply_at: None,
        last_run_at: None,
    }
}

/// Convert an HLC's `physical_ms` field to an ISO 8601 (RFC 3339) string.
///
/// Returns `None` if:
/// - the HLC itself is `None`,
/// - the physical timestamp is `0` (the uninitialized sentinel —
///   `HLC::now` would never produce this since the wall clock is well
///   past epoch, so `0` only appears in a corrupted or seed-injected
///   `sync_state` row, and emitting `1970-01-01T00:00:00Z` would be
///   misleading per M8 opencode-review M2), or
/// - the physical timestamp does not fit in a `chrono::DateTime` (e.g.
///   far-past or far-future values that overflow `i64` seconds, or
///   pre-1970 timestamps).
///
/// Per the M8 spec (`conductor/trackM8/spec.md:60`), the response
/// field is expected to be a clean ISO 8601 date — emitting a raw
/// `u64` ms integer in a date field would be a worse contract than
/// omitting the field entirely.
#[cfg(feature = "sync")]
fn hlc_to_iso8601(hlc: Option<&crate::sync::hlc::HLC>) -> Option<String> {
    let hlc = hlc?;
    let ms = hlc.physical_ms;
    if ms == 0 {
        return None;
    }
    let secs = i64::try_from(ms / 1000).ok()?;
    let nsecs = u32::try_from((ms % 1000) * 1_000_000).ok()?;
    chrono::DateTime::from_timestamp(secs, nsecs).map(|dt| dt.to_rfc3339())
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
struct LedgerEntryResponse {
    id: i64,
    tx_id: String,
    category: String,
    entry_type: String,
    entity: String,
    entity_normalized: String,
    change_type: String,
    summary: String,
    reason: String,
    is_breaking: bool,
    committed_at: String,
    verification_status: Option<String>,
    verification_basis: Option<String>,
    outcome_notes: Option<String>,
    origin: String,
    trace_id: Option<String>,
    signature: Option<String>,
    public_key: Option<String>,
    risk: Option<String>,
    related_tickets: Option<String>,
    author: String,
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
struct LedgerDetailResponse {
    #[serde(flatten)]
    base: LedgerEntryResponse,
    files: Vec<ChangedFileResponse>,
    hotspots_crossed: usize,
    tests_run: usize,
    flakes: usize,
}

#[derive(Serialize)]
struct ChangedFileResponse {
    path: String,
    additions: i64,
    deletions: i64,
}

#[derive(Debug, Deserialize, Default)]
struct LedgerListQuery {
    category: Option<String>,
    limit: Option<usize>,
    offset: Option<usize>,
}

async fn ledger_handler(
    State(state): State<Arc<AppState>>,
    Query(params): Query<LedgerListQuery>,
) -> Result<impl IntoResponse, WebError> {
    let layout = state.layout.clone();
    let category = params.category.clone();
    let limit = params.limit;
    let offset = params.offset.unwrap_or(0);
    let entries = tokio::task::spawn_blocking(move || {
        fetch_ledger_entries(&layout, category.as_deref(), limit, offset)
    })
    .await
    .map_err(|e| WebError::Internal(format!("Background task failed: {}", e)))?
    .map_err(|e| WebError::Internal(format!("Failed to fetch ledger entries: {}", e)))?;
    Ok(Json(entries))
}

fn fetch_ledger_entries(
    layout: &Layout,
    category: Option<&str>,
    limit: Option<usize>,
    offset: usize,
) -> Result<Vec<LedgerEntryResponse>> {
    let db_path = layout.state_subdir().join("ledger.db");
    if !db_path.exists() {
        return Ok(Vec::new());
    }

    let conn = open_ledger_connection(db_path.as_std_path())?;
    let db = LedgerDb::new(&conn);
    let entries = db
        .get_committed_ledger_entries_paginated(category, limit.unwrap_or(1000), offset)
        .map_err(|e| miette!("Failed to query ledger entries: {}", e))?;

    Ok(entries.into_iter().map(LedgerEntryResponse::from).collect())
}

async fn ledger_tx_handler(
    State(state): State<Arc<AppState>>,
    Path(tx_id): Path<String>,
) -> Result<impl IntoResponse, WebError> {
    let layout = state.layout.clone();
    let entry = tokio::task::spawn_blocking(move || fetch_ledger_entry(&layout, &tx_id))
        .await
        .map_err(|e| WebError::Internal(format!("Background task failed: {}", e)))?
        .map_err(|e| WebError::Internal(format!("Failed to fetch ledger entry: {}", e)))?;

    match entry {
        Some(e) => Ok(Json(e)),
        None => Err(WebError::NotFound),
    }
}

fn fetch_ledger_entry(layout: &Layout, tx_id: &str) -> Result<Option<LedgerDetailResponse>> {
    let db_path = layout.state_subdir().join("ledger.db");
    if !db_path.exists() {
        return Ok(None);
    }

    let conn = open_ledger_connection(db_path.as_std_path())?;
    let db = LedgerDb::new(&conn);
    let entries = db
        .get_ledger_entries_for_tx(tx_id)
        .map_err(|e| miette!("Failed to query ledger entry: {}", e))?;

    let base = match entries.into_iter().next() {
        Some(e) => LedgerEntryResponse::from(e),
        None => return Ok(None),
    };

    // Enrich with files, hotspots_crossed, tests_run, flakes
    let files = match fetch_changed_files(&conn, tx_id) {
        Ok(files) => files,
        Err(e) => {
            tracing::warn!("Failed to fetch changed files for tx {}: {}", tx_id, e);
            Vec::new()
        }
    };
    let hotspots_crossed = count_hotspots_crossed(&conn, &files).unwrap_or_else(|e| {
        tracing::warn!("Failed to count hotspots crossed for tx {}: {}", tx_id, e);
        0
    });
    let (tests_run, flakes) = match fetch_verification_stats(&conn, tx_id) {
        Ok(stats) => stats,
        Err(e) => {
            tracing::warn!("Failed to fetch verification stats for tx {}: {}", tx_id, e);
            (0, 0)
        }
    };

    Ok(Some(LedgerDetailResponse {
        base,
        files,
        hotspots_crossed,
        tests_run,
        flakes,
    }))
}

/// Fetch the list of files changed by a transaction, joined via the
/// canonical `transactions.snapshot_id -> snapshots.id -> changed_files.snapshot_id`
/// path (the same path used by `TransactionManager::get_transaction_files`
/// at `src/ledger/transaction.rs:894-908`).
///
/// Per-file `additions`/`deletions` are surfaced as `0` because the
/// `changed_files` schema (m1_to_m10:24-30) does not track per-file diff
/// stats — only `(id, snapshot_id, path, status, is_staged)`. Adding
/// those columns is a separate track (see `output/m8-review-1.md` C1).
fn fetch_changed_files(
    conn: &rusqlite::Connection,
    tx_id: &str,
) -> Result<Vec<ChangedFileResponse>, LedgerError> {
    let mut stmt = conn
        .prepare(
            "SELECT cf.path
             FROM changed_files cf
             WHERE cf.snapshot_id = (
                 SELECT snapshot_id FROM transactions WHERE tx_id = ?1
             )",
        )
        .map_err(LedgerError::from)?;

    let rows = stmt
        .query_map(rusqlite::params![tx_id], |row| {
            Ok(ChangedFileResponse {
                path: row.get(0)?,
                additions: 0,
                deletions: 0,
            })
        })
        .map_err(LedgerError::from)?;

    let mut files = Vec::new();
    for row in rows {
        files.push(row?);
    }
    Ok(files)
}

/// Count the number of changed files that also appear in the most recent
/// `hotspot_history` snapshot (the same table `fetch_hotspots` reads for
/// the `/api/hotspots` endpoint).
///
/// Performance notes:
/// - M8 review H2: this previously recomputed the full hotspots
///   ranking from git history on every detail-view request, which is
///   a multi-second operation on real repos. The cached
///   `hotspot_history` snapshot is the same data the dashboard's
///   `/api/hotspots` endpoint renders, so reading it here keeps the
///   two views consistent and avoids the O(history) recomputation.
/// - M8 opencode-review L5: this function now takes `&Connection`
///   (reusing the caller's already-open connection) instead of opening
///   a second `Connection` to the same DB file. The
///   `hotspot_history(timestamp)` index from migration m38 makes the
///   `MAX(timestamp)` subquery O(1) — see the `CREATE INDEX IF NOT
///   EXISTS idx_hotspot_history_timestamp` block appended to m38 in
///   this resolve. On any read failure we log a warning and return 0
///   so a transient unavailability is loud, not silent.
fn count_hotspots_crossed(
    conn: &rusqlite::Connection,
    files: &[ChangedFileResponse],
) -> Result<usize, LedgerError> {
    if files.is_empty() {
        return Ok(0);
    }

    let mut stmt = conn
        .prepare(
            "SELECT file_path FROM hotspot_history \
             WHERE timestamp = (SELECT MAX(timestamp) FROM hotspot_history)",
        )
        .map_err(LedgerError::from)?;
    let rows = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(LedgerError::from)?;
    let mut hotspot_paths: std::collections::HashSet<String> = std::collections::HashSet::new();
    for path in rows.flatten() {
        hotspot_paths.insert(path);
    }

    let crossed = files
        .iter()
        .filter(|f| hotspot_paths.contains(&f.path))
        .count();
    Ok(crossed)
}

/// Fetch the `tests_run` and `flakes` counts for a transaction, joined
/// via the new `tx_id` column introduced by migration m45.
///
/// Before m45, `verification_runs` had no `tx_id` column at all, so the
/// spec's "join against `verification_runs` keyed by `tx_id`" was
/// impossible. m45 adds a nullable `tx_id` to both
/// `verification_runs` and `verification_results`; pre-M8 rows leave the
/// column NULL and contribute zero to the count, which is the
/// "honest zero" the spec calls for (rather than a confusing
/// non-zero from a mis-keyed join).
///
/// Per M8 opencode-review L1:
/// - `tests_run` counts the number of `verification_results` rows
///   (individual test/build commands) tied to this tx, not the
///   number of `verification_runs` (invocations).
/// - `flakes` counts the number of `verification_runs` invocations
///   with `overall_pass = 0` (i.e., the run as a whole failed),
///   so a single failing invocation that drove many commands is
///   reported as a single flake at the run level.
fn fetch_verification_stats(
    conn: &rusqlite::Connection,
    tx_id: &str,
) -> Result<(usize, usize), LedgerError> {
    let mut stmt = conn
        .prepare(
            "SELECT
                COALESCE((SELECT COUNT(*) FROM verification_results WHERE tx_id = ?1), 0)
                  AS tests_run,
                COALESCE((SELECT COUNT(*) FROM verification_runs
                          WHERE tx_id = ?1 AND overall_pass = 0), 0)
                  AS flakes
             ",
        )
        .map_err(LedgerError::from)?;

    let result = stmt
        .query_row(rusqlite::params![tx_id], |row| {
            let tests_run: i64 = row.get(0)?;
            let flakes: i64 = row.get(1)?;
            Ok((tests_run as usize, flakes as usize))
        })
        .map_err(LedgerError::from)?;

    Ok(result)
}

#[derive(Debug, Deserialize, Default)]
struct LedgerSearchQuery {
    q: Option<String>,
    days: Option<u64>,
    limit: Option<usize>,
    offset: Option<usize>,
}

async fn ledger_search_handler(
    State(state): State<Arc<AppState>>,
    Query(params): Query<LedgerSearchQuery>,
) -> Result<impl IntoResponse, WebError> {
    let query = match params.q {
        Some(q) if !q.is_empty() => q,
        _ => {
            return Err(WebError::BadRequest(
                "Missing search query parameter 'q'".to_string(),
            ));
        }
    };

    let layout = state.layout.clone();
    let entries = tokio::task::spawn_blocking(move || {
        search_ledger_entries(
            &layout,
            &query,
            params.days,
            params.limit,
            params.offset.unwrap_or(0),
        )
    })
    .await
    .map_err(|e| WebError::Internal(format!("Background task failed: {}", e)))?
    .map_err(|e| WebError::Internal(format!("Failed to search ledger: {}", e)))?;
    Ok(Json(entries))
}

fn search_ledger_entries(
    layout: &Layout,
    query: &str,
    days: Option<u64>,
    limit: Option<usize>,
    offset: usize,
) -> Result<Vec<LedgerEntryResponse>> {
    let db_path = layout.state_subdir().join("ledger.db");
    if !db_path.exists() {
        return Ok(Vec::new());
    }

    let conn = open_ledger_connection(db_path.as_std_path())?;
    let db = LedgerDb::new(&conn);
    let entries = db
        .search_ledger(query, None, days, false, limit, offset)
        .map_err(|e| miette!("Failed to search ledger entries: {}", e))?;
    Ok(entries.into_iter().map(LedgerEntryResponse::from).collect())
}

#[derive(Debug, Deserialize, Default)]
struct ChangesQuery {
    days: Option<u64>,
    working_tree: Option<bool>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ChangeResponse {
    id: String,
    path: String,
    status: String,
    summary: String,
    author: String,
    time_ago: String,
    file_count: usize,
    additions: usize,
    deletions: usize,
    risk: String,
}

async fn changes_handler(
    State(state): State<Arc<AppState>>,
    Query(params): Query<ChangesQuery>,
) -> Result<impl IntoResponse, WebError> {
    let layout = state.layout.clone();
    let days = params.days.unwrap_or(7);
    let include_working_tree = params.working_tree.unwrap_or(true);
    let changes =
        tokio::task::spawn_blocking(move || fetch_changes(&layout, days, include_working_tree))
            .await
            .map_err(|e| WebError::Internal(format!("Background task failed: {}", e)))?
            .map_err(|e| WebError::Internal(format!("Failed to fetch changes: {}", e)))?;
    Ok(Json(changes))
}

fn fetch_changes(
    layout: &Layout,
    days: u64,
    include_working_tree: bool,
) -> Result<Vec<ChangeResponse>> {
    let repo = match open_repo(layout.root.as_std_path()) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("No git repository available for /api/changes: {}", e);
            return Ok(Vec::new());
        }
    };

    let mut changes = Vec::new();

    if include_working_tree {
        let file_changes = crate::git::status::get_repo_status(&repo)
            .map_err(|e| miette!("Failed to get repository status: {}", e))?;
        if !file_changes.is_empty() {
            let file_count = file_changes.len();
            let (additions, deletions) = count_worktree_diff_stats(
                &repo,
                &file_changes
                    .iter()
                    .map(|c| c.path.to_string_lossy().to_string())
                    .collect::<Vec<_>>(),
            );
            let summary = file_changes
                .first()
                .map(|c| {
                    format!(
                        "{}: {}",
                        status_label(&c.change_type),
                        c.path.to_string_lossy()
                    )
                })
                .unwrap_or_else(|| "Uncommitted changes".to_string());
            changes.push(ChangeResponse {
                id: "working-tree".to_string(),
                path: if file_count == 1 {
                    file_changes[0].path.to_string_lossy().to_string()
                } else {
                    format!("{} files", file_count)
                },
                status: "Uncommitted".to_string(),
                summary,
                author: current_user(),
                time_ago: "now".to_string(),
                file_count,
                additions,
                deletions,
                risk: "MEDIUM".to_string(),
            });
        }
    }

    if days == 0 {
        return Ok(changes);
    }

    let max_commits = 50;
    let commit_changes = fetch_recent_commits(&repo, days, max_commits)
        .map_err(|e| miette!("Failed to walk recent commits: {}", e))?;
    changes.extend(commit_changes);

    Ok(changes)
}

fn status_label(change_type: &crate::git::ChangeType) -> &str {
    match change_type {
        crate::git::ChangeType::Added => "Added",
        crate::git::ChangeType::Modified => "Modified",
        crate::git::ChangeType::Deleted => "Deleted",
        crate::git::ChangeType::Renamed { .. } => "Renamed",
    }
}

fn current_user() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_else(|_| "unknown".to_string())
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct UserSession {
    id: String,
    name: String,
    email: String,
    role: String,
}

async fn session_handler() -> Result<impl IntoResponse, WebError> {
    let user = current_user();
    let session = UserSession {
        id: user.clone(),
        name: user,
        email: String::new(),
        role: "admin".to_string(),
    };
    Ok(Json(session))
}

fn count_worktree_diff_stats(repo: &gix::Repository, _paths: &[String]) -> (usize, usize) {
    // Single `git diff HEAD --numstat` call instead of one per file.
    let repo_root = repo.workdir().unwrap_or(repo.path());
    let mut additions = 0usize;
    let mut deletions = 0usize;
    let output = std::process::Command::new("git")
        .args(["--no-pager", "diff", "HEAD", "--numstat"])
        .current_dir(repo_root)
        .output();
    if let Ok(output) = output
        && output.status.success()
    {
        let text = String::from_utf8_lossy(&output.stdout);
        for line in text.lines() {
            let mut parts = line.split('\t');
            if let (Some(a), Some(d)) = (
                parts.next().and_then(|s| s.parse::<usize>().ok()),
                parts.next().and_then(|s| s.parse::<usize>().ok()),
            ) {
                additions += a;
                deletions += d;
            }
        }
    }
    (additions, deletions)
}

fn fetch_recent_commits(
    repo: &gix::Repository,
    days: u64,
    max_commits: usize,
) -> Result<Vec<ChangeResponse>> {
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
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let cutoff = now.saturating_sub(days * 86400);

    let mut changes = Vec::new();

    // Batch-fetch numstat for all commits in a single git subprocess instead of
    // spawning one `git diff --numstat` per commit (was 250 subprocesses).
    let head_oid = head.id().to_string();
    let commit_stats = batch_numstat(repo, &head_oid, max_commits, cutoff);

    for res in walk {
        if changes.len() >= max_commits {
            break;
        }
        let info = match res {
            Ok(info) => info,
            Err(e) => {
                tracing::warn!("Failed to retrieve commit info during changes walk: {e}");
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

        let diff = match repo.diff_tree_to_tree(Some(&parent_tree), Some(&current_tree), None) {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!("Failed to diff tree for {}: {e}", info.id());
                continue;
            }
        };

        let mut files = Vec::new();
        for change in diff {
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

        // Diff stats from the batched numstat map (no per-commit subprocess).
        let commit_id = info.id().to_string();
        let (additions, deletions) = commit_stats.get(&commit_id).copied().unwrap_or((0, 0));

        let file_count = files.len();
        let summary = commit
            .message_raw()
            .map(|m| String::from_utf8_lossy(m.as_ref()).to_string())
            .unwrap_or_default()
            .lines()
            .next()
            .unwrap_or("")
            .to_string();
        let author = commit
            .author()
            .map(|a| a.name.to_string())
            .unwrap_or_else(|_| "unknown".to_string());
        let risk = if additions.saturating_add(deletions) > 100 {
            "HIGH"
        } else if additions.saturating_add(deletions) > 20 {
            "MEDIUM"
        } else {
            "LOW"
        };

        changes.push(ChangeResponse {
            id: commit_id[..commit_id.len().min(8)].to_string(),
            path: if file_count == 1 {
                files.into_iter().next().unwrap_or_default()
            } else {
                format!("{} files", file_count)
            },
            status: "Committed".to_string(),
            summary: if summary.is_empty() {
                "(no message)".to_string()
            } else {
                summary
            },
            author,
            time_ago: format_time_ago(commit_time),
            file_count,
            additions,
            deletions,
            risk: risk.to_string(),
        });
    }

    Ok(changes)
}

/// Batch-fetch addition/deletion stats for up to `max_commits` commits in a
/// single `git log --numstat` subprocess, avoiding N individual `git diff`
/// calls. Returns a map of full commit hash → (additions, deletions).
fn batch_numstat(
    repo: &gix::Repository,
    head_oid: &str,
    max_commits: usize,
    cutoff: u64,
) -> std::collections::HashMap<String, (usize, usize)> {
    let repo_root = repo.workdir().unwrap_or(repo.path());
    let mut stats = std::collections::HashMap::new();

    let output = std::process::Command::new("git")
        .args([
            "--no-pager",
            "log",
            "--numstat",
            "--format=COMMIT:%H|%at",
            "--no-merges",
            &format!("-n{max_commits}"),
            head_oid,
        ])
        .current_dir(repo_root)
        .output();

    let Ok(output) = output else {
        tracing::warn!("batch_numstat: git log subprocess failed");
        return stats;
    };
    if !output.status.success() {
        tracing::warn!("batch_numstat: git log exited non-zero");
        return stats;
    }

    let text = String::from_utf8_lossy(&output.stdout);
    let mut current_hash: Option<String> = None;
    let mut current_time: Option<u64> = None;
    let mut current_adds = 0usize;
    let mut current_dels = 0usize;

    for line in text.lines() {
        if let Some(meta) = line.strip_prefix("COMMIT:") {
            // Save the previous commit's accumulated stats.
            if let Some(hash) = current_hash.take() {
                let time = current_time.unwrap_or(0);
                if time >= cutoff {
                    stats.insert(hash, (current_adds, current_dels));
                }
                current_adds = 0;
                current_dels = 0;
            }
            let mut parts = meta.split('|');
            current_hash = parts.next().map(|s| s.to_string());
            current_time = parts.next().and_then(|s| s.parse::<u64>().ok());
        } else if !line.is_empty() {
            // numstat line: "additions\tdeletions\tpath"
            let mut parts = line.split('\t');
            if let (Some(a), Some(d)) = (
                parts.next().and_then(|s| s.parse::<usize>().ok()),
                parts.next().and_then(|s| s.parse::<usize>().ok()),
            ) {
                current_adds += a;
                current_dels += d;
            }
        }
    }
    // Flush the last commit.
    if let Some(hash) = current_hash {
        let time = current_time.unwrap_or(0);
        if time >= cutoff {
            stats.insert(hash, (current_adds, current_dels));
        }
    }

    stats
}

fn format_time_ago(commit_time: u64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let seconds = now.saturating_sub(commit_time);
    if seconds < 60 {
        "just now".to_string()
    } else if seconds < 3600 {
        format!("{}m ago", seconds / 60)
    } else if seconds < 86400 {
        format!("{}h ago", seconds / 3600)
    } else if seconds < 30 * 86400 {
        format!("{}d ago", seconds / 86400)
    } else {
        format!("{}mo ago", seconds / (30 * 86400))
    }
}

#[derive(Debug, Deserialize, Default)]
struct HotspotsQueryParams {
    limit: Option<usize>,
    days: Option<u64>,
}

async fn hotspots_handler(
    State(state): State<Arc<AppState>>,
    Query(params): Query<HotspotsQueryParams>,
) -> Result<impl IntoResponse, WebError> {
    let layout = state.layout.clone();
    let cache = state.git_meta_cache.clone();
    let responses = tokio::task::spawn_blocking(move || {
        fetch_hotspots_response(&layout, params.limit, params.days, &cache)
    })
    .await
    .map_err(|e| WebError::Internal(format!("Background task failed: {}", e)))?
    .map_err(|e| WebError::Internal(format!("Failed to calculate hotspots: {}", e)))?;
    Ok(Json(responses))
}

fn fetch_hotspots(
    layout: &Layout,
    limit: Option<usize>,
    days: Option<u64>,
) -> Result<Vec<Hotspot>> {
    let repo = match open_repo(layout.root.as_std_path()) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("No git repository available for /api/hotspots: {}", e);
            return Ok(Vec::new());
        }
    };

    let storage = match StorageManager::open_read_only_sqlite_only(&layout.root) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("Storage not available for /api/hotspots: {}", e);
            return Ok(Vec::new());
        }
    };

    let config = match load_ledger_config(layout) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("Failed to load config for /api/hotspots: {}", e);
            Config::default()
        }
    };

    let history_provider = GixHistoryProvider::new(&repo);
    let query = HotspotQuery {
        limit: limit.unwrap_or(config.hotspots.limit),
        commits: config.hotspots.max_commits,
        days,
        decay_half_life: config.hotspots.decay_half_life,
        ..Default::default()
    };
    calculate_hotspots(&storage, &history_provider, &query)
        .map_err(|e| miette!("Failed to calculate hotspots: {}", e))
}

/// Fetch hotspots and map to `HotspotResponse` DTOs with git metadata
/// enrichment.
///
/// **TA30**: First attempts to read `last_touched_at` and `last_contributor`
/// directly from `project_files` (persisted during indexing). If those columns
/// are populated, no git walk is needed and the TTL cache is bypassed.
/// Falls back to the 5-minute TTL cache (TA29's interim solution) when the
/// columns are NULL or the table is unavailable.
fn fetch_hotspots_response(
    layout: &Layout,
    limit: Option<usize>,
    days: Option<u64>,
    cache: &tokio::sync::Mutex<crate::commands::web::git_meta::GitMetaCacheEntry>,
) -> Result<Vec<HotspotResponse>> {
    let hotspots = fetch_hotspots(layout, limit, days)?;

    if hotspots.is_empty() {
        return Ok(Vec::new());
    }

    // TA30: Try reading git metadata from project_files first.
    let file_paths: Vec<String> = hotspots
        .iter()
        .map(|h| h.path.to_string_lossy().replace('\\', "/"))
        .collect();

    let mut git_meta: HashMap<String, (String, String)> = HashMap::new();
    let mut need_cache_fallback = false;

    if let Ok(storage) = StorageManager::open_read_only_sqlite_only(&layout.root) {
        let conn = storage.get_connection();
        for chunk in file_paths.chunks(500) {
            let placeholders = std::iter::repeat_n("?", chunk.len())
                .collect::<Vec<_>>()
                .join(",");
            let query = format!(
                "SELECT file_path, last_touched_at, last_contributor FROM project_files \
                 WHERE REPLACE(file_path, '\\', '/') IN ({}) \
                 AND last_touched_at IS NOT NULL",
                placeholders
            );
            match conn.prepare(&query) {
                Ok(mut stmt) => {
                    let rows = stmt.query_map(rusqlite::params_from_iter(chunk), |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, Option<String>>(1)?,
                            row.get::<_, Option<String>>(2)?,
                        ))
                    });
                    if let Ok(rows) = rows {
                        for row in rows.flatten() {
                            let (path, ts, author) = row;
                            if let (Some(ts), Some(author)) = (ts, author) {
                                git_meta.insert(path, (ts, author));
                            }
                        }
                    }
                }
                Err(_) => {
                    // Column might not exist on old DBs — fall back to cache.
                    need_cache_fallback = true;
                }
            }
        }
    } else {
        need_cache_fallback = true;
    }

    // Check if we got metadata for all files from SQLite. If not, fall back
    // to the TTL cache (TA29 interim solution).
    let all_covered = file_paths.iter().all(|p| git_meta.contains_key(p));
    if !all_covered || need_cache_fallback {
        // Use the TTL cache for any files not covered by project_files.
        let mut cache_guard = cache.blocking_lock();
        let now = Instant::now();
        let needs_refresh = git_meta_cache_needs_refresh(&cache_guard, now);

        if needs_refresh {
            let new_map = build_git_metadata_map(&layout.root)?;
            *cache_guard = Some((now, new_map));
        }

        if let Some((_, ref cached_map)) = *cache_guard {
            for path in &file_paths {
                let normalized = path.replace('\\', "/");
                if !git_meta.contains_key(&normalized)
                    && let Some((ts, author)) = cached_map.get(&normalized)
                {
                    git_meta.insert(normalized.clone(), (ts.clone(), author.clone()));
                }
            }
        }
        drop(cache_guard);
    }

    Ok(map_hotspots_to_responses(&hotspots, &git_meta))
}

async fn server_header_middleware(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    let value = HeaderValue::from_static(concat!("ledgerful-web/", env!("CARGO_PKG_VERSION")));
    response.headers_mut().insert(header::SERVER, value);
    response
}

const RATE_LIMIT_WINDOW: Duration = Duration::from_secs(60);
const RATE_LIMIT_MAX: usize = 60;

/// Per-IP, per-path sliding-window rate limiter. Bursts up to the configured
/// limit are allowed; exceeding the window returns 429. Keying by path prevents a
/// single noisy endpoint from starving the rest of the dashboard.
async fn rate_limit_layer(
    State(state): State<Arc<AppState>>,
    request: Request,
    next: Next,
) -> Result<Response, WebError> {
    let ip = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ci| ci.ip())
        .unwrap_or_else(|| IpAddr::from([127, 0, 0, 1]));
    let path = request.uri().path().to_string();
    let now = Instant::now();
    let mut map = state.rate_limiter.lock().await;
    let entries = map.entry((ip, path)).or_default();
    entries.retain(|t| now.duration_since(*t) < RATE_LIMIT_WINDOW);
    if entries.len() >= RATE_LIMIT_MAX {
        return Err(WebError::TooManyRequests);
    }
    entries.push(now);
    drop(map);
    Ok(next.run(request).await)
}

#[derive(Serialize)]
struct ConfigResponse {
    project: String,
    repo_path: String,
    ledger_path: String,
    graph_path: String,
    signing_key: String,
    llm_backend: String,
    polling_interval: String,
    telemetry: String,
    version: String,
}

async fn config_handler(State(state): State<Arc<AppState>>) -> Result<impl IntoResponse, WebError> {
    let layout = state.layout.clone();
    let config =
        tokio::task::spawn_blocking(move || load_ledger_config(&layout).unwrap_or_default())
            .await
            .map_err(|e| WebError::Internal(format!("Background task failed: {}", e)))?;

    let llm_backend = if config.local_model.base_url.is_empty() {
        "none".to_string()
    } else {
        format!("local ({})", config.local_model.base_url)
    };

    Ok(Json(ConfigResponse {
        project: state.layout.get_project_id(),
        repo_path: state.layout.root.to_string(),
        ledger_path: state.layout.state_subdir().join("ledger.db").to_string(),
        graph_path: state.layout.state_subdir().join("ledger.cozo").to_string(),
        signing_key: "Ed25519 · configured".to_string(),
        llm_backend,
        polling_interval: "30s".to_string(),
        telemetry: "disabled".to_string(),
        version: format!("ledgerful {}", env!("CARGO_PKG_VERSION")),
    }))
}

/// Embedded SPA assets bundled at release time. In debug builds the folder is
/// not required because the dev loop uses `--spa-dir`.
///
/// Excluded files (via `#[exclude]`): unused marketing assets that would bloat
/// the binary with no functional benefit. Patterns use `**/` prefix to match
/// files regardless of subdirectory depth (robust against Next.js export layout
/// changes). When adding new public-only assets to `ledgerful-frontend/public/`,
/// either keep them out of the dashboard build or add them to the exclude list here.
#[cfg(not(debug_assertions))]
#[derive(rust_embed::RustEmbed)]
#[folder = "../ledgerful-frontend/out"]
#[exclude = "**/Banner.png"]
#[exclude = "**/Icon.png"]
struct SpaAssets;

/// Serve a static file from the embedded SPA bundle, falling back to
/// `index.html` so client-side routing works.
#[cfg(not(debug_assertions))]
async fn embedded_spa_handler(uri: axum::http::Uri) -> Result<Response, WebError> {
    let request_path = uri.path();
    serve_embedded_asset(request_path)
}

#[cfg(not(debug_assertions))]
fn serve_embedded_asset(request_path: &str) -> Result<Response, WebError> {
    let asset_path = request_path.trim_start_matches('/');
    let (file, path_for_mime) = SpaAssets::get(asset_path)
        .map(|f| (f, asset_path))
        .or_else(|| SpaAssets::get("index.html").map(|f| (f, "index.html")))
        .ok_or_else(|| WebError::Internal("index.html missing from embedded SPA".to_string()))?;

    let content_type = mime_guess::from_path(path_for_mime)
        .first_or_octet_stream()
        .to_string();

    let mut response = Response::new(axum::body::Body::from(file.data.into_owned()));
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(&content_type)
            .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream")),
    );
    // Embedded assets are immutable (baked at compile time). Aggressive caching
    // avoids redundant transfers on the localhost dashboard.
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=31536000, immutable"),
    );

    Ok(response)
}

/// Debug build fallback when no `--spa-dir` is provided. Release builds use
/// the embedded bundle above.
#[cfg(debug_assertions)]
async fn embedded_spa_handler(_uri: axum::http::Uri) -> Result<Response, WebError> {
    Ok((
        axum::http::StatusCode::NOT_FOUND,
        "No SPA directory configured; use --spa-dir in development or build a release binary.",
    )
        .into_response())
}

/// Open a SQLite connection to the ledger with concurrency-safe pragmas.
pub(crate) fn open_ledger_connection(path: &std::path::Path) -> Result<rusqlite::Connection> {
    let conn = rusqlite::Connection::open(path).into_diagnostic()?;
    conn.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA busy_timeout = 5000;",
    )
    .into_diagnostic()?;
    Ok(conn)
}

/// Bind a TCP listener and serve the router until SIGINT.
pub async fn serve(router: Router, bind: String, port: u16) -> Result<()> {
    let addr = SocketAddr::new(
        bind.parse()
            .map_err(|e| miette!("Invalid bind address {}: {}", bind, e))?,
        port,
    );

    let listener = TcpListener::bind(addr).await.into_diagnostic()?;
    tracing::info!("ledgerful web listening on {}", addr);

    axum::serve(listener, router)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .into_diagnostic()?;

    Ok(())
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use camino::Utf8Path;
    use std::process::Command;
    use tempfile::tempdir;

    fn init_git_repo_with_commit(root: &std::path::Path) {
        let run = |args: &[&str]| {
            assert!(
                Command::new("git")
                    .args(args)
                    .current_dir(root)
                    .status()
                    .unwrap()
                    .success(),
                "git {} failed",
                args.join(" ")
            );
        };
        run(&["init", "--quiet"]);
        run(&["config", "user.email", "test@example.com"]);
        run(&["config", "user.name", "Test User"]);
        std::fs::write(root.join("marker.txt"), "hello\n").unwrap();
        run(&["add", "."]);
        run(&["commit", "--quiet", "-m", "initial commit"]);
    }

    #[test]
    fn fetch_changes_days_filter() {
        let tmp = tempdir().unwrap();
        let root = Utf8Path::from_path(tmp.path()).unwrap();
        let layout = Layout::new(root);
        init_git_repo_with_commit(layout.root.as_std_path());

        let within_window = fetch_changes(&layout, 1, false).unwrap();
        assert_eq!(
            within_window.len(),
            1,
            "commit within 1 day should be returned"
        );

        let before_cutoff = fetch_changes(&layout, 0, false).unwrap();
        assert!(
            before_cutoff.is_empty(),
            "commit before 0-day cutoff should be excluded"
        );
    }

    #[test]
    fn fetch_changes_includes_commit_metadata() {
        let tmp = tempdir().unwrap();
        let root = Utf8Path::from_path(tmp.path()).unwrap();
        let layout = Layout::new(root);
        init_git_repo_with_commit(layout.root.as_std_path());

        let changes = fetch_changes(&layout, 1, false).unwrap();
        let first = changes.first().unwrap();
        assert_eq!(first.status, "Committed");
        assert_eq!(first.author, "Test User");
        assert!(!first.summary.is_empty());
    }

    #[test]
    fn change_response_uses_camel_case_keys() {
        let response = ChangeResponse {
            id: "abc123".to_string(),
            path: "src/main.rs".to_string(),
            status: "Committed".to_string(),
            summary: "init".to_string(),
            author: "dev".to_string(),
            time_ago: "just now".to_string(),
            file_count: 1,
            additions: 2,
            deletions: 3,
            risk: "LOW".to_string(),
        };
        let value = serde_json::to_value(response).unwrap();
        assert_eq!(value["timeAgo"].as_str(), Some("just now"));
        assert_eq!(value["fileCount"].as_u64(), Some(1));
        assert_eq!(value["additions"].as_u64(), Some(2));
        assert_eq!(value["deletions"].as_u64(), Some(3));
        assert!(value.get("time_ago").is_none());
    }

    #[cfg(not(debug_assertions))]
    #[test]
    fn embedded_bundle_excludes_unused_marketing_assets() {
        // Banner.png and Icon.png are confirmed-unused marketing assets.
        // They must NOT be embedded in the binary — at any path depth.
        // The `**/` glob prefix ensures subdirectory matches (e.g. public/Banner.png).
        assert!(SpaAssets::get("Banner.png").is_none());
        assert!(SpaAssets::get("Icon.png").is_none());
        // Also verify subdirectory paths are excluded (defense against
        // Next.js export layout changes).
        assert!(SpaAssets::get("public/Banner.png").is_none());
        assert!(SpaAssets::get("public/Icon.png").is_none());
    }

    #[cfg(not(debug_assertions))]
    #[test]
    fn embedded_bundle_includes_dashboard_assets() {
        // Core dashboard files must be present in the embedded bundle.
        assert!(
            SpaAssets::get("index.html").is_some(),
            "index.html must be present in the embedded SPA bundle"
        );
        // Verify at least one route-specific HTML file exists (not just index.html).
        // This proves dashboard routes were built and embedded, not just a stub.
        let has_dashboard_route = SpaAssets::iter().any(|f| {
            f.starts_with("dashboard")
                || f.starts_with("ledger")
                || f.starts_with("changes")
                || f.starts_with("hotspots")
                || f.starts_with("settings")
        });
        assert!(
            has_dashboard_route,
            "Expected dashboard route files in embedded bundle (e.g. dashboard.html, ledger.html)"
        );
    }
}
