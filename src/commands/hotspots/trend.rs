use super::list::persist_hotspots_and_couplings;
use crate::commands::hook_post_commit::insert_hotspot_trends_with_retry;
use crate::impact::hotspots::{HotspotQuery, calculate_hotspots, normalize_score};
use crate::impact::temporal::GixHistoryProvider;
use crate::output::table::build_premium_table;
use crate::state::storage::StorageManager;
use crate::util::term::prompt_yes_no;
use chrono::Utc;
use miette::{IntoDiagnostic, Result};
use owo_colors::{OwoColorize, Stream, Style};

/// Insert a hotspot snapshot into `hotspot_trends` so `hotspots trend` can
/// display bootstrapped data immediately. Does not deduplicate: callers are
/// responsible for ensuring this is used only when a new snapshot is desired.
fn insert_hotspot_trends_snapshot(
    storage: &StorageManager,
    hotspots: &[crate::impact::packet::Hotspot],
    repo: &gix::Repository,
) -> Result<()> {
    let (head_hash, _branch) = crate::git::repo::get_head_info(repo)?;
    let head_hash = head_hash.unwrap_or_default();
    let timestamp = Utc::now().to_rfc3339();
    insert_hotspot_trends_with_retry(storage, hotspots, &head_hash, &timestamp)?;
    Ok(())
}

/// Collect the last `samples` commits from HEAD (first-parent only), oldest
/// first, paired with their committer timestamps. Returns a vector suitable
/// for historical hotspot bootstrapping. Errors are returned only for
/// unrecoverable git metadata failures.
fn collect_sample_commits(
    repo: &gix::Repository,
    samples: usize,
) -> Result<Vec<(gix::ObjectId, String)>> {
    let head = repo
        .head_commit()
        .map_err(|e| miette::miette!("Failed to read HEAD commit: {}", e))?;
    let walk = head
        .id()
        .ancestors()
        .first_parent_only()
        .all()
        .map_err(|e| miette::miette!("Failed to start commit walk: {}", e))?;

    let mut commits: Vec<(gix::ObjectId, u64)> = Vec::new();
    for res in walk {
        let info = res.map_err(|e| miette::miette!("Commit walk error: {}", e))?;
        let commit = info
            .id()
            .object()
            .map_err(|e| miette::miette!("Failed to read commit {}: {}", info.id(), e))?
            .into_commit();
        let time = commit
            .time()
            .map_err(|e| miette::miette!("Failed to read commit time for {}: {}", info.id(), e))?
            .seconds as u64;
        commits.push((info.id().into(), time));
        if commits.len() >= samples {
            break;
        }
    }

    // Process oldest -> newest so the trend table builds forward in time.
    commits.reverse();

    Ok(commits
        .into_iter()
        .map(|(id, time)| {
            let dt = chrono::DateTime::from_timestamp(time as i64, 0)
                .unwrap_or(chrono::DateTime::UNIX_EPOCH);
            (id, dt.to_rfc3339())
        })
        .collect())
}

pub(super) fn format_trend_ts(ts: &str) -> String {
    match chrono::DateTime::parse_from_rfc3339(ts) {
        Ok(dt) => dt
            .with_timezone(&chrono::Utc)
            .format("%Y-%m-%d %H:%M UTC")
            .to_string(),
        Err(_) => ts.to_string(),
    }
}

/// Render full/entity hotspot trend rows as a premium-styled `comfy-table`.
pub(super) fn render_hotspot_trend_table(rows: &[TrendRow]) -> String {
    let mut table = build_premium_table(["Timestamp", "File", "Score"]);
    for row in rows {
        table.add_row(vec![
            format_trend_ts(&row.recorded_at),
            row.file_path.clone(),
            format!("{:.3}", normalize_score(row.score)),
        ]);
    }
    table.to_string()
}

