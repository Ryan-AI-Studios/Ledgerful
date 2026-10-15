use crate::commands::helpers::{get_layout, load_ledger_config};
use crate::commands::hook_sidecar::{RECOVER_HINT, hash_message, mark_promote_failed};
// Re-export so existing `hook_post_commit::PendingHookTx` / `read_pending_sidecar`
// call sites keep working.
pub use crate::commands::hook_sidecar::{PendingHookTx, read_pending_sidecar};
use crate::git::numstat::per_file_numstat;
use crate::git::repo::{get_head_info, open_repo};
use crate::impact::budget::{AnalysisBudget, HistoryWalkStop};
use crate::impact::hotspots::normalize_score;
use crate::impact::hotspots::{HotspotQuery, calculate_hotspots_detailed};
use crate::impact::packet::Hotspot;
use crate::impact::temporal::GixHistoryProvider;
use crate::ledger::{ChangeType, CommitRequest, TransactionManager, VerificationStatus};
use crate::state::storage::StorageManager;
use crate::state::storage::hotspot_trends::{
    TrendInsert, insert_hotspot_trends_with_retry, report_is_database_busy,
};
use chrono::{Duration as ChronoDuration, Utc};
use miette::{IntoDiagnostic, Result, miette};
use std::collections::HashMap;
use std::fs;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::{Duration, Instant};

/// Entry point for the `internal hook-post-commit` command. This is invoked by
/// the git `post-commit` hook and must never block or fail the commit: any
/// error is logged at debug level and the function returns `Ok(())`.
pub fn execute_hook_post_commit() -> Result<()> {
    let layout = match get_layout() {
        Ok(l) => l,
        Err(e) => {
            tracing::debug!("Post-commit hook: no layout: {}", e);
            return Ok(());
        }
    };

    execute_hook_post_commit_for_layout(&layout)
}

