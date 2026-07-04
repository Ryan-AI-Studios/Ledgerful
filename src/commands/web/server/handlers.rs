//! HTTP handlers for the Ledgerful web dashboard.

use crate::commands::helpers::load_ledger_config;
use crate::commands::web::error::WebError;
use crate::commands::web::git_meta::{
    GitMetaCacheEntry, build_git_metadata_map, git_meta_cache_needs_refresh,
};
use crate::commands::web::server::git::{current_user, fetch_changes};
use crate::commands::web::server::health::{compute_health_score, project_status_from_score};
use crate::commands::web::server::startup::open_ledger_connection;
use crate::commands::web::state::AppState;
use crate::commands::web::types::{
    ChangeResponse, ChangedFileResponse, ChangesQuery, ConfigResponse, HotspotResponse,
    HotspotsQueryParams, LedgerDetailResponse, LedgerEntryResponse, LedgerListQuery,
    LedgerSearchQuery, ProjectResponse, SnapshotResponse, StatusResponse, SyncStatusResponse,
    UserSession, map_hotspots_to_responses,
};
use crate::config::model::Config;
use crate::git::repo::open_repo;
use crate::impact::hotspots::{HotspotQuery, calculate_hotspots};
use crate::impact::packet::Hotspot;
use crate::impact::temporal::GixHistoryProvider;
use crate::ledger::db::LedgerDb;
use crate::ledger::error::LedgerError;

use crate::state::layout::Layout;
use crate::state::storage::StorageManager;
use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use miette::{Result, miette};
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::Mutex;

/// `GET /health` — daemon liveness check. Does **not** require auth.
#[utoipa::path(
    get,
    path = "/health",
    operation_id = "getHealth",
    tag = "health",
    responses(
        (status = 200, description = "Daemon is live", body = Object, content_type = "application/json")
    )
)]
pub(crate) async fn health_handler() -> impl IntoResponse {
    Json(json!({ "status": "ok" }))
}

/// `GET /api/session` — current user session.
#[utoipa::path(
    get,
    path = "/api/session",
    operation_id = "getSession",
    tag = "session",
    responses(
        (status = 200, description = "Current session", body = UserSession)
    )
)]
pub(crate) async fn session_handler() -> Result<impl IntoResponse, WebError> {
    let user = current_user();
    let session = UserSession {
        id: user.clone(),
        name: user,
        email: String::new(),
        role: "admin".to_string(),
    };
    Ok(Json(session))
}

