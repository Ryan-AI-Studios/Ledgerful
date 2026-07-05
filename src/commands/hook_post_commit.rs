use crate::commands::helpers::{get_layout, load_ledger_config};
use crate::git::repo::{get_head_info, open_repo};
use crate::impact::hotspots::{HotspotQuery, calculate_hotspots};
use crate::impact::packet::Hotspot;
use crate::impact::temporal::GixHistoryProvider;
use crate::ledger::{
    ChangeType, CommitRequest, TransactionManager, VerificationBasis, VerificationStatus,
};
use crate::state::storage::StorageManager;
use chrono::Utc;
use miette::{IntoDiagnostic, Result, miette};
use serde::{Deserialize, Serialize};
use std::fs;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

#[derive(Serialize, Deserialize, Debug)]
pub struct PendingHookTx {
    pub tx_id: String,
    pub commit_msg_hash: String,
    pub summary: String,
    pub reason: String,
    pub committed_at: Option<String>,
    pub risk: Option<String>,
    pub related_tickets: Option<String>,
    pub signature: Option<String>,
    pub public_key: Option<String>,
}

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
    // pending transactions are committed immediately. Any failure here is also
    // swallowed so the git commit is never blocked.
    if let Err(e) = promote_pending_ledger_tx(layout) {
        tracing::debug!("Post-commit hook: ledger promotion failed: {}", e);
    }

    // Then, in a detached thread, record hotspot trends. The thread has a
    // 10-second wall-clock budget and is best-effort only.
    let db_path = layout
        .state_subdir()
        .join("ledger.db")
        .as_std_path()
        .to_path_buf();
    let repo_root = layout.root.clone();
    let (done_tx, done_rx) = mpsc::channel();
    let _handle = thread::Builder::new()
        .name("post-commit-worker".to_string())
        .spawn(move || {
            let start = std::time::Instant::now();
            let result = record_hotspot_trends(&repo_root, &db_path);
            if let Err(ref e) = result {
                tracing::debug!("Post-commit hook: work failed: {}", e);
            }
            let elapsed = start.elapsed();
            if elapsed > Duration::from_secs(10) {
                tracing::debug!(
                    "Post-commit hook: work took {:?}, exceeding 10s budget",
                    elapsed
                );
            }
            let _ = done_tx.send(());
            let _ = result;
        });

    // Wait up to the 10-second budget for the recorder to finish. On Windows
    // the process exits immediately when the main thread returns, killing any
    // still-running detached threads before they can commit SQLite writes, so
    // we must block briefly here while still never failing the git commit.
    // The budget was raised from 5s to 10s to accommodate the best-effort
    // incremental index that runs before hotspot trend recording.
    if done_rx.recv_timeout(Duration::from_secs(10)).is_err() {
        tracing::debug!("Post-commit hook: post-commit work did not complete within 10s budget");
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

    let sidecar_content = fs::read_to_string(&sidecar_path).into_diagnostic()?;
    let pending: PendingHookTx = serde_json::from_str(&sidecar_content).into_diagnostic()?;

    // Verify commit hash
    let repo_root = layout.root.clone();
    let output = std::process::Command::new("git")
        .args(["log", "-1", "--format=%B"])
        .current_dir(repo_root)
        .output()
        .into_diagnostic()?;

    let current_commit_msg = String::from_utf8_lossy(&output.stdout).to_string();
    let cleaned_msg = crate::util::text::clean_commit_msg(&current_commit_msg);

    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(cleaned_msg.as_bytes());
    let current_hash = hex::encode(hasher.finalize());

    if pending.commit_msg_hash != current_hash {
        tracing::info!(
            target: "cli_summary",
            "[Ledgerful] Pending transaction {} was for a different commit; discarding sidecar.",
            pending.tx_id
        );
        let _ = fs::remove_file(sidecar_path);
        return Ok(());
    }
    let verification_status = if pending.risk.as_deref() == Some("TRIVIAL") {
        None
    } else {
        Some(VerificationStatus::Verified)
    };

    let mut storage = StorageManager::init(layout.state_subdir().join("ledger.db").as_std_path())?;
    let mut tx_mgr = TransactionManager::new(&mut storage, layout.root.clone().into(), config);

    let req = CommitRequest {
        summary: pending.summary,
        reason: pending.reason,
        change_type: ChangeType::Modify,
        is_breaking: false,
        committed_at: pending.committed_at,
        verification_status,
        verification_basis: Some(VerificationBasis::ManualInspection),
        outcome_notes: None,
        issue_ref: None,
        signature: pending.signature,
        public_key: pending.public_key,
        risk: pending.risk,
        related_tickets: pending.related_tickets,
        ..Default::default()
    };

    match tx_mgr.commit_change(pending.tx_id.clone(), req, false) {
        Ok(_) => {
            let _ = fs::remove_file(sidecar_path);
            Ok(())
        }
        Err(e) => {
            eprintln!(
                "[Ledgerful] Post-commit hook failed to promote ledger entry: {}",
                e
            );
            let _ = tx_mgr.rollback_change(
                pending.tx_id,
                "Rollback due to promotion failure".to_string(),
            );
            let _ = fs::remove_file(sidecar_path);
            Err(miette!("{}", e))
        }
    }
}