pub fn execute_hook_post_commit_for_layout(layout: &crate::state::layout::Layout) -> Result<()> {
    // First, run the existing ledger-promotion sidecar logic synchronously so
    // pending transactions are committed immediately. Git must never be blocked
    // (always return Ok). Under enforce, promote failures retain PENDING+sidecar
    // with promote_failed and emit CRITICAL (not debug-only).
    if let Err(e) = promote_pending_ledger_tx(layout) {
        let msg = format!(
            "[Ledgerful] CRITICAL: post-commit promote failed (trail retained): {}. Recover with: {}",
            e, RECOVER_HINT
        );
        // Single emission via cli_summary (0093 DoD-9); error! → stderr via level-split.
        tracing::error!(target: "cli_summary", "{msg}");
    }

    // One 10-second deadline for the insert and for the index that follows it.
    // The insert outcome is sent first. Catch-up and the incremental index run
    // only when at least one second remains, and the main thread waits for
    // that completion only until this same deadline. It does not join the
    // worker. On Windows, process exit kills a thread that is still running
    // when the deadline fires. Git is never failed.
    let deadline = Instant::now() + POST_COMMIT_BUDGET;
    let db_path = layout
        .state_subdir()
        .join("ledger.db")
        .as_std_path()
        .to_path_buf();
    let repo_root = layout.root.clone();
    let layout_for_worker = layout.clone();
    let (outcome_tx, outcome_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();
    let spawned = thread::Builder::new()
        .name("post-commit-worker".to_string())
        .spawn(move || {
            let budget = post_commit_walk_budget(&layout_for_worker, deadline);
            let outcome = record_hotspot_trends_with_budget(&repo_root, &db_path, &budget);
            let _ = outcome_tx.send(outcome);

            if deadline.saturating_duration_since(Instant::now()) >= Duration::from_secs(1) {
                if let Ok(storage) = StorageManager::init_with_layout(&layout_for_worker)
                    && let Err(e) = catch_up_project_trends(&storage, 90)
                {
                    tracing::debug!("Post-commit hook: catch-up failed: {}", e);
                }
                if let Ok(storage) = StorageManager::init_with_layout(&layout_for_worker) {
                    let config = load_ledger_config(&layout_for_worker).unwrap_or_default();
                    let mut indexer =
                        crate::index::ProjectIndexer::new(storage, repo_root.clone(), config);
                    if let Err(e) = indexer.incremental_index() {
                        tracing::debug!("Post-commit hook: incremental index failed: {}", e);
                    }
                    if let Err(e) = indexer.shutdown_storage() {
                        tracing::debug!("Post-commit hook: storage shutdown failed: {}", e);
                    }
                }
            }
            let _ = done_tx.send(());
        });
    if spawned.is_err() {
        tracing::debug!("Post-commit hook: failed to spawn worker");
        return Ok(());
    }

    let outcome_wait = deadline.saturating_duration_since(Instant::now());
    match outcome_rx.recv_timeout(outcome_wait) {
        Ok(outcome) => {
            if let Some(line) = warning_line(outcome) {
                tracing::warn!(target: "cli_summary", "{line}");
            }
            let done_wait = deadline.saturating_duration_since(Instant::now());
            if done_rx.recv_timeout(done_wait).is_err() {
                tracing::debug!("Post-commit hook: index did not finish before the 10s deadline");
            }
        }
        Err(_) => {
            tracing::warn!(target: "cli_summary", "{DEADLINE_WARNING}");
        }
    }

    Ok(())
}

/// Promote a pending ledger sidecar transaction if one exists and matches the
/// current HEAD commit message hash.
fn promote_pending_ledger_tx(layout: &crate::state::layout::Layout) -> Result<()> {
    let config = load_ledger_config(layout)?;
    let sidecar_path = layout.state_subdir().join("pending_hook_tx");

    if !sidecar_path.exists() {
        return Ok(());
    }

    let mut pending = match read_pending_sidecar(sidecar_path.as_std_path())? {
        Some(p) => p,
        None => return Ok(()),
    };

    // Verify commit hash (message-hash heuristic; not git oid).
    let repo_root = layout.root.clone();
    let output = crate::git::git_command_with_policy(&config.verify.effective_process_policy())?
        .args(["log", "-1", "--format=%B"])
        .current_dir(repo_root.as_std_path())
        .output()
        .into_diagnostic()?;

    let current_commit_msg = String::from_utf8_lossy(&output.stdout).to_string();
    let cleaned_msg = crate::util::text::clean_commit_msg(&current_commit_msg);
    let current_hash = hash_message(&cleaned_msg);

    if pending.commit_msg_hash != current_hash {
        // Never discard a promote-failed orphan on hash mismatch — recovery owns it.
        if pending.is_promote_failed() {
            tracing::warn!(
                target: "cli_summary",
                "[Ledgerful] Promote-failed orphan {} does not match HEAD message-hash; retaining for recovery. {}",
                pending.tx_id,
                RECOVER_HINT
            );
            return Ok(());
        }
        tracing::info!(
            target: "cli_summary",
            "[Ledgerful] Pending transaction {} was for a different commit; discarding sidecar.",
            pending.tx_id
        );
        let _ = fs::remove_file(&sidecar_path);
        return Ok(());
    }

    // RT-H3: promote never sets Verified/ManualInspection. Only bound
    // `ledgerful verify --tx-id` may set Verified. TRIVIAL stays None/None.
    // Durable [SKIPPED] rows always promote as Unverified (phase0 ceiling).
    let is_skipped_coverage = pending.summary.starts_with("[SKIPPED]");
    let verification_status = if is_skipped_coverage {
        Some(VerificationStatus::Unverified)
    } else if pending.risk.as_deref() == Some("TRIVIAL") {
        None
    } else {
        Some(VerificationStatus::Unverified)
    };

    let mut storage = StorageManager::init_with_layout(layout)?;
    let mut tx_mgr =
        TransactionManager::new(&mut storage, layout.root.clone().into(), config.clone());

    let req = CommitRequest {
        summary: pending.summary.clone(),
        reason: pending.reason.clone(),
        change_type: ChangeType::Modify,
        is_breaking: false,
        committed_at: pending.committed_at.clone(),
        verification_status,
        verification_basis: None,
        outcome_notes: None,
        issue_ref: None,
        signature: pending.signature.clone(),
        public_key: pending.public_key.clone(),
        risk: pending.risk.clone(),
        related_tickets: pending.related_tickets.clone(),
        snapshot_id: pending.snapshot_id,
        observed: pending.observed,
        ..Default::default()
    };

    let snapshot_id_from_sidecar = pending.snapshot_id;

    let committed_tx_id = pending.tx_id.clone();
    let repo_root_for_stats = repo_root.clone();
    match tx_mgr.commit_change(committed_tx_id.clone(), req, false) {
        Ok(_) => {
            // Best-effort: attach per-file diff stats to the changed_files rows
            // for the transaction's snapshot.  The git commit object now exists,
            // so we can compute the committed-diff basis.
            if let Err(e) = update_changed_files_diff_stats(
                &mut tx_mgr,
                &committed_tx_id,
                &repo_root_for_stats,
                snapshot_id_from_sidecar,
            ) {
                tracing::debug!(
                    "Post-commit hook: failed to update diff stats for {}: {}",
                    committed_tx_id,
                    e
                );
            }
            let _ = fs::remove_file(&sidecar_path);
            Ok(())
        }
        Err(e) => {
            // RT-H1: never destroy the trail. Keep PENDING + sidecar, mark
            // promote_failed. Do not rollback. Git still exits 0 (outer Ok).
            let err_str = e.to_string();
            if let Err(mark_err) =
                mark_promote_failed(sidecar_path.as_std_path(), &mut pending, &err_str)
            {
                tracing::error!(
                    target: "cli_summary",
                    "[Ledgerful] CRITICAL: failed to mark promote_failed on sidecar: {}",
                    mark_err
                );
            }

            if config.gate.is_enforce() {
                // Single CRITICAL surface is the outer handler
                // (`execute_hook_post_commit_for_layout`) — do not double-print here.
                return Err(miette!(
                    "post-commit promote failed for {} (PENDING+sidecar retained): {}",
                    committed_tx_id,
                    err_str
                ));
            }

            tracing::warn!(
                target: "cli_summary",
                "[Ledgerful] Post-commit hook failed to promote ledger entry (observe mode, trail retained): {}",
                err_str
            );
            // Observe: keep trail without hard-failing the promote path; still Ok
            // so the outer hook stays quiet beyond the warn.
            Ok(())
        }
    }
}

/// Update per-file addition/deletion stats for the committed transaction's
/// snapshot using the committed-diff basis.
///
/// Best-effort: any failure is returned so the caller can log it and continue;
/// the git commit must never be blocked by stats computation.
fn update_changed_files_diff_stats(
    tx_mgr: &mut TransactionManager<'_>,
    tx_id: &str,
    repo_root: &camino::Utf8Path,
    snapshot_id_override: Option<i64>,
) -> Result<()> {
    let snapshot_id = if let Some(sid) = snapshot_id_override {
        sid
    } else {
        let tx = tx_mgr
            .get_transaction(tx_id)
            .map_err(|e| miette!("failed to read transaction: {}", e))?
            .ok_or_else(|| miette!("transaction not found after commit"))?;
        tx.snapshot_id
            .ok_or_else(|| miette!("committed transaction has no snapshot"))?
    };

    let (head_hash, _branch) = match open_repo(repo_root.as_std_path())
        .map_err(|e| miette!("cannot open repo: {}", e))?
        .head()
        .map_err(|e| miette!("cannot read HEAD: {}", e))?
        .id()
    {
        Some(id) => (id.to_string(), None::<&str>),
        None => return Ok(()),
    };

    let stats = per_file_numstat(repo_root.as_std_path(), &head_hash)
        .map_err(|e| miette!("numstat failed: {}", e))?;

    tx_mgr
        .storage_mut()
        .update_changed_files_stats(snapshot_id, &stats)
        .map_err(|e| miette!("failed to persist diff stats: {}", e))?;

    tracing::debug!(
        "Post-commit hook: updated diff stats for {} changed files in snapshot {}",
        stats.len(),
        snapshot_id
    );
    Ok(())
}

/// Shared post-commit wall clock. The outcome wait and the completion wait
/// both end at `Instant::now() + POST_COMMIT_BUDGET` captured once.
const POST_COMMIT_BUDGET: Duration = Duration::from_secs(10);

const HISTORY_BUDGET_WARNING: &str =
    "[Ledgerful] WARNING: post-commit hotspot trend sample was not recorded (history budget).";
const DATABASE_LOCKED_WARNING: &str =
    "[Ledgerful] WARNING: post-commit hotspot trend sample was not recorded (database locked).";
const DEADLINE_WARNING: &str =
    "[Ledgerful] WARNING: post-commit hotspot trend sample did not finish within 10s.";

/// What the post-commit worker recorded. Every exit sends one of these.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RecordOutcome {
    Inserted,
    Deduped,
    Empty,
    NotRecordedHistoryBudget,
    NotRecordedDatabaseLocked,
    Silent,
}