/// Render default summary (top-N files) as a premium-styled table.
pub(super) fn render_trend_summary_table(summary: &TrendSummary) -> String {
    use crate::output::table::{TableStyleKind, resolve_table_style};
    let style = resolve_table_style();
    let delta_hdr = if style == TableStyleKind::Ascii {
        "Delta"
    } else {
        "Δ"
    };
    let missing = if style == TableStyleKind::Ascii {
        "-"
    } else {
        "—"
    };
    let mut table = build_premium_table([
        "File",
        "Score",
        "Prior",
        delta_hdr,
        "Samples",
        "Last recorded",
    ]);
    for file in &summary.files {
        let prior = file
            .prior_display_score
            .map(|p| format!("{p:.3}"))
            .unwrap_or_else(|| missing.to_string());
        let delta = file
            .delta
            .map(|d| {
                if d > 0.0 {
                    format!("+{d:.3}")
                } else {
                    format!("{d:.3}")
                }
            })
            .unwrap_or_else(|| missing.to_string());
        table.add_row(vec![
            file.file_path.clone(),
            format!("{:.3}", file.display_score),
            prior,
            delta,
            file.sample_count.to_string(),
            format_trend_ts(&file.last_recorded_at),
        ]);
    }
    table.to_string()
}

/// The exact command an operator should run to bootstrap trend history.
const BOOTSTRAP_HINT: &str = "ledgerful hotspots trend --bootstrap";

/// Outcome of a single [`run_bootstrap_compute`] invocation. Both the explicit
/// `--bootstrap` flag path and the DX1 interactive prompt path share this
/// single implementation so the bootstrap logic is never duplicated.
struct BootstrapOutcome {
    /// True when a fresh snapshot was computed and persisted this call.
    bootstrapped: bool,
    /// True when history already existed under the write lock (no-op).
    skipped: bool,
    /// Whether temporal coupling history was actually persisted (false when the
    /// repository has fewer than 10 commits — soft degradation, not a failure).
    couplings_persisted: bool,
}

/// Compute and persist an initial hotspot snapshot under SQLite's write lock
/// (`BEGIN IMMEDIATE`), shared by the `--bootstrap` flag and the DX1
/// interactive prompt. Re-checks row count under the lock so two concurrent
/// bootstraps cannot both observe `0` and double-insert. Returns the outcome
/// without printing anything; callers own the human-readable reporting.
fn run_bootstrap_compute(
    storage: &StorageManager,
    repo: &gix::Repository,
    config: &crate::config::model::Config,
) -> Result<BootstrapOutcome> {
    let conn = storage.get_connection();
    conn.execute_batch("BEGIN IMMEDIATE").into_diagnostic()?;
    let result = (|| -> Result<BootstrapOutcome> {
        let locked_existing_rows = hotspot_history_row_count(storage)?;
        if locked_existing_rows == 0 {
            // Bounded by the same config-driven defaults used by the main
            // `hotspots --snapshot` path; do not introduce an unbounded scan.
            let history_provider = GixHistoryProvider::new(repo);
            let query = HotspotQuery {
                limit: config.hotspots.limit,
                commits: config.hotspots.max_commits,
                decay_half_life: config.hotspots.decay_half_life,
                ..Default::default()
            };
            let hotspots = calculate_hotspots(storage, &history_provider, &query)?;
            let couplings_persisted =
                persist_hotspots_and_couplings(storage, repo, &hotspots, config)?;
            // The trend view reads from `hotspot_trends` (populated by the
            // post-commit hook); make the bootstrapped snapshot visible there
            // too.
            insert_hotspot_trends_snapshot(storage, &hotspots, repo)?;
            Ok(BootstrapOutcome {
                bootstrapped: true,
                skipped: false,
                couplings_persisted,
            })
        } else {
            Ok(BootstrapOutcome {
                bootstrapped: false,
                skipped: true,
                couplings_persisted: false,
            })
        }
    })();
    match result {
        Ok(o) => {
            conn.execute_batch("COMMIT").into_diagnostic()?;
            Ok(o)
        }
        Err(e) => {
            conn.execute_batch("ROLLBACK").into_diagnostic()?;
            Err(e)
        }
    }
}

fn hotspot_history_row_count(storage: &StorageManager) -> Result<i64> {
    let conn = storage.get_connection();
    conn.query_row("SELECT COUNT(*) FROM hotspot_history", [], |row| row.get(0))
        .into_diagnostic()
}

fn hotspot_trends_row_count(storage: &StorageManager) -> Result<i64> {
    let conn = storage.get_connection();
    conn.query_row("SELECT COUNT(*) FROM hotspot_trends", [], |row| row.get(0))
        .into_diagnostic()
}

