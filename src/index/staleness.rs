use crate::state::layout::Layout;
use crate::state::storage::StorageManager;
use chrono::Utc;
use miette::Result;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum IndexFreshnessState {
    NeverIndexed,
    FreshEmpty,
    FreshPopulated,
    StaleEmpty,
    StalePopulated,
    Indeterminate,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum EmptyIndexReason {
    RepositoryEmpty,
    NoSupportedFiles,
    AllIndexableCandidatesIgnored,
    FilteredByConfiguration,
    UnknownPartial,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmptyDiscoveryDiagnostics {
    pub visible_files_examined: usize,
    pub ignored_indexable_candidates_lower_bound: usize,
    pub configured_exclusions_lower_bound: usize,
    pub scan_complete: bool,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum FreshnessSource {
    RepositoryMetadata,
    LegacyProjectFiles,
    None,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexFreshnessAssessment {
    pub state: IndexFreshnessState,
    pub empty_reason: Option<EmptyIndexReason>,
    pub empty_diagnostics: Option<EmptyDiscoveryDiagnostics>,
    pub last_indexed_at: Option<String>,
    pub days_since_indexed: Option<u64>,
    pub indexed_files: usize,
    pub stale_files: usize,
    pub unindexed_files: usize,
    pub sample_paths: Vec<String>,
    pub source: FreshnessSource,
    pub warnings: Vec<String>,
}

pub fn assess_index_freshness(
    storage: &StorageManager,
    threshold_days: u64,
) -> IndexFreshnessAssessment {
    assess_index_freshness_at(storage, threshold_days, Utc::now())
}

pub fn assess_index_freshness_at(
    storage: &StorageManager,
    threshold_days: u64,
    now: chrono::DateTime<Utc>,
) -> IndexFreshnessAssessment {
    let conn = storage.get_connection();

    let meta_indexed: Result<Option<String>, rusqlite::Error> = conn.query_row(
        "SELECT value FROM index_metadata WHERE key = 'last_indexed_at'",
        [],
        |row| row.get(0),
    );

    let mut warnings = Vec::new();

    let (source, ts_str, db_err) = match meta_indexed {
        Ok(Some(val)) => (FreshnessSource::RepositoryMetadata, Some(val), false),
        Ok(None) => {
            // Missing table is Ok(None) or Err depending on if table exists.
            // Wait, if table doesn't exist, it returns Err.
            (FreshnessSource::None, None, false)
        }
        Err(e) => {
            if e.to_string().contains("no such table")
                || matches!(e, rusqlite::Error::QueryReturnedNoRows)
            {
                (FreshnessSource::None, None, false)
            } else {
                warnings.push(format!("Database error reading metadata: {}", e));
                (FreshnessSource::None, None, true)
            }
        }
    };

    let (final_source, final_ts, mut warnings) = if db_err {
        (FreshnessSource::RepositoryMetadata, None, warnings)
    } else if source == FreshnessSource::None {
        // Legacy fallback
        let max_indexed: Result<Option<String>, rusqlite::Error> = conn.query_row(
            "SELECT MAX(last_indexed_at) FROM project_files WHERE parse_status != 'DELETED'",
            [],
            |row| row.get(0),
        );
        match max_indexed {
            Ok(Some(val)) => (FreshnessSource::LegacyProjectFiles, Some(val), warnings),
            _ => (FreshnessSource::None, None, warnings),
        }
    } else {
        (source, ts_str, warnings)
    };

    let active_rows: usize = conn
        .query_row(
            "SELECT COUNT(*) FROM project_files WHERE parse_status != 'DELETED'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .unwrap_or(0) as usize;

    let stale_files = active_rows;

    let dt = match final_ts {
        Some(ref ts) => match chrono::DateTime::parse_from_rfc3339(ts) {
            Ok(d) => Some(d.with_timezone(&Utc)),
            Err(e) => {
                warnings.push(format!("Malformed timestamp in metadata: {}", e));
                None
            }
        },
        None => None,
    };

    let mut state = IndexFreshnessState::Indeterminate;
    let mut days_since = None;

    if db_err {
        state = IndexFreshnessState::Indeterminate;
    } else if final_source == FreshnessSource::None && active_rows == 0 {
        state = IndexFreshnessState::NeverIndexed;
    } else if let Some(parsed_dt) = dt {
        let diff = now - parsed_dt;
        let days = diff.num_days();

        if days < -1 {
            // Clock skew tolerance of 1 day
            warnings.push("Future timestamp detected (clock skew).".to_string());
            state = IndexFreshnessState::Indeterminate;
        } else {
            let clamped_days = if days < 0 { 0 } else { days as u64 };
            days_since = Some(clamped_days);

            if clamped_days <= threshold_days {
                if active_rows == 0 {
                    state = IndexFreshnessState::FreshEmpty;
                } else {
                    state = IndexFreshnessState::FreshPopulated;
                }
            } else {
                if active_rows == 0 {
                    state = IndexFreshnessState::StaleEmpty;
                } else {
                    state = IndexFreshnessState::StalePopulated;
                }
            }
        }
    } else if final_ts.is_none() && active_rows > 0 {
        // Legacy with active rows but NO timestamp?
        state = IndexFreshnessState::NeverIndexed; // Actually shouldn't happen
    }

    // Unindexed files logic placeholder
    let unindexed_files = 0;

    let mut sample_paths = Vec::new();
    if state == IndexFreshnessState::StalePopulated
        || state == IndexFreshnessState::FreshPopulated
        || state == IndexFreshnessState::Indeterminate
    {
        let mut stmt = conn
            .prepare("SELECT file_path FROM project_files WHERE parse_status != 'DELETED' ORDER BY file_path LIMIT 3")
            .ok();
        if let Some(ref mut stmt) = stmt {
            sample_paths = stmt
                .query_map([], |row| row.get::<_, String>(0))
                .ok()
                .map(|iter| iter.filter_map(|r| r.ok()).collect())
                .unwrap_or_default();
        }
    }

    // Try to load empty_reason if applicable
    let mut empty_reason = None;
    let empty_diagnostics = None;

    if state == IndexFreshnessState::FreshEmpty || state == IndexFreshnessState::StaleEmpty {
        let reason_str: Option<String> = conn
            .query_row(
                "SELECT value FROM index_metadata WHERE key = 'empty_reason'",
                [],
                |row| row.get(0),
            )
            .ok()
            .flatten();

        if let Some(r) = reason_str {
            empty_reason = match r.as_str() {
                "RepositoryEmpty" => Some(EmptyIndexReason::RepositoryEmpty),
                "NoSupportedFiles" => Some(EmptyIndexReason::NoSupportedFiles),
                "AllIndexableCandidatesIgnored" => {
                    Some(EmptyIndexReason::AllIndexableCandidatesIgnored)
                }
                "FilteredByConfiguration" => Some(EmptyIndexReason::FilteredByConfiguration),
                _ => Some(EmptyIndexReason::UnknownPartial),
            };
        }
    }

    IndexFreshnessAssessment {
        state,
        empty_reason,
        empty_diagnostics,
        last_indexed_at: final_ts,
        days_since_indexed: days_since,
        indexed_files: active_rows,
        stale_files,
        unindexed_files,
        sample_paths,
        source: final_source,
        warnings,
    }
}

/// Warning emitted when the index has not been refreshed recently.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StalenessWarning {
    /// Approximate number of days since the last index operation completed.
    pub days_since_indexed: u64,
    /// Number of files whose content has changed since they were last indexed.
    pub stale_files: usize,
    /// Number of tracked files that have not been indexed yet.
    #[serde(default)]
    pub unindexed_files: usize,
    /// Sample paths that are stale.
    pub sample_paths: Vec<String>,
    /// Last successful index completion timestamp.
    pub last_indexed_at: Option<String>,
    /// Whether the index is completely missing (no storage found).
    #[serde(default)]
    pub is_missing: bool,
}

/// Check whether the Tantivy/CozoDB index is stale relative to the configured
/// threshold.
///
/// Returns `Some(StalenessWarning)` when `days_since_indexed > threshold_days`,
/// or when no index has ever been run. Returns `None` when the index is fresh
/// enough.
///
/// # Parameters
///
/// * `storage`  – opened `StorageManager` whose SQLite connection holds the
///   `project_files` table.
/// * `threshold_days` – number of days that may elapse before the index is
///   considered stale (e.g. 3).
pub fn check_index_staleness(
    storage: &StorageManager,
    threshold_days: u64,
) -> Option<StalenessWarning> {
    let assessment = assess_index_freshness(storage, threshold_days);
    match assessment.state {
        IndexFreshnessState::StaleEmpty | IndexFreshnessState::StalePopulated => {
            Some(StalenessWarning {
                days_since_indexed: assessment.days_since_indexed.unwrap_or(999),
                stale_files: assessment.indexed_files,
                unindexed_files: assessment.unindexed_files,
                sample_paths: assessment.sample_paths,
                last_indexed_at: assessment.last_indexed_at,
                is_missing: false,
            })
        }
        IndexFreshnessState::NeverIndexed => Some(StalenessWarning {
            days_since_indexed: 999,
            stale_files: 0,
            unindexed_files: assessment.unindexed_files,
            sample_paths: Vec::new(),
            last_indexed_at: None,
            is_missing: true,
        }),
        _ => None,
    }
}

pub fn print_staleness_warning(warning: &StalenessWarning) {
    use owo_colors::OwoColorize;

    eprintln!(
        "\n{} [STALE] Index is {} day{} old with {} indexed file{} and {} unindexed file{}.",
        "WARN".yellow().bold(),
        warning.days_since_indexed,
        if warning.days_since_indexed == 1 {
            ""
        } else {
            "s"
        },
        warning.stale_files,
        if warning.stale_files == 1 { "" } else { "s" },
        warning.unindexed_files,
        if warning.unindexed_files == 1 {
            ""
        } else {
            "s"
        },
    );

    if !warning.sample_paths.is_empty() {
        eprintln!(
            "  Sample paths: {}",
            warning.sample_paths.join(", ").dimmed()
        );
    }

    eprintln!(
        "  {} Results may be degraded. Run {} to refresh.",
        "➜".blue(),
        "ledgerful index --incremental".cyan().bold()
    );
}

/// Check whether the LEDGERFUL_NON_INTERACTIVE env var is set.
/// When non-empty, interactive prompts (e.g. inquire confirmations) should be skipped.
pub fn is_non_interactive() -> bool {
    std::env::var("LEDGERFUL_NON_INTERACTIVE")
        .ok()
        .is_some_and(|v| !v.is_empty())
}

/// Run `check_index_staleness` and print the warning banner when stale.
/// Returns `true` if a warning was printed.
pub fn warn_if_stale(storage: &StorageManager, threshold_days: u64) -> bool {
    if let Some(warning) = check_index_staleness(storage, threshold_days) {
        print_staleness_warning(&warning);
        true
    } else {
        false
    }
}

/// Run an incremental index if the current index is stale.
/// Returns the (possibly re-opened) StorageManager.
pub fn try_auto_index(storage: StorageManager, threshold_days: u64) -> Result<StorageManager> {
    let assessment = assess_index_freshness(&storage, threshold_days);
    match assessment.state {
        IndexFreshnessState::Indeterminate => {
            miette::bail!(
                "Error: Index state is indeterminate (metadata corruption or mismatch). Run 'ledgerful index --repair-metadata' to repair."
            );
        }
        IndexFreshnessState::FreshEmpty | IndexFreshnessState::StaleEmpty => {
            if matches!(
                assessment.empty_reason,
                Some(EmptyIndexReason::NoSupportedFiles)
                    | Some(EmptyIndexReason::AllIndexableCandidatesIgnored)
            ) {
                eprintln!("Index is up to date (0 indexable files).");
                return Ok(storage);
            }
            if matches!(
                assessment.empty_reason,
                Some(EmptyIndexReason::RepositoryEmpty)
            ) {
                miette::bail!(
                    "Error: Index is missing or empty. Run 'ledgerful index' to build it."
                );
            }
        }
        _ => {}
    }

    if let Some(warning) = check_index_staleness(&storage, threshold_days) {
        use crate::index::ProjectIndexer;
        use owo_colors::OwoColorize;

        eprintln!(
            "{} Index is stale ({} days old). Running auto-index...",
            "INFO".blue().bold(),
            warning.days_since_indexed
        );

        let root = storage.root().to_path_buf();

        // StorageManager::init handles write-mode and migrations
        let write_storage = StorageManager::init(
            Layout::new(&root)
                .state_subdir()
                .join("ledger.db")
                .as_std_path(),
        )?;

        use crate::config::model::Config;
        let mut indexer = ProjectIndexer::new(write_storage, root.clone(), Config::default());
        indexer.incremental_index()?;

        // Re-open in read-only mode for the caller
        StorageManager::open_read_only(&root)
    } else {
        Ok(storage)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::migrations::get_migrations;
    use rusqlite::Connection;

    fn in_memory_storage() -> StorageManager {
        let conn = Connection::open_in_memory().unwrap();
        let mut conn = conn;
        get_migrations().to_latest(&mut conn).unwrap();
        StorageManager::init_from_conn(conn)
    }

    #[test]
    fn staleness_check_fresh() {
        let storage = in_memory_storage();
        let now = Utc::now().to_rfc3339();
        let conn = storage.get_connection();

        conn.execute(
            "INSERT INTO project_files (file_path, parse_status, last_indexed_at) \
             VALUES (?1, ?2, ?3)",
            rusqlite::params!["src/main.rs", "OK", &now],
        )
        .unwrap();

        let result = check_index_staleness(&storage, 3);
        assert!(result.is_none(), "fresh index should not be stale");
    }

    #[test]
    fn staleness_check_stale() {
        let storage = in_memory_storage();
        let old_date = (Utc::now() - chrono::Duration::days(10)).to_rfc3339();
        let conn = storage.get_connection();

        conn.execute(
            "INSERT INTO project_files (file_path, parse_status, last_indexed_at) \
             VALUES (?1, ?2, ?3)",
            rusqlite::params!["src/main.rs", "OK", &old_date],
        )
        .unwrap();

        let result = check_index_staleness(&storage, 3);
        assert!(result.is_some(), "stale index should return warning");
        if let Some(warning) = result {
            assert!(
                warning.days_since_indexed >= 10,
                "days_since_indexed should be >= 10, got {}",
                warning.days_since_indexed
            );
            assert!(
                warning.stale_files >= 1,
                "should have at least 1 stale file"
            );
        }
    }

    #[test]
    fn staleness_check_threshold_respected() {
        let storage = in_memory_storage();
        let old_date = (Utc::now() - chrono::Duration::days(2)).to_rfc3339();
        let conn = storage.get_connection();

        conn.execute(
            "INSERT INTO project_files (file_path, parse_status, last_indexed_at) \
             VALUES (?1, ?2, ?3)",
            rusqlite::params!["src/main.rs", "OK", &old_date],
        )
        .unwrap();

        let result = check_index_staleness(&storage, 1);
        assert!(
            result.is_some(),
            "should be stale with threshold=1 day and age=2 days"
        );
    }

    #[test]
    fn staleness_check_empty_db() {
        let storage = in_memory_storage();
        // No project_files rows at all.
        let result = check_index_staleness(&storage, 3);
        assert!(
            result.is_some(),
            "empty DB should warn as stale to trigger initial index"
        );
        assert_eq!(result.unwrap().days_since_indexed, 999);
    }

    #[test]
    fn staleness_check_clock_skew() {
        let storage = in_memory_storage();
        // future timestamp => clock skew, should not warn
        let future = (Utc::now() + chrono::Duration::days(1)).to_rfc3339();
        let conn = storage.get_connection();
        conn.execute(
            "INSERT INTO project_files (file_path, parse_status, last_indexed_at) \
             VALUES (?1, ?2, ?3)",
            rusqlite::params!["src/main.rs", "OK", &future],
        )
        .unwrap();

        let result = check_index_staleness(&storage, 3);
        assert!(result.is_none(), "clock skew should not trigger staleness");
    }

    #[test]
    fn warn_if_stale_prints_when_stale() {
        let storage = in_memory_storage();
        let old_date = (Utc::now() - chrono::Duration::days(10)).to_rfc3339();
        let conn = storage.get_connection();
        conn.execute(
            "INSERT INTO project_files (file_path, parse_status, last_indexed_at) \
             VALUES (?1, ?2, ?3)",
            rusqlite::params!["src/main.rs", "OK", &old_date],
        )
        .unwrap();

        // Capture stderr
        let result = warn_if_stale(&storage, 3);
        assert!(result, "warn_if_stale should return true when stale");
    }
}