/// `Some` when the walk must not be stored. A non-complete walk is not a
/// sample, including when the file list is non-empty: complexity was skipped
/// and every non-docs score is `0.0`.
pub(crate) fn outcome_for_walk(
    stop: HistoryWalkStop,
    hotspots_empty: bool,
) -> Option<RecordOutcome> {
    match stop {
        HistoryWalkStop::Complete if hotspots_empty => Some(RecordOutcome::Empty),
        HistoryWalkStop::Complete => None,
        HistoryWalkStop::Budget | HistoryWalkStop::Cancelled => {
            Some(RecordOutcome::NotRecordedHistoryBudget)
        }
    }
}

pub(crate) fn warning_line(outcome: RecordOutcome) -> Option<&'static str> {
    match outcome {
        RecordOutcome::Inserted
        | RecordOutcome::Deduped
        | RecordOutcome::Empty
        | RecordOutcome::Silent => None,
        RecordOutcome::NotRecordedHistoryBudget => Some(HISTORY_BUDGET_WARNING),
        RecordOutcome::NotRecordedDatabaseLocked => Some(DATABASE_LOCKED_WARNING),
    }
}

fn post_commit_walk_budget(
    layout: &crate::state::layout::Layout,
    deadline: Instant,
) -> AnalysisBudget {
    let cancel = Arc::new(AtomicBool::new(false));
    let history_secs = load_ledger_config(layout)
        .map(|config| config.hotspots.history_budget_secs)
        .unwrap_or(0);
    match deadline.checked_sub(Duration::from_secs(1)) {
        Some(insert_reserve) => {
            AnalysisBudget::capped_by_overall(history_secs, Some(insert_reserve), cancel)
        }
        None => AnalysisBudget::expired(cancel),
    }
}