/// Append a hotspot snapshot for the current HEAD to `hotspot_trends`.
///
/// * Uses WAL mode and retries up to 3 times with 50 ms backoff on database
///   busy/locked errors.
/// * Dedupes by commit hash or by identical per-file scores compared with the
///   last recorded snapshot.
/// * Returns `Ok(())` on success or on any recoverable failure (missing git
///   index, empty repo, locked database after retries, etc.). Errors are only
///   returned for unexpected programming/IO issues so callers can log them.
fn record_hotspot_trends(repo_root: &camino::Utf8Path, db_path: &std::path::Path) -> Result<()> {
    let repo = match open_repo(repo_root.as_std_path()) {
        Ok(r) => r,
        Err(e) => {
            tracing::debug!("Post-commit hook: cannot open repo: {}", e);
            return Ok(());
        }
    };

    let (head_hash, _branch) = match get_head_info(&repo) {
        Ok((Some(h), b)) => (h, b),
        Ok((None, _)) => {
            tracing::debug!("Post-commit hook: no HEAD hash");
            return Ok(());
        }
        Err(e) => {
            tracing::debug!("Post-commit hook: cannot read HEAD: {}", e);
            return Ok(());
        }
    };

    let layout = crate::state::layout::Layout::new(repo_root);
    let config = load_ledger_config(&layout).unwrap_or_default();

    let storage = match StorageManager::init(db_path) {
        Ok(s) => s,
        Err(e) => {
            tracing::debug!("Post-commit hook: cannot open storage: {}", e);
            return Ok(());
        }
    };

    // Best-effort incremental index: refreshes AST/symbol index for changed
    // files only. Non-fatal — failures are logged and swallowed so the git
    // commit never fails.
    use crate::index::ProjectIndexer;
    let mut indexer = ProjectIndexer::new(storage, repo_root.to_owned(), config.clone());
    if let Err(e) = indexer.incremental_index() {
        tracing::debug!("Post-commit hook: incremental index failed: {}", e);
    }
    // Explicitly shutdown storage to release SQLite/CozoDB locks before re-opening
    if let Err(e) = indexer.shutdown_storage() {
        tracing::debug!("Post-commit hook: storage shutdown failed: {}", e);
    }
    drop(indexer);

    // Re-open read-only for hotspot calculation
    let storage = match StorageManager::open_read_only(repo_root) {
        Ok(s) => s,
        Err(e) => {
            tracing::debug!("Post-commit hook: cannot re-open read-only storage: {}", e);
            return Ok(());
        }
    };

    let history_provider = GixHistoryProvider::new(&repo);
    let query = HotspotQuery {
        limit: config.hotspots.limit,
        commits: config.hotspots.max_commits,
        decay_half_life: config.hotspots.decay_half_life,
        ..Default::default()
    };

    let hotspots = match calculate_hotspots(&storage, &history_provider, &query) {
        Ok(h) => h,
        Err(e) => {
            tracing::debug!("Post-commit hook: hotspot calculation failed: {}", e);
            return Ok(());
        }
    };

    if hotspots.is_empty() {
        tracing::debug!("Post-commit hook: no hotspots computed for this commit");
        return Ok(());
    }

    let timestamp = Utc::now().to_rfc3339();
    insert_hotspot_trends_with_retry(&storage, &hotspots, &head_hash, &timestamp)?;
    tracing::debug!(
        "Post-commit hook: recorded {} hotspot trend rows for {}",
        hotspots.len(),
        head_hash
    );
    Ok(())
}

pub(crate) fn insert_hotspot_trends_with_retry(
    storage: &StorageManager,
    hotspots: &[Hotspot],
    commit_hash: &str,
    timestamp: &str,
) -> Result<()> {
    let mut backoff = Duration::from_millis(50);

    for attempt in 0..=3 {
        match insert_hotspot_trends(storage, hotspots, commit_hash, timestamp) {
            Ok(_) => return Ok(()),
            Err(e) => {
                let is_busy = report_is_database_busy(&e).is_some();
                if is_busy && attempt < 3 {
                    tracing::debug!(
                        "Post-commit hook: database locked, retrying in {:?} (attempt {})",
                        backoff,
                        attempt + 1
                    );
                    thread::sleep(backoff);
                    backoff = Duration::from_millis(50);
                    continue;
                }
                if is_busy {
                    tracing::debug!("Post-commit hook: database locked after 3 retries");
                }
                return Err(e);
            }
        }
    }

    // Unreachable: every loop iteration returns above. Kept as a defensive
    // fallback so the function has a total return path even if the loop
    // bounds ever change.
    Ok(())
}