/// Clear all rows from `hotspot_trends`. Should only be called after the user
/// has confirmed they want to re-bootstrap from scratch.
fn clear_hotspot_trends(storage: &StorageManager) -> Result<()> {
    let conn = storage.get_connection();
    conn.execute("DELETE FROM hotspot_trends", [])
        .into_diagnostic()?;
    Ok(())
}

fn query_trend_rows(
    storage: &StorageManager,
    entity: &Option<String>,
    days: u32,
) -> Result<Vec<TrendRow>> {
    let conn = storage.get_connection();
    let cutoff = Utc::now() - chrono::Duration::days(days as i64);
    let cutoff_str = cutoff.to_rfc3339();

    let sql = if entity.is_some() {
        "SELECT file_path, recorded_at, score, commit_hash FROM hotspot_trends \
         WHERE recorded_at >= ?1 AND file_path = ?2 \
         ORDER BY recorded_at DESC, file_path ASC"
    } else {
        "SELECT file_path, recorded_at, score, commit_hash FROM hotspot_trends \
         WHERE recorded_at >= ?1 \
         ORDER BY recorded_at DESC, file_path ASC"
    };

    let mut stmt = conn.prepare(sql).into_diagnostic()?;
    let rows = if let Some(path) = entity {
        stmt.query_map(rusqlite::params![&cutoff_str, path], |row| {
            Ok(TrendRow {
                file_path: row.get(0)?,
                recorded_at: row.get(1)?,
                score: row.get(2)?,
                commit_hash: row.get(3)?,
            })
        })
        .into_diagnostic()?
        .collect::<rusqlite::Result<Vec<_>>>()
        .into_diagnostic()?
    } else {
        stmt.query_map([&cutoff_str], |row| {
            Ok(TrendRow {
                file_path: row.get(0)?,
                recorded_at: row.get(1)?,
                score: row.get(2)?,
                commit_hash: row.get(3)?,
            })
        })
        .into_diagnostic()?
        .collect::<rusqlite::Result<Vec<_>>>()
        .into_diagnostic()?
    };

    Ok(rows)
}

/// A single row from the `hotspot_trends` table, used for CLI and JSON output.
#[derive(Debug, Clone, serde::Serialize)]
pub(super) struct TrendRow {
    pub(super) file_path: String,
    pub(super) recorded_at: String,
    pub(super) score: f64,
    pub(super) commit_hash: Option<String>,
}

/// Per-file rollup for default summary mode (top-N by latest raw score).
#[derive(Debug, Clone, PartialEq)]
pub(super) struct TrendFileSummary {
    pub(super) file_path: String,
    pub(super) latest_score: f64,
    pub(super) display_score: f64,
    pub(super) prior_display_score: Option<f64>,
    pub(super) delta: Option<f64>,
    pub(super) sample_count: usize,
    pub(super) last_recorded_at: String,
    pub(super) commit_hash: Option<String>,
}

/// Order-independent summary of trend rows for the days window.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct TrendSummary {
    pub(super) files: Vec<TrendFileSummary>,
    pub(super) total_files: usize,
    pub(super) total_entries: usize,
    pub(super) snapshot_count: usize,
    pub(super) limit: usize,
    pub(super) truncated: bool,
}

/// Output mode for `hotspots trend` (precedence: entity > all > summary).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TrendMode {
    Summary { limit: usize },
    Full,
    Entity,
}

pub(super) fn resolve_trend_mode(entity: &Option<String>, all: bool, limit: usize) -> TrendMode {
    if entity.is_some() {
        TrendMode::Entity
    } else if all {
        TrendMode::Full
    } else {
        TrendMode::Summary { limit }
    }
}

/// Map non-finite scores to lowest rank so `total_cmp` never panics on NaN.
fn rankable_score(score: f64) -> f64 {
    if score.is_finite() {
        score
    } else {
        f64::NEG_INFINITY
    }
}