/// Append a hotspot snapshot for the current HEAD to `hotspot_trends`.
///
/// The caller supplies the walk budget. This function does not index. Every
/// path returns a [`RecordOutcome`], including storage-open failure.
pub(crate) fn record_hotspot_trends_with_budget(
    repo_root: &camino::Utf8Path,
    db_path: &std::path::Path,
    budget: &AnalysisBudget,
) -> RecordOutcome {
    let repo = match open_repo(repo_root.as_std_path()) {
        Ok(r) => r,
        Err(e) => {
            tracing::debug!("Post-commit hook: cannot open repo: {}", e);
            return RecordOutcome::Silent;
        }
    };

    let (head_hash, _branch) = match get_head_info(&repo) {
        Ok((Some(h), b)) => (h, b),
        Ok((None, _)) => {
            tracing::debug!("Post-commit hook: no HEAD hash");
            return RecordOutcome::Silent;
        }
        Err(e) => {
            tracing::debug!("Post-commit hook: cannot read HEAD: {}", e);
            return RecordOutcome::Silent;
        }
    };

    // Reconstruct layout from known work root + state home of the shared db_path
    // (never Layout::new(repo_root) — that invents private linked-worktree state).
    // db_path is …/.ledgerful/state/ledger.db → state_dir is parent of "state/".
    let layout = {
        let state_dir = db_path
            .parent()
            .and_then(|state_subdir| state_subdir.parent())
            .and_then(|p| camino::Utf8PathBuf::from_path_buf(p.to_path_buf()).ok());
        match state_dir {
            Some(sd) => crate::state::layout::Layout::from_roots(repo_root, sd),
            None => {
                tracing::debug!(
                    "Post-commit hook: cannot derive state_dir from db_path {}",
                    db_path.display()
                );
                return RecordOutcome::Silent;
            }
        }
    };
    let config = load_ledger_config(&layout).unwrap_or_default();

    let storage = match StorageManager::init_with_layout(&layout) {
        Ok(s) => s,
        Err(e) => {
            tracing::debug!("Post-commit hook: cannot open storage: {}", e);
            return RecordOutcome::Silent;
        }
    };

    let history_provider = GixHistoryProvider::new(&repo);
    let query = HotspotQuery {
        limit: config.hotspots.limit,
        commits: config.hotspots.max_commits,
        decay_half_life: config.hotspots.decay_half_life,
        budget: Some(budget.clone()),
        ..Default::default()
    };

    let calculated = match calculate_hotspots_detailed(&storage, &history_provider, &query) {
        Ok(calc) => calc,
        Err(e) => {
            tracing::debug!("Post-commit hook: hotspot calculation failed: {}", e);
            return RecordOutcome::Silent;
        }
    };

    if let Some(outcome) = outcome_for_walk(calculated.walk_stop, calculated.hotspots.is_empty()) {
        if outcome == RecordOutcome::Empty {
            tracing::debug!("Post-commit hook: no hotspots computed for this commit");
        }
        return outcome;
    }

    let timestamp = Utc::now().to_rfc3339();
    let inserted = match insert_hotspot_trends_with_retry(
        &storage,
        &calculated.hotspots,
        &head_hash,
        &timestamp,
    ) {
        Ok(kind) => kind,
        Err(e) => {
            if report_is_database_busy(&e).is_some() {
                return RecordOutcome::NotRecordedDatabaseLocked;
            }
            tracing::debug!("Post-commit hook: trend insert failed: {}", e);
            return RecordOutcome::Silent;
        }
    };
    if let Err(e) = upsert_project_trend_day(&storage, &timestamp) {
        tracing::debug!(
            "Post-commit hook: failed to upsert project trend day: {}",
            e
        );
    }
    tracing::debug!(
        "Post-commit hook: recorded {} hotspot trend rows for {}",
        calculated.hotspots.len(),
        head_hash
    );
    match inserted {
        TrendInsert::Inserted => RecordOutcome::Inserted,
        TrendInsert::Deduped => RecordOutcome::Deduped,
    }
}