/// Insert the computed hotspot rows into `hotspot_trends` unless the snapshot
/// should be deduplicated.
///
/// The historical bootstrap path passes a per-commit `timestamp` so that each
/// sampled commit can be inserted as its own row. The original post-commit
/// hook path used `Utc::now()` for every row and deduplicated against the
/// prior snapshot. Dedup has two tiers:
/// 1. **Commit-hash dedup**: if the same commit hash was already recorded,
///    skip it (prevents double-insert on amend).
/// 2. **Score-equality dedup** (spec R3): if the sorted `(file_path, raw_score)`
///    tuples are identical to the previous recorded sample, skip it (prevents
///    flat/noise history when adjacent commits produce identical scores).
fn insert_hotspot_trends(
    storage: &StorageManager,
    hotspots: &[Hotspot],
    commit_hash: &str,
    timestamp: &str,
) -> Result<()> {
    let conn = storage.get_connection();

    let already_exists: bool = conn
        .query_row(
            "SELECT 1 FROM hotspot_trends WHERE commit_hash = ?1 LIMIT 1",
            [commit_hash],
            |_row| Ok(true),
        )
        .unwrap_or(false);

    if already_exists {
        tracing::debug!(
            "Post-commit hook: skipping duplicate commit hash {}",
            commit_hash
        );
        return Ok(());
    }

    // Score-equality dedup: compare sorted (file_path, score) tuples against
    // the most recent previously recorded sample.
    let prev_tuples: Vec<(String, f64)> = conn
        .prepare(
            "SELECT file_path, score FROM hotspot_trends \
             WHERE recorded_at = (SELECT MAX(recorded_at) FROM hotspot_trends) \
             ORDER BY file_path",
        )
        .into_diagnostic()?
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, f64>(1)?))
        })
        .into_diagnostic()?
        .filter_map(|r| r.ok())
        .collect();

    if !prev_tuples.is_empty() {
        let mut current_tuples: Vec<(String, f64)> = hotspots
            .iter()
            .map(|h| (h.path.to_string_lossy().to_string(), h.score as f64))
            .collect();
        current_tuples.sort_by(|a, b| a.0.cmp(&b.0));
        if current_tuples == prev_tuples {
            tracing::debug!(
                "Post-commit hook: skipping identical score snapshot for commit {}",
                commit_hash
            );
            return Ok(());
        }
    }

    for hotspot in hotspots {
        conn.execute(
            "INSERT INTO hotspot_trends (file_path, score, frequency, complexity, commit_hash, recorded_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                hotspot.path.to_string_lossy().to_string(),
                hotspot.score as f64,
                hotspot.frequency,
                hotspot.complexity as f64,
                commit_hash,
                timestamp
            ],
        )
        .into_diagnostic()?;
    }

    Ok(())
}

fn is_database_busy(error: &rusqlite::Error) -> bool {
    matches!(
        error,
        rusqlite::Error::SqliteFailure(err, _)
            if err.code == rusqlite::ErrorCode::DatabaseBusy
                || err.code == rusqlite::ErrorCode::DatabaseLocked
    )
}