/// Build a top-`limit` file summary from trend rows.
///
/// Order-independent: groups and ranks internally (does not trust SQL order).
/// Ranking: latest raw score DESC (non-finite → lowest), path ASC. Prior/Δ are
/// window-scoped (previous sample inside the loaded rows only).
pub(super) fn build_trend_summary(rows: &[TrendRow], limit: usize) -> TrendSummary {
    use std::collections::BTreeMap;

    let total_entries = rows.len();
    let mut by_file: BTreeMap<&str, Vec<&TrendRow>> = BTreeMap::new();
    for row in rows {
        by_file.entry(row.file_path.as_str()).or_default().push(row);
    }
    let total_files = by_file.len();

    let mut timestamps: Vec<&str> = rows.iter().map(|r| r.recorded_at.as_str()).collect();
    timestamps.sort_unstable();
    timestamps.dedup();
    let snapshot_count = timestamps.len();

    let mut files: Vec<TrendFileSummary> = by_file
        .into_iter()
        .map(|(path, mut file_rows)| {
            // Latest first by recorded_at (RFC3339 lexicographic = chronological).
            file_rows.sort_by(|a, b| b.recorded_at.cmp(&a.recorded_at));
            let sample_count = file_rows.len();
            let latest = file_rows[0];
            let display_score = normalize_score(latest.score);
            let (prior_display_score, delta) = if sample_count >= 2 {
                let prior = file_rows[1];
                let prior_d = normalize_score(prior.score);
                (Some(prior_d), Some(display_score - prior_d))
            } else {
                (None, None)
            };
            TrendFileSummary {
                file_path: path.to_string(),
                latest_score: latest.score,
                display_score,
                prior_display_score,
                delta,
                sample_count,
                last_recorded_at: latest.recorded_at.clone(),
                commit_hash: latest.commit_hash.clone(),
            }
        })
        .collect();

    files.sort_by(|a, b| {
        let sa = rankable_score(a.latest_score);
        let sb = rankable_score(b.latest_score);
        match sb.total_cmp(&sa) {
            std::cmp::Ordering::Equal => a.file_path.cmp(&b.file_path),
            other => other,
        }
    });

    let effective_limit = if limit == 0 { files.len() } else { limit };
    let truncated = total_files > effective_limit;
    if effective_limit < files.len() {
        files.truncate(effective_limit);
    }

    TrendSummary {
        files,
        total_files,
        total_entries,
        snapshot_count,
        limit: effective_limit,
        truncated,
    }
}

/// Count distinct `file_path` / `recorded_at` in a row set (for full/entity JSON).
fn trend_window_counts(rows: &[TrendRow]) -> (usize, usize, usize) {
    let total_entries = rows.len();
    let mut files: Vec<&str> = rows.iter().map(|r| r.file_path.as_str()).collect();
    files.sort_unstable();
    files.dedup();
    let mut timestamps: Vec<&str> = rows.iter().map(|r| r.recorded_at.as_str()).collect();
    timestamps.sort_unstable();
    timestamps.dedup();
    (files.len(), total_entries, timestamps.len())
}

fn trend_entries_json(rows: &[TrendRow]) -> Vec<serde_json::Value> {
    rows.iter()
        .map(|row| {
            serde_json::json!({
                "file_path": row.file_path,
                "recorded_at": row.recorded_at,
                "score": row.score,
                "display_score": normalize_score(row.score),
                "commit_hash": row.commit_hash,
            })
        })
        .collect()
}

pub(super) fn trend_file_json(file: &TrendFileSummary) -> serde_json::Value {
    let mut obj = serde_json::Map::new();
    obj.insert(
        "filePath".to_string(),
        serde_json::Value::String(file.file_path.clone()),
    );
    obj.insert(
        "latestScore".to_string(),
        serde_json::json!(file.latest_score),
    );
    obj.insert(
        "displayScore".to_string(),
        serde_json::json!(file.display_score),
    );
    if let Some(prior) = file.prior_display_score {
        obj.insert("priorDisplayScore".to_string(), serde_json::json!(prior));
    }
    if let Some(delta) = file.delta {
        obj.insert("delta".to_string(), serde_json::json!(delta));
    }
    obj.insert(
        "sampleCount".to_string(),
        serde_json::json!(file.sample_count),
    );
    obj.insert(
        "lastRecordedAt".to_string(),
        serde_json::Value::String(file.last_recorded_at.clone()),
    );
    obj.insert(
        "commitHash".to_string(),
        match &file.commit_hash {
            Some(h) => serde_json::Value::String(h.clone()),
            None => serde_json::Value::Null,
        },
    );
    serde_json::Value::Object(obj)
}