/// Update the daily project trend rollup for the UTC date of `timestamp`.
///
/// This is best-effort: failures are logged and swallowed so the git commit
/// is never blocked. The rollup aggregates the latest `hotspot_trends`
/// snapshot for the UTC day using the same log-scale transform used for
/// per-file display scores.
/// Compute the project-level trend score from a collection of per-file hotspot
/// scores. Mirrors the `normalize_score` log-scale transform in the hotspot
/// module, clamped to the 0-100 range expected by the frontend `TrendPoint`.
fn project_score_from_hotspots(hotspots: &[Hotspot]) -> f64 {
    if hotspots.is_empty() {
        return 0.0;
    }
    let sum: f64 = hotspots
        .iter()
        .map(|h| normalize_score(h.score as f64))
        .sum();
    let avg = sum / hotspots.len() as f64;
    let scaled = avg * 20.0;
    scaled.clamp(0.0, 100.0)
}

/// Count distinct files whose log-scale hotspot score meets or exceeds the
/// HIGH threshold (3.0 on the display-score scale).
fn high_risk_count_from_hotspots(hotspots: &[Hotspot]) -> i64 {
    let mut max_scores: HashMap<String, f64> = HashMap::new();
    for h in hotspots {
        let path = h.path.to_string_lossy().to_string();
        let score = normalize_score(h.score as f64);
        max_scores
            .entry(path)
            .and_modify(|s| {
                if score > *s {
                    *s = score;
                }
            })
            .or_insert(score);
    }
    max_scores.values().filter(|&&s| s >= 3.0).count() as i64
}

/// Update the daily project trend rollup for the UTC date of `timestamp`.
///
/// This is best-effort: failures are logged and swallowed so the git commit
/// is never blocked. The rollup is computed in Rust because the bundled
/// SQLite build does not enable math functions (`ln()`).
fn upsert_project_trend_day(storage: &StorageManager, timestamp: &str) -> Result<()> {
    let conn = storage.get_connection();
    let day = day_from_rfc3339(timestamp);

    let mut stmt = conn
        .prepare(
            "SELECT file_path, score FROM hotspot_trends
             WHERE recorded_at = (
                 SELECT MAX(recorded_at) FROM hotspot_trends
                 WHERE strftime('%Y-%m-%d', recorded_at) = strftime('%Y-%m-%d', ?1)
             )",
        )
        .into_diagnostic()?;
    let rows: Vec<(String, f64)> = stmt
        .query_map([timestamp], |row| Ok((row.get(0)?, row.get(1)?)))
        .into_diagnostic()?
        .filter_map(|r| match r {
            Ok(v) => Some(v),
            Err(e) => {
                tracing::warn!("upsert_project_trend_day: skipping malformed row: {}", e);
                None
            }
        })
        .collect();
    drop(stmt);

    let hotspots: Vec<Hotspot> = rows
        .into_iter()
        .map(|(path, score)| Hotspot {
            path: std::path::PathBuf::from(path),
            score: score as f32,
            display_score: normalize_score(score) as f32,
            complexity: 0,
            frequency: 0.0,
            centrality: None,
        })
        .collect();

    // Each row in hotspot_trends is one file in the latest snapshot, so
    // hotspots.len() == COUNT(DISTINCT file_path) for this snapshot.
    let changes = hotspots.len() as i64;
    let score = project_score_from_hotspots(&hotspots);
    let high_risk_count = high_risk_count_from_hotspots(&hotspots);

    conn.execute(
        "INSERT OR REPLACE INTO project_trend_days (day, score, changes, high_risk_count) VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![day, score, changes, high_risk_count],
    )
    .into_diagnostic()?;
    Ok(())
}