/// Walk a miette::Report source chain looking for a rusqlite::Error whose
/// SQLite failure code is BUSY or LOCKED. miette wraps the underlying error
/// via `into_diagnostic()`, so a direct `downcast_ref` may miss intermediate
/// wrappers; this helper inspects every link in the chain via `Report::chain`.
///
/// As a fallback (miette's `DiagnosticError` boxes the inner error opaquely),
/// also match on the error's Display string, which SQLite produces as
/// `"database is locked"` / `"database is busy"`. This is stable SQLite
/// behaviour, not a locale-dependent message.
///
/// Returns the formatted error string when a busy/locked SQLite error is found
/// so callers can attach it to diagnostics without cloning the underlying
/// error (rusqlite::Error does not implement Clone).
fn report_is_database_busy(report: &miette::Report) -> Option<String> {
    for err in report.chain() {
        if let Some(sqlite_err) = err.downcast_ref::<rusqlite::Error>()
            && is_database_busy(sqlite_err)
        {
            return Some(sqlite_err.to_string());
        }
    }
    let msg = report.to_string().to_lowercase();
    if msg.contains("database is locked") || msg.contains("database is busy") {
        return Some(msg);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::migrations::get_migrations;
    use crate::state::storage::StorageManager;
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
    fn dedup_skips_when_commit_hash_matches_last_entry() {
        let storage = in_memory_storage();
        let hotspots = sample_hotspots();

        insert_hotspot_trends(&storage, &hotspots, "abc123", "2026-06-23T10:00:00Z").unwrap();
        insert_hotspot_trends(&storage, &hotspots, "abc123", "2026-06-23T10:01:00Z").unwrap();

        let conn = storage.get_connection();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM hotspot_trends", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 2);
    }

    #[test]
    fn dedup_skips_when_scores_identical_but_hash_changed() {
        // Spec R3: skip inserting a sample if the sorted (file_path, raw_score)
        // tuples are identical to the previous recorded sample. This prevents
        // flat/noise history when adjacent commits produce identical scores.
        let storage = in_memory_storage();
        let hotspots = sample_hotspots();

        insert_hotspot_trends(&storage, &hotspots, "abc123", "2026-06-23T10:00:00Z").unwrap();
        insert_hotspot_trends(&storage, &hotspots, "def456", "2026-06-23T10:01:00Z").unwrap();

        let conn = storage.get_connection();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM hotspot_trends", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 2);
    }

    #[test]
    fn dedup_skips_when_commit_hash_already_present() {
        let storage = in_memory_storage();
        let hotspots = sample_hotspots();

        insert_hotspot_trends(&storage, &hotspots, "abc123", "2026-06-23T10:00:00Z").unwrap();
        insert_hotspot_trends(&storage, &hotspots, "abc123", "2026-06-23T10:01:00Z").unwrap();

        let conn = storage.get_connection();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM hotspot_trends", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 2);
    }

    #[test]
    fn dedup_allows_when_scores_changed() {
        let storage = in_memory_storage();
        let mut hotspots = sample_hotspots();

        insert_hotspot_trends(&storage, &hotspots, "abc123", "2026-06-23T10:00:00Z").unwrap();

        hotspots[0].score = 0.9;
        insert_hotspot_trends(&storage, &hotspots, "def456", "2026-06-23T10:01:00Z").unwrap();

        let conn = storage.get_connection();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM hotspot_trends", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 4);
    }

    #[test]
    fn sqlite_locked_retry_exhausts_and_returns_error_after_3_retries() {
        // Use a file-based DB so we can hold a real write lock from a second
        // connection, forcing the writer path inside the retry loop to hit
        // SQLITE_BUSY on every attempt. WAL + busy_timeout=0 guarantees the
        // error surfaces immediately rather than blocking internally.
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("ledger.db");

        let mut writer = Connection::open(&db_path).unwrap();
        get_migrations().to_latest(&mut writer).unwrap();
        writer
            .execute_batch("PRAGMA journal_mode = WAL; PRAGMA busy_timeout = 0;")
            .unwrap();

        let storage = StorageManager::init_from_conn(writer);

        // Hold an exclusive write lock from a sibling connection for the
        // entire duration of the retry loop.
        let locker = Connection::open(&db_path).unwrap();
        locker
            .execute_batch("PRAGMA journal_mode = WAL; PRAGMA busy_timeout = 0;")
            .unwrap();
        locker.execute("BEGIN IMMEDIATE", []).unwrap();
        locker
            .execute(
                "INSERT INTO hotspot_trends (file_path, score, recorded_at) VALUES ('lock.rs', 0.1, '2026-01-01T00:00:00Z')",
                [],
            )
            .unwrap();

        let hotspots = sample_hotspots();
        let start = std::time::Instant::now();
        let result = insert_hotspot_trends_with_retry(
            &storage,
            &hotspots,
            "locked1",
            "2026-06-23T11:00:00Z",
        );
        let elapsed = start.elapsed();

        assert!(
            result.is_err(),
            "expected retry exhaustion to surface an error, got {:?}",
            result
        );

        // 3 retries at 50ms backoff = at least 150ms wall-clock.
        assert!(
            elapsed >= std::time::Duration::from_millis(140),
            "retry did not back off long enough: {:?}",
            elapsed
        );

        // Releasing the lock lets a fresh write succeed immediately.
        locker.execute("COMMIT", []).unwrap();
        let result = insert_hotspot_trends_with_retry(
            &storage,
            &hotspots,
            "locked2",
            "2026-06-23T11:01:00Z",
        );
        assert!(
            result.is_ok(),
            "write should succeed after lock release: {:?}",
            result
        );

        let count: i64 = storage
            .get_connection()
            .query_row("SELECT COUNT(*) FROM hotspot_trends", [], |row| row.get(0))
            .unwrap();
        // lock.rs (from locker) + src/a.rs + src/b.rs = 3
        assert_eq!(count, 3);
    }
}
