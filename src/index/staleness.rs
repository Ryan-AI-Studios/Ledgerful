use crate::state::layout::Layout;
use crate::state::storage::StorageManager;
use camino::Utf8Path;
use chrono::Utc;
use miette::{IntoDiagnostic, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum IndexFreshnessState {
    NeverIndexed,
    FreshEmpty,
    FreshPopulated,
    /// Age-fresh metadata but worktree content-hash drift (check-path only).
    /// Set by [`apply_content_drift_override`]; age-only assess never emits this.
    ContentStalePopulated,
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

/// Worktree content-hash drift vs stored `project_files.content_hash`.
///
/// Same comparison as `lifecycle::check_status`: a file is drifted when the
/// stored hash is missing or does not match the current worktree bytes.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContentHashDrift {
    /// Files whose stored hash differs from the worktree (includes never-stored).
    pub changed_or_unindexed: usize,
    /// Subset of the above that had no stored row / hash.
    pub unindexed: usize,
    /// Deterministic sample paths (sorted, capped).
    pub sample_paths: Vec<String>,
}

impl ContentHashDrift {
    pub fn is_dirty(&self) -> bool {
        self.changed_or_unindexed > 0
    }
}

/// Action selected by light on-demand `--auto-index`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AutoIndexAction {
    /// Index is time-fresh and content-hash clean.
    None,
    /// NeverIndexed / missing usable floor → full first build.
    FullBootstrap,
    /// Populated (or recoverable empty) time/drift stale → incremental only.
    Incremental { time_stale: bool, drift_stale: bool },
}

/// Epoch written by [`mark_index_stale`] so age-based staleness is immediate.
pub const STALE_EPOCH_RFC3339: &str = "1970-01-01T00:00:00Z";

/// Max sample paths retained on drift / staleness warnings.
const DRIFT_SAMPLE_LIMIT: usize = 5;

/// Whether a stored `project_files` path is a code source subject to discovery.
/// Enrichment rows (README, CI YAML, `.env.example`, …) are excluded so delete
/// drift does not false-positive against non-code index material.
fn is_supported_code_path(relative_path: &str) -> bool {
    use crate::index::orchestrator::{BINARY_EXTENSIONS, SUPPORTED_EXTENSIONS};
    let ext = std::path::Path::new(relative_path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");
    SUPPORTED_EXTENSIONS.contains(&ext) && !BINARY_EXTENSIONS.contains(&ext)
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

    // Age-only assess does not walk the worktree for content drift (cheap path).
    // Real stale_files come from `count_content_hash_drift` via check_status
    // (`apply_content_drift_override`) or try_auto_index.
    let stale_files = 0;

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
            } else if active_rows == 0 {
                state = IndexFreshnessState::StaleEmpty;
            } else {
                state = IndexFreshnessState::StalePopulated;
            }
        }
    } else if final_ts.is_none() && active_rows > 0 {
        // Legacy with active rows but NO timestamp?
        state = IndexFreshnessState::NeverIndexed; // Actually shouldn't happen
    }

    // Age-only assess stays cheap: worktree unindexed count lives in
    // content-hash drift (`try_auto_index` / `count_content_hash_drift`).
    let unindexed_files = 0;

    let mut sample_paths = Vec::new();
    if state == IndexFreshnessState::StalePopulated
        || state == IndexFreshnessState::FreshPopulated
        || state == IndexFreshnessState::ContentStalePopulated
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