/// Bounded, idempotent backfill of `project_trend_days` from existing
/// `hotspot_trends` rows.
///
/// Only re-aggregates the last `catchup_days` UTC days. Uses `INSERT OR
/// REPLACE` keyed on `day`, so it is safe to run repeatedly. This is the
/// catch-up path for repos that already had `hotspot_trends` rows before
/// migration m49 created the rollup table.
pub fn catch_up_project_trends(storage: &StorageManager, catchup_days: u32) -> Result<()> {
    let cutoff = (Utc::now() - ChronoDuration::days(catchup_days as i64)).to_rfc3339();
    let conn = storage.get_connection();

    let mut days_stmt = conn
        .prepare(
            "SELECT strftime('%Y-%m-%d', recorded_at) AS day, MAX(recorded_at) AS max_recorded
             FROM hotspot_trends
             WHERE recorded_at >= ?1
             GROUP BY strftime('%Y-%m-%d', recorded_at)",
        )
        .into_diagnostic()?;
    let days: Vec<(String, String)> = days_stmt
        .query_map([&cutoff], |row| Ok((row.get(0)?, row.get(1)?)))
        .into_diagnostic()?
        .filter_map(|r| match r {
            Ok(v) => Some(v),
            Err(e) => {
                tracing::warn!("catch_up_project_trends: skipping malformed day row: {}", e);
                None
            }
        })
        .collect();
    drop(days_stmt);

    let mut insert_stmt = conn
        .prepare(
            "INSERT OR REPLACE INTO project_trend_days (day, score, changes, high_risk_count) VALUES (?1, ?2, ?3, ?4)",
        )
        .into_diagnostic()?;

    for (day, max_recorded) in days {
        let mut rows_stmt = conn
            .prepare("SELECT file_path, score FROM hotspot_trends WHERE recorded_at = ?1")
            .into_diagnostic()?;
        let rows: Vec<(String, f64)> = rows_stmt
            .query_map([&max_recorded], |row| Ok((row.get(0)?, row.get(1)?)))
            .into_diagnostic()?
            .filter_map(|r| match r {
                Ok(v) => Some(v),
                Err(e) => {
                    tracing::warn!("catch_up_project_trends: skipping malformed row: {}", e);
                    None
                }
            })
            .collect();
        drop(rows_stmt);

        let hotspots: Vec<Hotspot> = rows
            .into_iter()
            .map(|(path, score)| Hotspot {
                path: std::path::PathBuf::from(path),
                score: score as f32,
                display_score: normalize_score(score) as f32,
                complexity: 0,
                frequency: 0.0,
                centrality: None,
            })
            .collect();

        let changes = hotspots.len() as i64;
        let score = project_score_from_hotspots(&hotspots);
        let high_risk_count = high_risk_count_from_hotspots(&hotspots);

        insert_stmt
            .execute(rusqlite::params![day, score, changes, high_risk_count])
            .into_diagnostic()?;
    }

    Ok(())
}