/// `GET /api/snapshot` — summary metrics + recent change feed.
#[utoipa::path(
    get,
    path = "/api/snapshot",
    operation_id = "getSnapshot",
    tag = "snapshot",
    responses(
        (status = 200, description = "Project snapshot", body = SnapshotResponse)
    )
)]
pub(crate) async fn snapshot_handler(
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, WebError> {
    let layout = state.layout.clone();
    let git_meta_cache = state.git_meta_cache.clone();
    let snapshot = tokio::task::spawn_blocking(move || compute_snapshot(&layout, &git_meta_cache))
        .await
        .map_err(|e| WebError::Internal(format!("Background task failed: {}", e)))?
        .unwrap_or_else(|e| {
            tracing::warn!("Failed to compute snapshot: {}", e);
            empty_snapshot(&state.layout)
        });

    Ok(Json(snapshot))
}

pub(crate) fn empty_snapshot(layout: &Layout) -> SnapshotResponse {
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

pub(crate) fn compute_snapshot(
    layout: &Layout,
    git_meta_cache: &Mutex<GitMetaCacheEntry>,
) -> Result<SnapshotResponse> {
    let pending_transactions = count_pending_transactions(layout).unwrap_or(0);
    let unaudited_drift = count_unaudited_transactions(layout).unwrap_or(0);
    let indexed_documents = count_indexed_documents(layout);

    let recent_changes: Vec<ChangeResponse> = fetch_changes(layout, 7, false)
        .unwrap_or_default()
        .into_iter()
        .take(10)
        .collect();

    let top_hotspots: Vec<HotspotResponse> =
        match fetch_hotspots_response(layout, Some(10), None, git_meta_cache) {
            Ok(h) => h,
            Err(e) => {
                tracing::warn!("Failed to fetch hotspots for /api/snapshot: {}", e);
                Vec::new()
            }
        };

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

/// `GET /api/status` — overall daemon / project health.
///
/// **Intentional divergence:** this endpoint returns a bare `StatusResponse`
/// rather than the standard `WithSource<T>` wrapper used by dashboard data
/// surfaces. Status is a health surface; surfacing daemon failures directly is
/// more useful than substituting mock data. This is deferred item #30, now
/// documented in `coordination.md` §4.8.
#[utoipa::path(
    get,
    path = "/api/status",
    operation_id = "getStatus",
    tag = "status",
    responses(
        (status = 200, description = "Daemon status", body = StatusResponse)
    )
)]
pub(crate) async fn status_handler(
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, WebError> {
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

pub(crate) fn count_pending_transactions(layout: &Layout) -> Result<usize> {
    let (pending, _) = drift_status(layout)?;
    Ok(pending)
}

pub(crate) fn count_unaudited_transactions(layout: &Layout) -> Result<usize> {
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

/// `GET /api/projects` — project list (local + discovered siblings).
#[utoipa::path(
    get,
    path = "/api/projects",
    operation_id = "getProjects",
    tag = "projects",
    responses(
        (status = 200, description = "Project list", body = [ProjectResponse])
    )
)]
pub(crate) async fn projects_handler(
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

/// `GET /api/sync/status` — local M0 sync state.
///
/// **Feature gate:** the route is **always registered** (track 0013 DoD-1).
/// When built **with** `sync`, the handler reads `SyncState` from the ledger
/// DB and returns real sync metadata. When built **without** `sync`, it
/// returns a `501 Not Implemented` — the schema documents the route, and the
/// runtime honors it: no documented-but-unserved route, no dangling handler.
#[utoipa::path(
    get,
    path = "/api/sync/status",
    operation_id = "getSyncStatus",
    tag = "sync",
    responses(
        (status = 200, description = "Local sync state", body = SyncStatusResponse),
        (status = 501, description = "Sync feature not enabled in this build — rebuild with --features sync", body = crate::commands::web::error::ProblemDetail, content_type = "application/problem+json")
    )
)]
pub(crate) async fn sync_status_handler(
    State(state): State<Arc<AppState>>,
) -> Result<Response, WebError> {
    #[cfg(feature = "sync")]
    {
        sync_status_impl(state).await.map(|r| r.into_response())
    }
    #[cfg(not(feature = "sync"))]
    {
        let _ = state;
        Err(WebError::NotImplemented("The sync feature is not enabled in this build. Rebuild with --features sync to use /api/sync/status.".to_string()))
    }
}

/// Sync-enabled implementation: reads `SyncState` from the ledger DB.
#[cfg(feature = "sync")]
async fn sync_status_impl(state: Arc<AppState>) -> Result<Json<SyncStatusResponse>, WebError> {
    let layout = state.layout.clone();
    let status = tokio::task::spawn_blocking(move || {
        let db_path = layout.state_subdir().join("ledger.db");
        if !db_path.exists() {
            return empty_sync_status();
        }

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

/// `GET /api/ledger` — transaction table, paginated.
#[utoipa::path(
    get,
    path = "/api/ledger",
    operation_id = "listLedger",
    tag = "ledger",
    params(LedgerListQuery),
    responses(
        (status = 200, description = "Ledger entries", body = [LedgerEntryResponse])
    )
)]
pub(crate) async fn ledger_handler(
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

/// `GET /api/ledger/{tx_id}` — single transaction detail.
#[utoipa::path(
    get,
    path = "/api/ledger/{tx_id}",
    operation_id = "getLedgerDetail",
    tag = "ledger",
    params(
        ("tx_id" = String, Path, description = "Transaction ID")
    ),
    responses(
        (status = 200, description = "Ledger entry detail", body = LedgerDetailResponse),
        (status = 404, description = "Transaction not found")
    )
)]
pub(crate) async fn ledger_tx_handler(
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
/// `hotspot_history` snapshot.
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

/// `GET /api/ledger/search` — full-text search over ledger entries.
#[utoipa::path(
    get,
    path = "/api/ledger/search",
    operation_id = "searchLedger",
    tag = "ledger",
    params(LedgerSearchQuery),
    responses(
        (status = 200, description = "Matching ledger entries", body = [LedgerEntryResponse]),
        (status = 400, description = "Missing search query")
    )
)]
pub(crate) async fn ledger_search_handler(
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

/// `GET /api/changes` — recent changes (working tree + commits).
#[utoipa::path(
    get,
    path = "/api/changes",
    operation_id = "listChanges",
    tag = "changes",
    params(ChangesQuery),
    responses(
        (status = 200, description = "Recent changes", body = [ChangeResponse])
    )
)]
pub(crate) async fn changes_handler(
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

/// `GET /api/hotspots` — hotspot rankings.
#[utoipa::path(
    get,
    path = "/api/hotspots",
    operation_id = "listHotspots",
    tag = "hotspots",
    params(HotspotsQueryParams),
    responses(
        (status = 200, description = "Hotspot rankings", body = [HotspotResponse])
    )
)]
pub(crate) async fn hotspots_handler(
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
pub(crate) fn fetch_hotspots_response(
    layout: &Layout,
    limit: Option<usize>,
    days: Option<u64>,
    cache: &Mutex<GitMetaCacheEntry>,
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

/// `GET /api/config` — daemon configuration (secrets redacted).
#[utoipa::path(
    get,
    path = "/api/config",
    operation_id = "getConfig",
    tag = "config",
    responses(
        (status = 200, description = "Daemon configuration", body = ConfigResponse)
    )
)]
pub(crate) async fn config_handler(
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, WebError> {
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
#[cfg(not(debug_assertions))]
#[derive(rust_embed::RustEmbed)]
#[folder = "../ledgerful-frontend/out"]
#[exclude = "**/Banner.png"]
#[exclude = "**/Icon.png"]
struct SpaAssets;

/// Serve a static file from the embedded SPA bundle, falling back to
/// `index.html` so client-side routing works.
#[cfg(not(debug_assertions))]
pub(crate) async fn embedded_spa_handler(uri: axum::http::Uri) -> Result<Response, WebError> {
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
pub(crate) async fn embedded_spa_handler(_uri: axum::http::Uri) -> Result<Response, WebError> {
    Ok((
        StatusCode::NOT_FOUND,
        "No SPA directory configured; use --spa-dir in development or build a release binary.",
    )
        .into_response())
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
        assert!(SpaAssets::get("Banner.png").is_none());
        assert!(SpaAssets::get("Icon.png").is_none());
        assert!(SpaAssets::get("public/Banner.png").is_none());
        assert!(SpaAssets::get("public/Icon.png").is_none());
    }

    #[cfg(not(debug_assertions))]
    #[test]
    fn embedded_bundle_includes_dashboard_assets() {
        assert!(
            SpaAssets::get("index.html").is_some(),
            "index.html must be present in the embedded SPA bundle"
        );
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

    #[test]
    fn snapshot_returns_typed_dto_collections() {
        let tmp = tempdir().unwrap();
        let root = Utf8Path::from_path(tmp.path()).unwrap();
        let layout = Layout::new(root);
        init_git_repo_with_commit(layout.root.as_std_path());

        let cache = Mutex::new(None);
        let snapshot = compute_snapshot(&layout, &cache).unwrap();

        assert!(
            !snapshot.recent_changes.is_empty(),
            "recent_changes should have at least one entry"
        );
        for change in &snapshot.recent_changes {
            assert!(!change.id.is_empty(), "ChangeResponse.id must be populated");
            assert!(!change.path.is_empty());
            assert!(!change.status.is_empty());
        }

        for hotspot in &snapshot.top_hotspots {
            assert!(
                !hotspot.id.is_empty(),
                "HotspotResponse.id must be populated"
            );
            assert!(!hotspot.file_path.is_empty());
            assert!(hotspot.rank >= 1, "HotspotResponse.rank must be 1-based");
        }
    }

    #[test]
    fn snapshot_serializes_typed_arrays() {
        let tmp = tempdir().unwrap();
        let root = Utf8Path::from_path(tmp.path()).unwrap();
        let layout = Layout::new(root);
        init_git_repo_with_commit(layout.root.as_std_path());

        let cache = Mutex::new(None);
        let snapshot = compute_snapshot(&layout, &cache).unwrap();
        let value = serde_json::to_value(&snapshot).unwrap();

        let recent = value["recent_changes"].as_array().unwrap();
        if !recent.is_empty() {
            let first = &recent[0];
            assert!(
                first.get("timeAgo").is_some(),
                "camelCase timeAgo must be present"
            );
            assert!(
                first.get("fileCount").is_some(),
                "camelCase fileCount must be present"
            );
            assert!(first.get("id").is_some(), "id must be present");
            assert!(first.get("time_ago").is_none(), "snake_case must not leak");
        }

        let hotspots = value["top_hotspots"].as_array().unwrap();
        for h in hotspots {
            assert!(
                h.get("filePath").is_some(),
                "camelCase filePath must be present"
            );
            assert!(
                h.get("riskLevel").is_some(),
                "camelCase riskLevel must be present"
            );
            assert!(h.get("rank").is_some(), "rank must be present");
        }
    }

    #[test]
    fn empty_snapshot_has_typed_empty_arrays() {
        let tmp = tempdir().unwrap();
        let root = Utf8Path::from_path(tmp.path()).unwrap();
        let layout = Layout::new(root);

        let snapshot = empty_snapshot(&layout);
        assert!(snapshot.recent_changes.is_empty());
        assert!(snapshot.top_hotspots.is_empty());

        let value = serde_json::to_value(&snapshot).unwrap();
        assert!(value["recent_changes"].is_array());
        assert!(value["top_hotspots"].is_array());
    }

    #[test]
    fn sync_status_response_serializes_snake_case() {
        let response = SyncStatusResponse {
            device_id: Some("device-123".to_string()),
            last_extract_at: Some("2026-06-30T12:00:00Z".to_string()),
            last_apply_at: None,
            last_run_at: None,
        };
        let value = serde_json::to_value(response).unwrap();
        assert!(
            value.get("device_id").is_some(),
            "snake_case device_id must be present"
        );
        assert!(value.get("last_extract_at").is_some());
        assert!(value.get("last_apply_at").is_some());
        assert!(value.get("last_run_at").is_some());
        assert!(value.get("deviceId").is_none(), "camelCase must not leak");
    }

    #[test]
    fn web_error_not_implemented_returns_501() {
        use axum::http::StatusCode;
        use axum::response::IntoResponse;

        let err = WebError::NotImplemented("test".to_string());
        let response = err.into_response();
        assert_eq!(response.status(), StatusCode::NOT_IMPLEMENTED);
    }
}