/// Compare worktree supported-source hashes against `project_files.content_hash`.
///
/// Walks the same supported-extension set as the indexer. Relative paths are
/// normalized to forward slashes so Windows worktrees match stored rows.
pub fn count_content_hash_drift(
    storage: &StorageManager,
    repo_root: &Utf8Path,
) -> Result<ContentHashDrift> {
    use crate::index::orchestrator::{BINARY_EXTENSIONS, SUPPORTED_EXTENSIONS};
    use crate::index::walker::RepoWalker;
    use std::collections::HashMap;

    let discovered = RepoWalker::new(
        repo_root.to_path_buf(),
        SUPPORTED_EXTENSIONS,
        BINARY_EXTENSIONS,
    )
    .discover_files()?;

    let conn = storage.get_connection();
    let mut stmt = conn
        .prepare(
            "SELECT file_path, content_hash FROM project_files WHERE parse_status != 'DELETED'",
        )
        .into_diagnostic()?;
    let mut stored: HashMap<String, Option<String>> = HashMap::new();
    let rows = stmt
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
        })
        .into_diagnostic()?;
    for row in rows {
        let (path, hash) = row.into_diagnostic()?;
        stored.insert(path.replace('\\', "/"), hash);
    }

    let mut changed_or_unindexed = 0usize;
    let mut unindexed = 0usize;
    let mut sample_paths = Vec::new();
    let mut seen_stored: std::collections::HashSet<String> = std::collections::HashSet::new();

    for file_path in &discovered {
        let relative = file_path
            .strip_prefix(repo_root)
            .unwrap_or(file_path)
            .as_str()
            .replace('\\', "/");
        seen_stored.insert(relative.clone());

        let current_hash =
            match crate::util::fs::read_to_string_with_encoding(file_path.as_std_path()) {
                Ok(c) => blake3::hash(c.as_bytes()).to_hex().to_string(),
                Err(_) => {
                    // Unreadable worktree path: treat as drift so refresh can retry.
                    changed_or_unindexed += 1;
                    if sample_paths.len() < DRIFT_SAMPLE_LIMIT {
                        sample_paths.push(relative);
                    }
                    continue;
                }
            };

        match stored.get(&relative) {
            Some(Some(stored_hash)) if stored_hash == &current_hash => {}
            Some(Some(_)) => {
                changed_or_unindexed += 1;
                if sample_paths.len() < DRIFT_SAMPLE_LIMIT {
                    sample_paths.push(relative);
                }
            }
            Some(None) | None => {
                changed_or_unindexed += 1;
                unindexed += 1;
                if sample_paths.len() < DRIFT_SAMPLE_LIMIT {
                    sample_paths.push(relative);
                }
            }
        }
    }

    // Indexed **code** files removed from the worktree are also drift (DoD-4).
    // Ignore enrichment-only rows (docs, CI, .env.example, …) which are not part of
    // supported-extension discovery and must not trigger false deletes.
    let mut deleted: Vec<String> = stored
        .keys()
        .filter(|path| !seen_stored.contains(*path) && is_supported_code_path(path))
        .cloned()
        .collect();
    deleted.sort();
    for path in deleted {
        changed_or_unindexed += 1;
        if sample_paths.len() < DRIFT_SAMPLE_LIMIT {
            sample_paths.push(path);
        }
    }

    sample_paths.sort();
    Ok(ContentHashDrift {
        changed_or_unindexed,
        unindexed,
        sample_paths,
    })
}

/// Apply content-hash drift onto an age-only assessment (check-path honesty).
///
/// Used by `lifecycle::check_status` after one `count_content_hash_drift` walk.
/// Does **not** reclassify `NeverIndexed` / `Indeterminate` merely because rows
/// or drift exist. Never leaves `FreshPopulated` when `stale_files > 0`.
pub fn apply_content_drift_override(
    mut assessment: IndexFreshnessAssessment,
    drift: &ContentHashDrift,
) -> IndexFreshnessAssessment {
    assessment.stale_files = drift.changed_or_unindexed;
    assessment.unindexed_files = drift.unindexed;
    if !drift.sample_paths.is_empty() {
        // Drift samples are already sorted + capped by count_content_hash_drift.
        assessment.sample_paths = drift.sample_paths.clone();
    }

    if drift.is_dirty() {
        // FreshEmpty + dirty: keep empty semantics honest (no invented state).
        // NeverIndexed / Indeterminate: do not reclassify merely because
        // drift or rows exist (preserve legacy branch).
        if assessment.state == IndexFreshnessState::FreshPopulated {
            assessment.state = IndexFreshnessState::ContentStalePopulated;
        }
    }

    // Hard ban: never report FreshPopulated with positive content-stale count.
    if assessment.stale_files > 0 && assessment.state == IndexFreshnessState::FreshPopulated {
        assessment.state = IndexFreshnessState::ContentStalePopulated;
    }

    assessment
}

/// Pure decision for light on-demand auto-index (testable without running indexers).
///
/// Callers must still respect empty-reason early exits (`NoSupportedFiles`, etc.)
/// before invoking this planner for empty assessments.
pub fn plan_auto_index_action(
    assessment: &IndexFreshnessAssessment,
    drift: &ContentHashDrift,
) -> AutoIndexAction {
    match assessment.state {
        IndexFreshnessState::Indeterminate => AutoIndexAction::None,
        IndexFreshnessState::NeverIndexed => AutoIndexAction::FullBootstrap,
        IndexFreshnessState::FreshEmpty
        | IndexFreshnessState::StaleEmpty
        | IndexFreshnessState::FreshPopulated
        | IndexFreshnessState::ContentStalePopulated
        | IndexFreshnessState::StalePopulated => {
            // ContentStalePopulated is always content-stale (not age-stale).
            let time_stale = matches!(
                assessment.state,
                IndexFreshnessState::StaleEmpty | IndexFreshnessState::StalePopulated
            );
            // When state is already ContentStalePopulated, treat as drift_stale
            // even if the pure drift param is empty (override path may plan later).
            let drift_stale =
                drift.is_dirty() || assessment.state == IndexFreshnessState::ContentStalePopulated;
            if time_stale || drift_stale {
                AutoIndexAction::Incremental {
                    time_stale,
                    drift_stale,
                }
            } else {
                AutoIndexAction::None
            }
        }
    }
}

