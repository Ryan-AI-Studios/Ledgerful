//! Additional read-only JSON API handlers for the Ledgerful web dashboard.
//!
//! These endpoints back the remaining SPA screens that were not covered by the
//! core handler set in `server.rs`: report downloads, hotspot trends, contract
//! impact, security boundaries, and the CozoDB knowledge-graph subgraph.

use crate::commands::helpers::load_ledger_config;
use crate::commands::web::error::WebError;
use crate::commands::web::state::AppState;
use crate::commands::web::types::*;
use crate::config::load::load_config;
use crate::contracts::AffectedContract;
use crate::git::repo::open_repo;
use crate::git::status::get_repo_status;
use crate::impact::hotspots::query_file_complexities;
use crate::ledger::db::LedgerDb;
use crate::ledger::types::LedgerEntry;
use crate::state::layout::Layout;
use crate::state::storage::StorageManager;
use axum::Json;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use chrono::{NaiveDate, Utc};
use miette::{IntoDiagnostic, Result, miette};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Instant, UNIX_EPOCH};

use super::KG_CACHE_TTL;
use super::cozo::{KG_MAX_LIMIT, fetch_knowledge_graph, fetch_security_boundaries};

// ---------------------------------------------------------------------------
// HotspotResponse DTO (Track TA29)
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Project trend endpoint
// ---------------------------------------------------------------------------

/// `GET /api/trends` — project-level trend series from the cached daily rollup.
#[utoipa::path(
    get,
    path = "/api/trends",
    operation_id = "getTrends",
    tag = "trends",
    params(TrendsQuery),
    responses(
        (status = 200, description = "Project-level trend data", body = TrendsResponse)
    )
)]
pub async fn trends_handler(
    State(state): State<Arc<AppState>>,
    Query(params): Query<TrendsQuery>,
) -> Result<impl IntoResponse, WebError> {
    let layout = state.layout.clone();
    let days = params.days.unwrap_or(90).clamp(1, 365);
    let response = tokio::task::spawn_blocking(move || fetch_trends(&layout, days))
        .await
        .map_err(|e| WebError::Internal(format!("Background task failed: {e}")))?
        .map_err(|e| WebError::Internal(format!("Failed to fetch trends: {e}")))?;
    Ok(Json(response))
}