fn day_from_rfc3339(timestamp: &str) -> String {
    match chrono::DateTime::parse_from_rfc3339(timestamp) {
        Ok(dt) => dt.with_timezone(&Utc).format("%Y-%m-%d").to_string(),
        Err(_) => {
            tracing::warn!(
                "day_from_rfc3339: malformed timestamp '{}', falling back to 1970-01-01",
                timestamp
            );
            "1970-01-01".to_string()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::migrations::get_migrations;
    use crate::state::storage::StorageManager;
    use crate::state::storage::hotspot_trends::insert_hotspot_trends;
    use rusqlite::Connection;
    use std::path::PathBuf;

    fn in_memory_storage() -> StorageManager {
        let mut conn = Connection::open_in_memory().unwrap();
        get_migrations().to_latest(&mut conn).unwrap();
        StorageManager::init_from_conn(conn)
    }

    fn sample_hotspots() -> Vec<Hotspot> {
        vec![
            Hotspot {
                path: PathBuf::from("src/a.rs"),
                score: 0.5,
                display_score: 1.0,
                complexity: 3,
                frequency: 2.0,
                centrality: None,
            },
            Hotspot {
                path: PathBuf::from("src/b.rs"),
                score: 0.3,
                display_score: 0.5,
                complexity: 2,
                frequency: 1.0,
                centrality: None,
            },
        ]
    }

    fn make_hotspot(path: &str, score: f32) -> Hotspot {
        Hotspot {
            path: PathBuf::from(path),
            score,
            display_score: normalize_score(score as f64) as f32,
            complexity: 1,
            frequency: 1.0,
            centrality: None,
        }
    }

    #[test]
    fn upsert_project_trend_day_computes_rollup_from_hotspot_trends() {
        let storage = in_memory_storage();
        let hotspots = vec![
            make_hotspot("src/high.rs", 0.03),
            make_hotspot("src/mid.rs", 0.01),
        ];
        insert_hotspot_trends(&storage, &hotspots, "abc123", "2026-06-23T10:00:00Z").unwrap();

        upsert_project_trend_day(&storage, "2026-06-23T10:00:00Z").unwrap();

        let conn = storage.get_connection();
        let (day, score, changes, high_risk_count): (String, f64, i64, i64) = conn
            .query_row(
                "SELECT day, score, changes, high_risk_count FROM project_trend_days",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(day, "2026-06-23");
        assert_eq!(changes, 2);
        assert_eq!(high_risk_count, 1);
        assert!(
            (score - 58.32).abs() < 0.01,
            "expected score ~58.32, got {}",
            score
        );
    }
    #[test]
    fn upsert_project_trend_day_is_idempotent_for_same_day() {
        let storage = in_memory_storage();
        let hotspots = vec![make_hotspot("src/a.rs", 0.01)];
        insert_hotspot_trends(&storage, &hotspots, "abc123", "2026-06-23T10:00:00Z").unwrap();
        upsert_project_trend_day(&storage, "2026-06-23T10:00:00Z").unwrap();
        upsert_project_trend_day(&storage, "2026-06-23T10:00:00Z").unwrap();

        let conn = storage.get_connection();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM project_trend_days", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn upsert_project_trend_day_uses_utc_date_not_localtime() {
        let storage = in_memory_storage();
        let hotspots = vec![make_hotspot("src/a.rs", 0.01)];
        // Late in the UTC day — should still bucket to the UTC date.
        insert_hotspot_trends(&storage, &hotspots, "abc123", "2026-06-23T23:30:00Z").unwrap();
        upsert_project_trend_day(&storage, "2026-06-23T23:30:00Z").unwrap();

        let conn = storage.get_connection();
        let day: String = conn
            .query_row("SELECT day FROM project_trend_days", [], |row| row.get(0))
            .unwrap();
        assert_eq!(day, "2026-06-23");
    }

    #[test]
    fn upsert_project_trend_day_clamps_score_to_zero_and_one_hundred() {
        let storage = in_memory_storage();
        let low = make_hotspot("src/low.rs", 0.0001);
        let high = make_hotspot("src/high.rs", 10.0);
        insert_hotspot_trends(&storage, &[low, high], "abc123", "2026-06-23T10:00:00Z").unwrap();
        upsert_project_trend_day(&storage, "2026-06-23T10:00:00Z").unwrap();

        let conn = storage.get_connection();
        let score: f64 = conn
            .query_row("SELECT score FROM project_trend_days", [], |row| row.get(0))
            .unwrap();
        assert!(
            (0.0..=100.0).contains(&score),
            "score {} should be clamped to [0, 100]",
            score
        );
    }

    #[test]
    fn day_from_rfc3339_handles_offset_bearing_timestamp() {
        // 23:30 with -05:00 offset → UTC is 04:30 next day → day should be 2026-06-24
        let day = day_from_rfc3339("2026-06-23T23:30:00-05:00");
        assert_eq!(day, "2026-06-24");
    }

    #[test]
    fn day_from_rfc3339_handles_zulu_timestamp() {
        let day = day_from_rfc3339("2026-06-23T10:00:00Z");
        assert_eq!(day, "2026-06-23");
    }

    #[test]
    fn catch_up_project_trends_is_bounded_and_idempotent() {
        let storage = in_memory_storage();
        let old = vec![make_hotspot("src/old.rs", 0.05)];
        let recent = vec![make_hotspot("src/recent.rs", 0.02)];
        // Use relative dates so the test is not time-sensitive.
        let now = Utc::now();
        let old_date = (now - ChronoDuration::days(100))
            .format("%Y-%m-%dT10:00:00Z")
            .to_string();
        let recent_date = (now - ChronoDuration::days(10))
            .format("%Y-%m-%dT10:00:00Z")
            .to_string();
        let old_day = old_date[..10].to_string();
        let recent_day = recent_date[..10].to_string();
        // 100 days ago should be excluded by default 90-day catch-up.
        insert_hotspot_trends(&storage, &old, "old123", &old_date).unwrap();
        insert_hotspot_trends(&storage, &recent, "recent123", &recent_date).unwrap();

        catch_up_project_trends(&storage, 90).unwrap();
        let first_run: Vec<String> = storage
            .get_connection()
            .prepare("SELECT day FROM project_trend_days ORDER BY day")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();

        catch_up_project_trends(&storage, 90).unwrap();
        let second_run: Vec<String> = storage
            .get_connection()
            .prepare("SELECT day FROM project_trend_days ORDER BY day")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();

        assert_eq!(first_run, second_run);
        assert!(first_run.contains(&recent_day));
        assert!(!first_run.contains(&old_day));
    }

    #[test]
    fn record_hotspot_trends_inserts_data_on_simulated_commit() {
        let storage = in_memory_storage();
        let hotspots = sample_hotspots();

        insert_hotspot_trends(&storage, &hotspots, "abc123", "2026-06-23T10:00:00Z").unwrap();

        let conn = storage.get_connection();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM hotspot_trends", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 2);

        let file_a: String = conn
            .query_row(
                "SELECT file_path FROM hotspot_trends WHERE commit_hash = 'abc123' AND file_path = 'src/a.rs'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(file_a, "src/a.rs");
    }

    #[test]
    fn post_commit_budget_stop_with_files_records_nothing() {
        assert_eq!(
            outcome_for_walk(HistoryWalkStop::Budget, false),
            Some(RecordOutcome::NotRecordedHistoryBudget)
        );
        assert_eq!(
            outcome_for_walk(HistoryWalkStop::Complete, true),
            Some(RecordOutcome::Empty)
        );
        assert_eq!(outcome_for_walk(HistoryWalkStop::Complete, false), None);
    }

    #[test]
    fn post_commit_silent_outcome_has_no_warning() {
        assert_eq!(warning_line(RecordOutcome::Silent), None);
        assert_eq!(
            warning_line(RecordOutcome::NotRecordedDatabaseLocked),
            Some(DATABASE_LOCKED_WARNING)
        );
        assert_ne!(
            warning_line(RecordOutcome::NotRecordedDatabaseLocked),
            Some(DEADLINE_WARNING)
        );
        assert_eq!(
            warning_line(RecordOutcome::NotRecordedHistoryBudget),
            Some(HISTORY_BUDGET_WARNING)
        );
    }

    #[test]
    fn post_commit_records_trends_before_incremental_index() {
        let src = include_str!("hook_post_commit.rs");
        let start = src
            .find("fn execute_hook_post_commit_for_layout")
            .expect("hook function");
        let rest = &src[start..];
        let end = rest
            .find("\nfn promote_pending_ledger_tx")
            .expect("next function");
        let body = &rest[..end];
        let send = body.find("outcome_tx.send").expect("outcome send");
        let index = body.find("incremental_index").expect("index call");
        assert!(send < index, "outcome send must precede incremental_index");
    }

    #[test]
    fn post_commit_expired_budget_records_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let git = |args: &[&str]| {
            let status = std::process::Command::new("git")
                .args(args)
                .current_dir(root)
                .status()
                .unwrap();
            assert!(status.success(), "git {args:?} failed");
        };
        git(&["init"]);
        git(&["config", "user.email", "ledgerful-test@example.com"]);
        git(&["config", "user.name", "ledgerful-test"]);
        std::fs::write(root.join("a.rs"), "fn a() {}\n").unwrap();
        git(&["add", "a.rs"]);
        git(&["commit", "--no-gpg-sign", "-m", "init"]);

        let repo_root = camino::Utf8PathBuf::from_path_buf(root.to_path_buf()).unwrap();
        let layout = crate::state::layout::Layout::new(repo_root.clone());
        layout.ensure_state_dir().unwrap();
        let db_path = layout.state_subdir().join("ledger.db");
        let storage = StorageManager::init(db_path.as_std_path()).unwrap();
        drop(storage);

        let cancel = Arc::new(AtomicBool::new(false));
        let budget = AnalysisBudget::expired(cancel);
        assert!(budget.deadline.is_some());
        let outcome = record_hotspot_trends_with_budget(&repo_root, db_path.as_std_path(), &budget);
        assert_eq!(outcome, RecordOutcome::NotRecordedHistoryBudget);
        assert_eq!(warning_line(outcome), Some(HISTORY_BUDGET_WARNING));

        let storage = StorageManager::init(db_path.as_std_path()).unwrap();
        let count: i64 = storage
            .get_connection()
            .query_row("SELECT COUNT(*) FROM hotspot_trends", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);
        let days: i64 = storage
            .get_connection()
            .query_row("SELECT COUNT(*) FROM project_trend_days", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(days, 0);
    }
}