/// Honest `historyAvailable`: true when trend rows exist or history flags say so.
/// Never false while files/entries are non-empty (H1).
pub(super) fn compute_history_available(history_known_present: bool, total_entries: usize) -> bool {
    history_known_present || total_entries > 0
}

#[allow(clippy::too_many_arguments)]
pub(super) fn execute_hotspots_trend(
    storage: &StorageManager,
    repo: &gix::Repository,
    config: &crate::config::model::Config,
    entity: Option<String>,
    days: u32,
    limit: usize,
    all: bool,
    json: bool,
    bootstrap: bool,
    samples: Option<usize>,
    force: bool,
) -> Result<()> {
    let existing_rows = hotspot_history_row_count(storage)?;
    let mut bootstrapped = false;
    let mut bootstrap_skipped = false;
    let mut _couplings_persisted = false;
    // Single source of truth for "does history exist" across the lock
    // boundary. The unlocked `existing_rows` read above can go stale the
    // moment another process bootstraps history between that read and the
    // `BEGIN IMMEDIATE` lock acquisition below; both the `bootstrapped` and
    // `bootstrap_skipped` branches observe a strictly more current state
    // under the lock, so both update this flag rather than letting the
    // post-lock code fall back to the stale pre-lock value.
    let mut history_known_present = existing_rows > 0;

    if bootstrap {
        let samples = samples.unwrap_or(30);
        let existing_trends = hotspot_trends_row_count(storage)?;
        let should_proceed = if force || existing_trends == 0 {
            if force && existing_trends > 0 {
                clear_hotspot_trends(storage)?;
            }
            true
        } else if json || !crate::util::term::is_interactive() {
            // Non-interactive / JSON mode: preserve data rather than risk an
            // unattended wipe. The user can re-run with --force if desired.
            bootstrap_skipped = true;
            false
        } else {
            let prompt = format!(
                "Trend data already has {} entries. Re-bootstrap from scratch? (y/n) ",
                existing_trends
            );
            let answer =
                crate::util::term::prompt_yes_no_with(&prompt, true, &mut std::io::stdin().lock());
            if answer {
                clear_hotspot_trends(storage)?;
            } else {
                bootstrap_skipped = true;
            }
            answer
        };

        if should_proceed {
            run_historical_bootstrap(storage, repo, config, samples, json)?;
            // The historical bootstrap inserts directly into hotspot_trends, so
            // the `hotspot_history` table remains untouched. Mark history as
            // present so the empty-state path below shows the populated data.
            history_known_present = true;
            bootstrapped = true;
        } else {
            history_known_present = existing_trends > 0 || existing_rows > 0;
        }
    }

    let mut rows = query_trend_rows(storage, &entity, days)?;

    // DX1: when there is no history and no `--bootstrap` flag was passed, offer
    // to bootstrap interactively (default YES). Non-interactive environments
    // (CI, piped stdin, `LEDGERFUL_NON_INTERACTIVE=1`) return false without
    // touching stdin, so they degrade to the existing read-only empty-state
    // output below. JSON mode is excluded so it stays machine-readable. The
    // `prompt_yes_no` call is gated by `&&` short-circuit so its stdout side
    // effect only fires when all the preceding empty-state conditions hold.
    if !bootstrap
        && !json
        && rows.is_empty()
        && !history_known_present
        && prompt_yes_no("No trend history found. Would you like to bootstrap it now? [Y/n] ")
    {
        let outcome = run_bootstrap_compute(storage, repo, config)?;
        if outcome.bootstrapped {
            bootstrapped = true;
            _couplings_persisted = outcome.couplings_persisted;
            history_known_present = true;
            // Re-query so the freshly persisted snapshot is displayed.
            rows = query_trend_rows(storage, &entity, days)?;
        } else if outcome.skipped {
            // History appeared between our unlocked read and the lock;
            // treat it as available and fall through to display.
            history_known_present = true;
            rows = query_trend_rows(storage, &entity, days)?;
        }
    }

    let mode = resolve_trend_mode(&entity, all, limit);
    let history_available = compute_history_available(history_known_present, rows.len());

    // C5: stale + single-datapoint hints from full rows before truncation.
    let (latest_hash, current_head_hash) = rows
        .iter()
        .max_by(|a, b| a.recorded_at.cmp(&b.recorded_at))
        .and_then(|r| r.commit_hash.clone())
        .map(|h| {
            (
                Some(h.clone()),
                crate::git::repo::get_head_info(repo)
                    .ok()
                    .and_then(|(head, _)| head),
            )
        })
        .unwrap_or((None, None));
    let stale =
        latest_hash.is_some() && current_head_hash.is_some() && latest_hash != current_head_hash;
    let single_datapoint = {
        let distinct_timestamps: std::collections::HashSet<&String> =
            rows.iter().map(|r| &r.recorded_at).collect();
        let distinct_commits: std::collections::HashSet<&String> =
            rows.iter().filter_map(|r| r.commit_hash.as_ref()).collect();
        !rows.is_empty() && (distinct_timestamps.len() == 1 || distinct_commits.len() == 1)
    };

    if json {
        let payload = match mode {
            TrendMode::Summary { limit } => {
                let summary = build_trend_summary(&rows, limit);
                let files_json: Vec<serde_json::Value> =
                    summary.files.iter().map(trend_file_json).collect();
                serde_json::json!({
                    "schemaVersion": 1,
                    "mode": "summary",
                    "days": days,
                    "limit": summary.limit,
                    "truncated": summary.truncated,
                    "historyAvailable": history_available,
                    "bootstrapHint": if history_available {
                        serde_json::Value::Null
                    } else {
                        serde_json::Value::String(BOOTSTRAP_HINT.to_string())
                    },
                    "totalFiles": summary.total_files,
                    "totalEntries": summary.total_entries,
                    "snapshotCount": summary.snapshot_count,
                    "files": files_json,
                })
            }
            TrendMode::Full => {
                let (total_files, total_entries, snapshot_count) = trend_window_counts(&rows);
                serde_json::json!({
                    "schemaVersion": 1,
                    "mode": "full",
                    "days": days,
                    "truncated": false,
                    "historyAvailable": history_available,
                    "bootstrapHint": if history_available {
                        serde_json::Value::Null
                    } else {
                        serde_json::Value::String(BOOTSTRAP_HINT.to_string())
                    },
                    "totalFiles": total_files,
                    "totalEntries": total_entries,
                    "snapshotCount": snapshot_count,
                    "entries": trend_entries_json(&rows),
                })
            }
            TrendMode::Entity => {
                let (distinct_files, total_entries, snapshot_count) = trend_window_counts(&rows);
                let total_files = if total_entries > 0 {
                    distinct_files.max(1)
                } else {
                    0
                };
                serde_json::json!({
                    "schemaVersion": 1,
                    "mode": "entity",
                    "days": days,
                    "truncated": false,
                    "historyAvailable": history_available,
                    "bootstrapHint": if history_available {
                        serde_json::Value::Null
                    } else {
                        serde_json::Value::String(BOOTSTRAP_HINT.to_string())
                    },
                    "totalFiles": total_files,
                    "totalEntries": total_entries,
                    "snapshotCount": snapshot_count,
                    "entries": trend_entries_json(&rows),
                })
            }
        };
        crate::output::json::emit(&payload)
            .map_err(|e| miette::miette!("Failed to serialize trend data: {}", e))?;
    } else {
        if bootstrapped {
            println!("Bootstrapped hotspot trend history from historical commits.");
        } else if bootstrap_skipped {
            println!(
                "History already exists; --bootstrap was skipped (no duplicate snapshot created)."
            );
        }

        match mode {
            TrendMode::Summary { limit } => {
                let summary = build_trend_summary(&rows, limit);
                let shown = summary.files.len();
                println!(
                    "\n{}",
                    format!(
                        "Hotspot Trends (Last {} days) — top {} of {} files · {} snapshots",
                        days, shown, summary.total_files, summary.snapshot_count
                    )
                    .if_supports_color(Stream::Stdout, |s| s.style(Style::new().blue().bold()))
                );
                if rows.is_empty() {
                    if history_available {
                        println!("  No trend data in this window.");
                    } else {
                        println!("  No trend history yet for this repository.");
                        println!("  To start tracking, run:");
                        println!("    {}", BOOTSTRAP_HINT);
                    }
                } else {
                    println!("{}", render_trend_summary_table(&summary));
                    if summary.truncated {
                        println!(
                            "\nShowing top {} of {} files. Use --all for full timestamp×file dump, --limit N to change cap, --entity <path> for one-file series.",
                            summary.limit, summary.total_files
                        );
                    }
                    println!("Prior/Δ are within the selected window.");
                    if single_datapoint {
                        println!(
                            "\nOnly one data point. Hotspot scores are recorded automatically via the post-commit hook — more data points will appear over time."
                        );
                    }
                    if stale {
                        println!(
                            "\nTrend data is stale (last recorded for commit {}). The post-commit hook will record a new data point on the next commit.",
                            latest_hash.unwrap_or_default()
                        );
                    }
                }
            }
            TrendMode::Full | TrendMode::Entity => {
                println!(
                    "\n{}",
                    format!("Hotspot Trends (Last {} days)", days)
                        .if_supports_color(Stream::Stdout, |s| s.style(Style::new().blue().bold()))
                );
                if rows.is_empty() {
                    if history_available {
                        println!("  No trend data in this window.");
                    } else {
                        println!("  No trend history yet for this repository.");
                        println!("  To start tracking, run:");
                        println!("    {}", BOOTSTRAP_HINT);
                    }
                } else {
                    println!("{}", render_hotspot_trend_table(&rows));
                    if single_datapoint {
                        println!(
                            "\nOnly one data point. Hotspot scores are recorded automatically via the post-commit hook — more data points will appear over time."
                        );
                    }
                    if stale {
                        println!(
                            "\nTrend data is stale (last recorded for commit {}). The post-commit hook will record a new data point on the next commit.",
                            latest_hash.unwrap_or_default()
                        );
                    }
                }
            }
        }
    }

    Ok(())
}