/// Force time-based staleness so the next auto-index / `index --check` sees STALE.
///
/// Used by watch mega-batch safety: refuse unbounded incremental, mark honest
/// staleness, tell the user to run `ledgerful index --full`.
pub fn mark_index_stale(storage: &mut StorageManager) -> Result<()> {
    let conn = storage.get_connection_mut();
    conn.execute(
        "INSERT OR REPLACE INTO index_metadata (key, value) VALUES ('last_indexed_at', ?1)",
        [STALE_EPOCH_RFC3339],
    )
    .into_diagnostic()?;
    Ok(())
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
    /// True when content-hash drift (not only age) contributed to staleness.
    #[serde(default)]
    pub is_content_drift: bool,
}

/// Check whether the Tantivy/CozoDB index is stale relative to the configured
/// **time** threshold.
///
/// Returns `Some(StalenessWarning)` when `days_since_indexed > threshold_days`,
/// or when no index has ever been run. Returns `None` when the index is
/// age-fresh. Content-hash drift is evaluated separately by
/// [`count_content_hash_drift`] / [`try_auto_index`].
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
                is_content_drift: false,
            })
        }
        IndexFreshnessState::NeverIndexed => Some(StalenessWarning {
            days_since_indexed: 999,
            stale_files: 0,
            unindexed_files: assessment.unindexed_files,
            sample_paths: Vec::new(),
            last_indexed_at: None,
            is_missing: true,
            is_content_drift: false,
        }),
        _ => None,
    }
}

