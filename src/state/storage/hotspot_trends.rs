//! The `hotspot_trends` insert (0467). The post-commit hook and `hotspots trend`
//! call `insert_hotspot_trends_with_retry`. Dedup and the busy retry stay as
//! shipped. This module does not calculate hotspots.

use super::StorageManager;
use crate::impact::packet::Hotspot;
use miette::{IntoDiagnostic, Result};
use std::thread;
use std::time::Duration;

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
pub(crate) fn insert_hotspot_trends(
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