/// Walk the last `samples` commits from HEAD and record hotspot scores for
/// each one in `hotspot_trends`. Complexity scoring is performed sequentially
/// per commit; progress is emitted to stderr (suppressed in JSON mode). Each
/// commit's score is inserted using the same deduplication logic the
/// post-commit hook uses.
fn run_historical_bootstrap(
    storage: &StorageManager,
    repo: &gix::Repository,
    config: &crate::config::model::Config,
    samples: usize,
    json: bool,
) -> Result<()> {
    let commits = collect_sample_commits(repo, samples)?;
    if commits.is_empty() {
        return Err(miette::miette!(
            "Repository has no commits to bootstrap trend history from."
        ));
    }

    let spinner = if json {
        None
    } else {
        Some(crate::ui::spinner::Spinner::new(format!(
            "Bootstrapping trend history: 0/{}",
            commits.len()
        )))
    };

    let mut slow_warning_printed = false;
    for (idx, (commit_id, timestamp)) in commits.iter().enumerate() {
        let step = idx + 1;
        if let Some(ref s) = spinner {
            s.set_message(format!(
                "Bootstrapping trend history: {}/{}",
                step,
                commits.len()
            ));
        }

        let start = std::time::Instant::now();
        let history_provider = GixHistoryProvider::from_commit(repo, *commit_id);
        let query = HotspotQuery {
            limit: config.hotspots.limit,
            commits: config.hotspots.max_commits,
            decay_half_life: config.hotspots.decay_half_life,
            ..Default::default()
        };
        let hotspots = calculate_hotspots(storage, &history_provider, &query).map_err(|e| {
            miette::miette!("Failed to calculate hotspots for {}: {}", commit_id, e)
        })?;

        if !slow_warning_printed && start.elapsed() > std::time::Duration::from_secs(5) {
            slow_warning_printed = true;
            eprintln!("Bootstrap is slow — consider reducing --samples for large repos.");
        }

        let commit_hash = commit_id.to_string();
        insert_hotspot_trends_with_retry(storage, &hotspots, &commit_hash, timestamp)?;
    }

    if let Some(s) = spinner {
        s.finish();
    }

    Ok(())
}