fn fetch_trends(layout: &Layout, days: u64) -> Result<TrendsResponse> {
    let storage = match StorageManager::open_read_only_sqlite_only(&layout.root) {
        Ok(s) => s,
        Err(e) => {
            let db_path = layout.state_subdir().join("ledger.db");
            if !db_path.as_std_path().exists() {
                return Ok(TrendsResponse { data: Vec::new() });
            }
            return Err(e);
        }
    };
    let conn = storage.get_connection();
    let cutoff = (Utc::now() - chrono::Duration::days((days - 1) as i64))
        .format("%Y-%m-%d")
        .to_string();
    let points: Vec<TrendPointDto> = conn
        .prepare(
            "SELECT day, score, changes, high_risk_count FROM project_trend_days WHERE day >= ?1 ORDER BY day ASC",
        )
        .into_diagnostic()?
        .query_map([&cutoff], |row| {
            Ok(TrendPointDto {
                date: row.get(0)?,
                score: row.get(1)?,
                changes: row.get(2)?,
                high_risk_count: row.get(3)?,
            })
        })
        .into_diagnostic()?
        .filter_map(|r| match r {
            Ok(dto) => Some(dto),
            Err(e) => {
                tracing::warn!("fetch_trends: skipping malformed row: {}", e);
                None
            }
        })
        .collect();
    Ok(TrendsResponse { data: points })
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

pub(crate) fn collect_recent_commits(
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

// ---------------------------------------------------------------------------
// Knowledge-graph subgraph endpoint
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Compliance dashboard endpoints (Track E2)
// ---------------------------------------------------------------------------

/// Maximum number of signature rows returned by `/api/compliance/signatures`.
/// Bound to keep the payload bounded for the dashboard table; the ledger is
/// ordered `committed_at DESC` so this is the most recent 100 entries.
const COMPLIANCE_SIGNATURES_LIMIT: usize = 100;

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

    // Load config to determine whether signing is required and pin list
    // (mirrors `verify_ledger_signatures` in `src/commands/verify.rs`).
    let intent = load_config(layout).unwrap_or_default().intent;
    let require_signing = intent.require_signing;
    let trusted_keys = intent.trusted_public_keys.as_slice();
    let min_sig_version = intent.min_sig_version;

    // `get_all_committed_ledger_entries` returns entries ordered
    // `committed_at ASC, tx_id ASC` (see `src/ledger/db/transactions.rs:349`).
    // Sort defensively into DESC order so `last_audit_at` (the most recent VALID
    // entry's `committed_at`) and downstream consumers are deterministic.
    let mut entries = {
        let db = LedgerDb::new(conn);
        db.get_all_committed_ledger_entries()
            .map_err(|e| miette!("Failed to read ledger entries: {e}"))?
    };
    entries.sort_by(|a, b| {
        b.committed_at
            .cmp(&a.committed_at)
            .then_with(|| b.tx_id.cmp(&a.tx_id))
    });

    let mut total_signed: u64 = 0;
    let mut valid_count: u64 = 0;
    let mut last_audit_at: Option<String> = None;

    for entry in &entries {
        let status = classify_signature(entry, require_signing, trusted_keys, min_sig_version);
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
    let intent = load_config(layout).unwrap_or_default().intent;
    let require_signing = intent.require_signing;
    let trusted_keys = intent.trusted_public_keys.as_slice();
    let min_sig_version = intent.min_sig_version;

    // `get_all_committed_ledger_entries` returns `committed_at ASC, tx_id ASC`
    // (`src/ledger/db/transactions.rs:349`). Sort into DESC order so the
    // most recent entries come first, then bound to the most recent
    // `COMPLIANCE_SIGNATURES_LIMIT` (100). `.take(100)` preserves this DESC
    // order, so no re-sort is needed after mapping into the response shape.
    let mut entries = {
        let db = LedgerDb::new(conn);
        db.get_all_committed_ledger_entries()
            .map_err(|e| miette!("Failed to read ledger entries: {e}"))?
    };
    entries.sort_by(|a, b| {
        b.committed_at
            .cmp(&a.committed_at)
            .then_with(|| b.tx_id.cmp(&a.tx_id))
    });

    let out: Vec<ComplianceSignatureEntry> = entries
        .into_iter()
        .take(COMPLIANCE_SIGNATURES_LIMIT)
        .map(|entry| {
            let status = classify_signature(&entry, require_signing, trusted_keys, min_sig_version);
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
/// `verify_ledger_signatures` in `src/commands/verify.rs`.
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

fn classify_signature(
    entry: &LedgerEntry,
    require_signing: bool,
    trusted_keys: &[String],
    min_sig_version: u32,
) -> SignatureStatus {
    use crate::ledger::crypto::{SignatureTrustStatus, classify_entry_signature};
    // Dashboard still surfaces VALID|INVALID|SKIPPED; map trusted/unknown → VALID.
    match classify_entry_signature(entry, trusted_keys, min_sig_version) {
        SignatureTrustStatus::ValidTrusted | SignatureTrustStatus::ValidUnknownKey => {
            SignatureStatus::Valid
        }
        SignatureTrustStatus::Invalid => SignatureStatus::Invalid,
        SignatureTrustStatus::Unsigned => {
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
    let is_demo = layout.root.join(".ledgerful").join("DEMO_MARKER").exists();
    let keys_dir = if is_demo {
        Some(
            layout
                .root
                .join(".ledgerful")
                .join("keys")
                .into_std_path_buf(),
        )
    } else {
        None
    };
    let filename = if is_demo {
        "attachment; filename=\"ledgerful-DEMO-evidence.zip\""
    } else {
        "attachment; filename=\"ledgerful-soc2-evidence.zip\""
    };
    let zip_bytes = tokio::task::spawn_blocking(move || {
        crate::export::soc2::generate_soc2_export_with_options(
            &layout,
            is_demo,
            keys_dir.as_deref(),
            None,
        )
    })
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
        axum::http::HeaderValue::from_static(filename),
    );
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;

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