pub fn print_staleness_warning(warning: &StalenessWarning) {
    use owo_colors::{OwoColorize, Stream, Style};

    if warning.is_content_drift && warning.days_since_indexed <= 3 {
        eprintln!(
            "\n{} [STALE] Index content drift: {} changed/unindexed file{} (index age {} day{}).",
            "WARN".if_supports_color(Stream::Stderr, |s| s.style(Style::new().yellow().bold())),
            warning.stale_files,
            if warning.stale_files == 1 { "" } else { "s" },
            warning.days_since_indexed,
            if warning.days_since_indexed == 1 {
                ""
            } else {
                "s"
            },
        );
    } else {
        eprintln!(
            "\n{} [STALE] Index is {} day{} old with {} indexed file{} and {} unindexed file{}.",
            "WARN".if_supports_color(Stream::Stderr, |s| s.style(Style::new().yellow().bold())),
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
    }

    if !warning.sample_paths.is_empty() {
        eprintln!(
            "  Sample paths: {}",
            warning
                .sample_paths
                .join(", ")
                .if_supports_color(Stream::Stderr, |s| s.dimmed())
        );
    }

    eprintln!(
        "  {} Results may be degraded. Run {} to refresh (or pass --auto-index on supported commands).",
        "➜".if_supports_color(Stream::Stderr, |s| s.blue()),
        "ledgerful index --incremental"
            .if_supports_color(Stream::Stderr, |s| s.style(Style::new().cyan().bold()))
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

/// Run a light index refresh if the current index is time-stale, never-indexed,
/// or content-hash drift-stale.
///
/// - **NeverIndexed / missing floor** → `full_index()` (bootstrap carve-out).
/// - **Populated time/drift stale** → `incremental_index()` only.
/// - Never SCIP, never `--analyze-graph`.
///
/// Returns the (possibly re-opened) [`StorageManager`] and the [`AutoIndexAction`]
/// that ran (or `None` on no-op early exits). Callers that open Tantivy (search)
/// must full-rebuild FTS when the action is `FullBootstrap` or `Incremental`.
///
/// `layout` must be the resolved work_root + state_dir (from
/// [`crate::commands::helpers::get_layout`]). Do **not** rebuild layout from
/// `storage.root()`: that path is the parent of `.ledgerful` inferred from the
/// DB path and invents a private state tree under a linked worktree.
pub fn try_auto_index(
    storage: StorageManager,
    threshold_days: u64,
    layout: &Layout,
) -> Result<(StorageManager, AutoIndexAction)> {
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
                return Ok((storage, AutoIndexAction::None));
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

    // Content-hash drift is load-bearing for same-day agent edits (time-fresh
    // alone would no-op). Skip the walk only for pure NeverIndexed bootstrap —
    // empty DB has nothing useful stored, and full_index rebuilds everything.
    let drift = if assessment.state == IndexFreshnessState::NeverIndexed {
        ContentHashDrift::default()
    } else {
        count_content_hash_drift(&storage, &layout.root)?
    };

    let action = plan_auto_index_action(&assessment, &drift);
    if matches!(action, AutoIndexAction::None) {
        return Ok((storage, AutoIndexAction::None));
    }

    use crate::config::model::Config;
    use crate::index::ProjectIndexer;
    use owo_colors::{OwoColorize, Stream, Style};

    match &action {
        AutoIndexAction::FullBootstrap => {
            eprintln!(
                "{} Index missing or never built. Running full bootstrap index...",
                "INFO".if_supports_color(Stream::Stderr, |s| s.style(Style::new().blue().bold()))
            );
        }
        AutoIndexAction::Incremental {
            time_stale,
            drift_stale,
        } => {
            if *time_stale && *drift_stale {
                eprintln!(
                    "{} Index is time-stale ({} days) and has content drift ({} file{}). Running auto-index...",
                    "INFO"
                        .if_supports_color(Stream::Stderr, |s| s.style(Style::new().blue().bold())),
                    assessment.days_since_indexed.unwrap_or(999),
                    drift.changed_or_unindexed,
                    if drift.changed_or_unindexed == 1 {
                        ""
                    } else {
                        "s"
                    },
                );
            } else if *time_stale {
                eprintln!(
                    "{} Index is stale ({} days old). Running auto-index...",
                    "INFO"
                        .if_supports_color(Stream::Stderr, |s| s.style(Style::new().blue().bold())),
                    assessment.days_since_indexed.unwrap_or(999)
                );
            } else {
                eprintln!(
                    "{} Index content drift detected ({} changed/unindexed file{}). Running auto-index...",
                    "INFO"
                        .if_supports_color(Stream::Stderr, |s| s.style(Style::new().blue().bold())),
                    drift.changed_or_unindexed,
                    if drift.changed_or_unindexed == 1 {
                        ""
                    } else {
                        "s"
                    },
                );
            }
        }
        AutoIndexAction::None => {}
    }

    // Open write DB under resolved state_dir (shared on linked worktrees).
    let write_storage = StorageManager::init_with_layout(layout)?;

    // Prefer repo config (ignore patterns, thresholds) over bare defaults.
    let index_config =
        crate::config::load::load_config(layout).unwrap_or_else(|_| Config::default());

    // Index analysis root is the current worktree workdir, not state parent.
    let mut indexer = ProjectIndexer::new(write_storage, layout.root.clone(), index_config);
    match &action {
        AutoIndexAction::FullBootstrap => {
            indexer.full_index()?;
        }
        AutoIndexAction::Incremental { .. } => {
            indexer.incremental_index()?;
        }
        AutoIndexAction::None => {
            // Handled above; defensive no-op keeps match exhaustive.
        }
    }

    // Re-open in read-only mode for the caller using the same layout.
    Ok((StorageManager::open_read_only(layout)?, action))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::migrations::get_migrations;
    use camino::Utf8PathBuf;
    use rusqlite::Connection;
    use std::fs;
    use std::io::Write;

    fn in_memory_storage() -> StorageManager {
        let conn = Connection::open_in_memory().unwrap();
        let mut conn = conn;
        get_migrations().to_latest(&mut conn).unwrap();
        StorageManager::init_from_conn(conn)
    }

    fn write_utf8(path: &std::path::Path, content: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        let mut f = fs::File::create(path).unwrap();
        f.write_all(content.as_bytes()).unwrap();
    }

    fn insert_file_with_hash(
        storage: &StorageManager,
        path: &str,
        content: &str,
        indexed_at: &str,
    ) {
        let hash = blake3::hash(content.as_bytes()).to_hex().to_string();
        let conn = storage.get_connection();
        conn.execute(
            "INSERT INTO project_files (file_path, parse_status, last_indexed_at, content_hash) \
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![path, "OK", indexed_at, hash],
        )
        .unwrap();
    }

    fn set_last_indexed_at(storage: &StorageManager, ts: &str) {
        let conn = storage.get_connection();
        conn.execute(
            "INSERT OR REPLACE INTO index_metadata (key, value) VALUES ('last_indexed_at', ?1)",
            [ts],
        )
        .unwrap();
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
        set_last_indexed_at(&storage, &now);

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
        set_last_indexed_at(&storage, &old_date);

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
        set_last_indexed_at(&storage, &old_date);

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
        set_last_indexed_at(&storage, &future);

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
        set_last_indexed_at(&storage, &old_date);

        // Capture stderr
        let result = warn_if_stale(&storage, 3);
        assert!(result, "warn_if_stale should return true when stale");
    }

    #[test]
    fn plan_never_indexed_is_full_bootstrap() {
        let assessment = IndexFreshnessAssessment {
            state: IndexFreshnessState::NeverIndexed,
            empty_reason: None,
            empty_diagnostics: None,
            last_indexed_at: None,
            days_since_indexed: None,
            indexed_files: 0,
            stale_files: 0,
            unindexed_files: 0,
            sample_paths: Vec::new(),
            source: FreshnessSource::None,
            warnings: Vec::new(),
        };
        assert_eq!(
            plan_auto_index_action(&assessment, &ContentHashDrift::default()),
            AutoIndexAction::FullBootstrap
        );
    }

    #[test]
    fn plan_age_fresh_clean_is_none() {
        let assessment = IndexFreshnessAssessment {
            state: IndexFreshnessState::FreshPopulated,
            empty_reason: None,
            empty_diagnostics: None,
            last_indexed_at: Some(Utc::now().to_rfc3339()),
            days_since_indexed: Some(0),
            indexed_files: 1,
            stale_files: 0,
            unindexed_files: 0,
            sample_paths: Vec::new(),
            source: FreshnessSource::RepositoryMetadata,
            warnings: Vec::new(),
        };
        assert_eq!(
            plan_auto_index_action(&assessment, &ContentHashDrift::default()),
            AutoIndexAction::None
        );
    }

    #[test]
    fn plan_age_fresh_dirty_is_incremental_drift() {
        let assessment = IndexFreshnessAssessment {
            state: IndexFreshnessState::FreshPopulated,
            empty_reason: None,
            empty_diagnostics: None,
            last_indexed_at: Some(Utc::now().to_rfc3339()),
            days_since_indexed: Some(0),
            indexed_files: 1,
            stale_files: 0,
            unindexed_files: 0,
            sample_paths: Vec::new(),
            source: FreshnessSource::RepositoryMetadata,
            warnings: Vec::new(),
        };
        let drift = ContentHashDrift {
            changed_or_unindexed: 2,
            unindexed: 0,
            sample_paths: vec!["src/a.rs".into()],
        };
        assert_eq!(
            plan_auto_index_action(&assessment, &drift),
            AutoIndexAction::Incremental {
                time_stale: false,
                drift_stale: true,
            }
        );
    }

    #[test]
    fn plan_age_stale_is_incremental_time() {
        let assessment = IndexFreshnessAssessment {
            state: IndexFreshnessState::StalePopulated,
            empty_reason: None,
            empty_diagnostics: None,
            last_indexed_at: Some((Utc::now() - chrono::Duration::days(10)).to_rfc3339()),
            days_since_indexed: Some(10),
            indexed_files: 1,
            stale_files: 1,
            unindexed_files: 0,
            sample_paths: Vec::new(),
            source: FreshnessSource::RepositoryMetadata,
            warnings: Vec::new(),
        };
        assert_eq!(
            plan_auto_index_action(&assessment, &ContentHashDrift::default()),
            AutoIndexAction::Incremental {
                time_stale: true,
                drift_stale: false,
            }
        );
    }

    #[test]
    fn content_hash_drift_detects_dirty_and_clean() {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        let src = root.join("src");
        write_utf8(src.join("lib.rs").as_std_path(), "fn a() {}\n");

        let storage = in_memory_storage();
        let now = Utc::now().to_rfc3339();
        insert_file_with_hash(&storage, "src/lib.rs", "fn a() {}\n", &now);
        set_last_indexed_at(&storage, &now);

        let clean = count_content_hash_drift(&storage, &root).unwrap();
        assert!(
            !clean.is_dirty(),
            "matching hashes should be clean, got {:?}",
            clean
        );

        write_utf8(src.join("lib.rs").as_std_path(), "fn a() { /* dirty */ }\n");
        let dirty = count_content_hash_drift(&storage, &root).unwrap();
        assert!(dirty.is_dirty(), "edited file should drift");
        assert_eq!(dirty.changed_or_unindexed, 1);
        assert!(dirty.sample_paths.iter().any(|p| p == "src/lib.rs"));
    }

    #[test]
    fn content_hash_drift_detects_unindexed_new_file() {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        write_utf8(root.join("src/lib.rs").as_std_path(), "fn a() {}\n");
        write_utf8(root.join("src/new.rs").as_std_path(), "fn b() {}\n");

        let storage = in_memory_storage();
        let now = Utc::now().to_rfc3339();
        insert_file_with_hash(&storage, "src/lib.rs", "fn a() {}\n", &now);
        set_last_indexed_at(&storage, &now);

        let drift = count_content_hash_drift(&storage, &root).unwrap();
        assert!(drift.is_dirty());
        assert_eq!(drift.unindexed, 1);
        assert!(drift.sample_paths.iter().any(|p| p == "src/new.rs"));
    }

    #[test]
    fn content_hash_drift_detects_deleted_worktree_file() {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        write_utf8(root.join("src/lib.rs").as_std_path(), "fn a() {}\n");
        // Indexed path gone from the worktree (same-day delete).
        let storage = in_memory_storage();
        let now = Utc::now().to_rfc3339();
        insert_file_with_hash(&storage, "src/lib.rs", "fn a() {}\n", &now);
        insert_file_with_hash(&storage, "src/gone.rs", "fn gone() {}\n", &now);
        set_last_indexed_at(&storage, &now);

        let drift = count_content_hash_drift(&storage, &root).unwrap();
        assert!(drift.is_dirty(), "deleted indexed file must count as drift");
        assert!(
            drift.changed_or_unindexed >= 1,
            "expected at least the deleted path"
        );
        assert!(drift.sample_paths.iter().any(|p| p == "src/gone.rs"));
        assert_eq!(
            plan_auto_index_action(&assess_index_freshness(&storage, 3), &drift),
            AutoIndexAction::Incremental {
                time_stale: false,
                drift_stale: true,
            }
        );
    }

    #[test]
    fn content_hash_drift_ignores_enrichment_only_rows() {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        write_utf8(root.join("src/lib.rs").as_std_path(), "fn a() {}\n");

        let storage = in_memory_storage();
        let now = Utc::now().to_rfc3339();
        insert_file_with_hash(&storage, "src/lib.rs", "fn a() {}\n", &now);
        // Enrichment material not in SUPPORTED_EXTENSIONS discovery.
        insert_file_with_hash(&storage, "README.md", "# readme\n", &now);
        insert_file_with_hash(&storage, ".env.example", "FOO=1\n", &now);
        insert_file_with_hash(&storage, ".github/workflows/ci.yml", "name: ci\n", &now);
        set_last_indexed_at(&storage, &now);

        let drift = count_content_hash_drift(&storage, &root).unwrap();
        assert!(
            !drift.is_dirty(),
            "enrichment-only rows must not false-positive delete drift: {:?}",
            drift
        );
    }

    #[test]
    fn age_fresh_plus_dirty_plans_auto_index() {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        write_utf8(root.join("src/lib.rs").as_std_path(), "fn dirty() {}\n");

        let storage = in_memory_storage();
        let now = Utc::now().to_rfc3339();
        // Stored hash is for different content → drift.
        insert_file_with_hash(&storage, "src/lib.rs", "fn clean() {}\n", &now);
        set_last_indexed_at(&storage, &now);

        let assessment = assess_index_freshness(&storage, 3);
        assert_eq!(assessment.state, IndexFreshnessState::FreshPopulated);
        assert!(check_index_staleness(&storage, 3).is_none());

        let drift = count_content_hash_drift(&storage, &root).unwrap();
        assert!(drift.is_dirty());
        assert_eq!(
            plan_auto_index_action(&assessment, &drift),
            AutoIndexAction::Incremental {
                time_stale: false,
                drift_stale: true,
            }
        );
    }

    #[test]
    fn mark_index_stale_forces_time_stale() {
        let mut storage = in_memory_storage();
        let now = Utc::now().to_rfc3339();
        let conn = storage.get_connection();
        conn.execute(
            "INSERT INTO project_files (file_path, parse_status, last_indexed_at) \
             VALUES (?1, ?2, ?3)",
            rusqlite::params!["src/main.rs", "OK", &now],
        )
        .unwrap();
        set_last_indexed_at(&storage, &now);
        assert!(check_index_staleness(&storage, 3).is_none());

        mark_index_stale(&mut storage).unwrap();
        let warning = check_index_staleness(&storage, 3);
        assert!(warning.is_some(), "marked stale should be age-stale");
        let assessment = assess_index_freshness(&storage, 3);
        assert_eq!(assessment.state, IndexFreshnessState::StalePopulated);
        assert_eq!(
            assessment.last_indexed_at.as_deref(),
            Some(STALE_EPOCH_RFC3339)
        );
    }

    // ── 0128: check honesty + ContentStalePopulated ───────────────────────

    #[test]
    fn age_only_assess_stale_files_is_zero_not_row_count() {
        let storage = in_memory_storage();
        let now = Utc::now().to_rfc3339();
        insert_file_with_hash(&storage, "src/a.rs", "fn a() {}\n", &now);
        insert_file_with_hash(&storage, "src/b.rs", "fn b() {}\n", &now);
        set_last_indexed_at(&storage, &now);

        let assessment = assess_index_freshness(&storage, 3);
        assert_eq!(assessment.state, IndexFreshnessState::FreshPopulated);
        assert_eq!(assessment.indexed_files, 2);
        assert_eq!(
            assessment.stale_files, 0,
            "age-only assess must not copy active_rows into stale_files"
        );
        assert_eq!(assessment.unindexed_files, 0);
    }

    #[test]
    fn apply_drift_override_age_fresh_dirty_is_content_stale() {
        let assessment = IndexFreshnessAssessment {
            state: IndexFreshnessState::FreshPopulated,
            empty_reason: None,
            empty_diagnostics: None,
            last_indexed_at: Some(Utc::now().to_rfc3339()),
            days_since_indexed: Some(0),
            indexed_files: 10,
            stale_files: 0,
            unindexed_files: 0,
            sample_paths: Vec::new(),
            source: FreshnessSource::RepositoryMetadata,
            warnings: Vec::new(),
        };
        let drift = ContentHashDrift {
            changed_or_unindexed: 7,
            unindexed: 2,
            sample_paths: vec!["src/a.rs".into(), "src/b.rs".into()],
        };
        let out = apply_content_drift_override(assessment, &drift);
        assert_eq!(out.state, IndexFreshnessState::ContentStalePopulated);
        assert_eq!(out.stale_files, 7);
        assert_eq!(out.unindexed_files, 2);
        assert_eq!(out.indexed_files, 10);
        assert_eq!(out.sample_paths, drift.sample_paths);
        assert_ne!(
            out.state,
            IndexFreshnessState::FreshPopulated,
            "never FreshPopulated with stale_files > 0"
        );
    }

    #[test]
    fn apply_drift_override_never_reclassifies_never_indexed() {
        let assessment = IndexFreshnessAssessment {
            state: IndexFreshnessState::NeverIndexed,
            empty_reason: None,
            empty_diagnostics: None,
            last_indexed_at: None,
            days_since_indexed: None,
            indexed_files: 3,
            stale_files: 0,
            unindexed_files: 0,
            sample_paths: Vec::new(),
            source: FreshnessSource::None,
            warnings: Vec::new(),
        };
        let drift = ContentHashDrift {
            changed_or_unindexed: 3,
            unindexed: 3,
            sample_paths: vec!["src/a.rs".into()],
        };
        let out = apply_content_drift_override(assessment, &drift);
        assert_eq!(
            out.state,
            IndexFreshnessState::NeverIndexed,
            "NeverIndexed + rows/drift must not become Fresh*"
        );
        assert_eq!(out.stale_files, 3);
    }

    #[test]
    fn apply_drift_override_fresh_empty_keeps_empty_semantics() {
        let assessment = IndexFreshnessAssessment {
            state: IndexFreshnessState::FreshEmpty,
            empty_reason: Some(EmptyIndexReason::NoSupportedFiles),
            empty_diagnostics: None,
            last_indexed_at: Some(Utc::now().to_rfc3339()),
            days_since_indexed: Some(0),
            indexed_files: 0,
            stale_files: 0,
            unindexed_files: 0,
            sample_paths: Vec::new(),
            source: FreshnessSource::RepositoryMetadata,
            warnings: Vec::new(),
        };
        let drift = ContentHashDrift {
            changed_or_unindexed: 1,
            unindexed: 1,
            sample_paths: vec!["src/a.rs".into()],
        };
        let out = apply_content_drift_override(assessment, &drift);
        assert_eq!(
            out.state,
            IndexFreshnessState::FreshEmpty,
            "FreshEmpty + dirty must not invent ContentStale without populated floor"
        );
        assert_eq!(out.stale_files, 1);
    }

    #[test]
    fn plan_content_stale_populated_is_incremental_drift() {
        let assessment = IndexFreshnessAssessment {
            state: IndexFreshnessState::ContentStalePopulated,
            empty_reason: None,
            empty_diagnostics: None,
            last_indexed_at: Some(Utc::now().to_rfc3339()),
            days_since_indexed: Some(0),
            indexed_files: 5,
            stale_files: 2,
            unindexed_files: 0,
            sample_paths: Vec::new(),
            source: FreshnessSource::RepositoryMetadata,
            warnings: Vec::new(),
        };
        // Even with empty drift param, ContentStale implies drift_stale.
        assert_eq!(
            plan_auto_index_action(&assessment, &ContentHashDrift::default()),
            AutoIndexAction::Incremental {
                time_stale: false,
                drift_stale: true,
            }
        );
        let drift = ContentHashDrift {
            changed_or_unindexed: 2,
            unindexed: 0,
            sample_paths: vec!["src/x.rs".into()],
        };
        assert_eq!(
            plan_auto_index_action(&assessment, &drift),
            AutoIndexAction::Incremental {
                time_stale: false,
                drift_stale: true,
            }
        );
    }

    #[test]
    fn never_fresh_populated_when_override_stale_positive() {
        // Defensive: force the hard ban even if state was still FreshPopulated
        // with positive drift counts somehow.
        let assessment = IndexFreshnessAssessment {
            state: IndexFreshnessState::FreshPopulated,
            empty_reason: None,
            empty_diagnostics: None,
            last_indexed_at: Some(Utc::now().to_rfc3339()),
            days_since_indexed: Some(0),
            indexed_files: 1,
            stale_files: 0,
            unindexed_files: 0,
            sample_paths: Vec::new(),
            source: FreshnessSource::RepositoryMetadata,
            warnings: Vec::new(),
        };
        let drift = ContentHashDrift {
            changed_or_unindexed: 1,
            unindexed: 0,
            sample_paths: vec!["src/a.rs".into()],
        };
        let out = apply_content_drift_override(assessment, &drift);
        assert!(out.stale_files > 0);
        assert_ne!(out.state, IndexFreshnessState::FreshPopulated);
        assert_eq!(out.state, IndexFreshnessState::ContentStalePopulated);
    }

    /// 0128 B7: search must not full-rebuild FTS when AutoIndexAction is None
    /// (except document_count==0 / explicit --index). Source-level gate.
    /// R1: post-auto-index rebuild must run before semantic early-return.
    #[test]
    fn search_fts_rebuild_gated_on_auto_index_action() {
        let search_src = include_str!("../commands/search/mod.rs");
        assert!(
            search_src.contains("auto_index_ran_work")
                || search_src.contains("AutoIndexAction::FullBootstrap"),
            "search must key FTS rebuild on auto-index action"
        );
        assert!(
            search_src.contains("needs_fts_rebuild"),
            "search must use a single needs_fts_rebuild gate"
        );
        assert!(
            search_src.contains("fts_rebuilt_for_auto_index"),
            "search must track early post-auto-index FTS rebuild (semantic early-return safe)"
        );
        // Ensure Action::None does not alone force rebuild: condition includes
        // FullBootstrap | Incremental, not a blanket rebuild after try_auto_index.
        let try_region = search_src
            .find("try_auto_index")
            .expect("search calls try_auto_index");
        let early_rebuild = search_src
            .find("Post-auto-index full FTS rebuild")
            .expect("early post-auto-index rebuild present");
        let semantic_block = search_src
            .find("if args.semantic")
            .expect("semantic block present");
        let rebuild_region = search_src
            .find("needs_fts_rebuild")
            .expect("needs_fts_rebuild present");
        assert!(
            early_rebuild > try_region && early_rebuild < semantic_block,
            "post-auto-index FTS rebuild must run after try_auto_index and before semantic"
        );
        assert!(
            rebuild_region > try_region,
            "BM25 needs_fts_rebuild gate should follow try_auto_index capture"
        );
        assert!(
            search_src.contains("rebuild_tantivy_index"),
            "search must use shared rebuild_tantivy_index helper"
        );
    }

    /// Reachability: light continuous / on-demand paths must not invoke SCIP.
    #[test]
    fn light_path_sources_are_scip_free_by_construction() {
        let watch = include_str!("../commands/watch.rs");
        let incremental = include_str!("incremental.rs");
        let staleness = include_str!("staleness.rs");

        /// Production source only: drop `#[cfg(test)]` modules and line comments so
        /// policy prose / unit-test string literals cannot trip the gate.
        fn production_code(src: &str) -> String {
            let without_tests = src.split("#[cfg(test)]").next().unwrap_or(src);
            without_tests
                .lines()
                .filter(|l| {
                    let t = l.trim_start();
                    !t.starts_with("//") && !t.starts_with("///") && !t.starts_with("//!")
                })
                .collect::<Vec<_>>()
                .join("\n")
        }

        for (name, raw) in [
            ("watch.rs", watch),
            ("incremental.rs", incremental),
            ("staleness.rs", staleness),
        ] {
            let code = production_code(raw).to_ascii_lowercase();
            assert!(
                !code.contains("auto_scip"),
                "{name} must not reference auto_scip on the light path"
            );
            assert!(
                !code.contains("generate_scip"),
                "{name} must not call generate_scip"
            );
            assert!(
                !code.contains("scip::"),
                "{name} must not use scip:: modules"
            );
            assert!(!code.contains("run_scip"), "{name} must not call run_scip");
            assert!(
                !code.contains("--auto-scip"),
                "{name} must not pass --auto-scip"
            );
        }

        // try_auto_index body must only call full_index / incremental_index.
        let try_start = staleness
            .find("pub fn try_auto_index")
            .expect("try_auto_index present");
        let try_end = staleness[try_start..]
            .find("#[cfg(test)]")
            .map(|i| try_start + i)
            .unwrap_or(staleness.len());
        let try_code = production_code(&staleness[try_start..try_end]).to_ascii_lowercase();
        assert!(
            try_code.contains("full_index") && try_code.contains("incremental_index"),
            "try_auto_index must use full/incremental bootstrap paths"
        );
        assert!(
            !try_code.contains("analyze_graph") && !try_code.contains("analyze-graph"),
            "try_auto_index must never run analyze-graph"
        );
    }
}
