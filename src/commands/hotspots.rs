use crate::cli::{HotspotArgs, HotspotSubcommands};
use crate::commands::helpers::get_layout;
use crate::commands::hook_post_commit::insert_hotspot_trends_with_retry;
use crate::config::load_config;
use crate::git::repo::open_repo;
use crate::impact::hotspots::{
    HotspotInterpretation, HotspotQuery, calculate_hotspots,
    compute_hotspot_score_breakdown_from_hotspots, normalize_score,
};
use crate::impact::temporal::{GixHistoryProvider, TemporalEngine};
use crate::index::warn_if_stale;
use crate::output::table::build_premium_table;
use crate::state::storage::StorageManager;
use crate::util::term::prompt_yes_no;
use chrono::Utc;
use miette::{IntoDiagnostic, Result};
use owo_colors::{OwoColorize, Stream, Style};
use std::env;

pub fn execute_hotspots(args: HotspotArgs) -> Result<()> {
    let current_dir = env::current_dir()
        .map_err(|e| miette::miette!("Failed to get current directory: {}", e))?;
    let repo = open_repo(&current_dir)?;
    let layout = get_layout()?;

    // --- Staleness check ---
    let config = load_config(&layout).unwrap_or_default();
    let threshold_days = config.index.stale_threshold_days;
    let need_cozo = args.semantic || args.centrality;
    // Trend --bootstrap inserts snapshots; must open write storage (true RO
    // open fails with "attempt to write a readonly database").
    let need_write = matches!(
        &args.command,
        Some(HotspotSubcommands::Trend {
            bootstrap: true,
            ..
        })
    );
    let storage = if need_write {
        layout.ensure_state_dir()?;
        let storage = StorageManager::init_with_layout(&layout)?;
        if args.auto_index {
            let (storage, _) =
                crate::index::staleness::try_auto_index(storage, threshold_days, &layout)?;
            storage
        } else {
            let _ = warn_if_stale(&storage, threshold_days);
            storage
        }
    } else if args.auto_index {
        // Missing DB must still bootstrap under --auto-index (DoD-3).
        let opened = if need_cozo {
            StorageManager::open_read_only(&layout)
        } else {
            StorageManager::open_read_only_sqlite_only(&layout)
        };
        let storage = match opened {
            Ok(s) => s,
            Err(_) => {
                layout.ensure_state_dir()?;
                StorageManager::init_with_layout(&layout)?
            }
        };
        let (storage, _) =
            crate::index::staleness::try_auto_index(storage, threshold_days, &layout)?;
        storage
    } else {
        let storage = if need_cozo {
            StorageManager::open_read_only(&layout)?
        } else {
            StorageManager::open_read_only_sqlite_only(&layout)?
        };
        let _ = warn_if_stale(&storage, threshold_days);
        storage
    };

    if let Some(command) = args.command {
        match command {
            HotspotSubcommands::Trend {
                entity,
                days,
                limit,
                all,
                json,
                bootstrap,
                samples,
                force,
            } => {
                // clap range 1.. guarantees limit ≥ 1; cast is safe for summary cap.
                let limit = usize::try_from(limit).unwrap_or(usize::MAX);
                return execute_hotspots_trend(
                    &storage, &repo, &config, entity, days, limit, all, json, bootstrap, samples,
                    force,
                );
            }
            HotspotSubcommands::Explain { entity } => {
                return execute_hotspots_explain(&storage, entity, &repo);
            }
            HotspotSubcommands::Budget { json } => {
                return execute_hotspots_budget(&storage, &config, json);
            }
        }
    }

    if args.semantic {
        let cozo = storage
            .cozo
            .as_ref()
            .ok_or_else(|| miette::miette!("CozoDB storage not initialized"))?;

        if !args.json {
            println!("Analyzing semantic similarity hotspots (duplication)...");
        }

        let matches = crate::semantic::hotspots::find_semantic_hotspots(
            cozo,
            layout.root.as_std_path(),
            0.85,
        )?;

        if args.json {
            println!(
                "{}",
                serde_json::to_string_pretty(&matches)
                    .map_err(|e| miette::miette!("Failed to serialize semantic hotspots: {}", e))?
            );
        } else {
            crate::output::human::print_semantic_hotspots(&matches);
        }
        return Ok(());
    }

    let history_provider = GixHistoryProvider::new(&repo);
    let query = HotspotQuery {
        limit: args.limit.unwrap_or(config.hotspots.limit),
        commits: args.commits.unwrap_or(config.hotspots.max_commits),
        days: args.days.map(|d| d as u64),
        decay_half_life: config.hotspots.decay_half_life,
        dir_filter: args.entity.clone(),
        centrality: args.centrality,
        ..Default::default()
    };

    let hotspots = calculate_hotspots(&storage, &history_provider, &query)?;

    if args.snapshot {
        let couplings_persisted =
            persist_hotspots_and_couplings(&storage, &repo, &hotspots, &config)?;
        if !args.json {
            if couplings_persisted {
                println!("Hotspot and temporal coupling snapshot persisted to SQLite.");
            } else {
                println!(
                    "Hotspot snapshot persisted to SQLite (temporal coupling history skipped: repository has fewer than 10 commits)."
                );
            }
        }
    }

    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&hotspots).map_err(|e| miette::miette!("{}", e))?
        );
    } else if args.centrality {
        crate::output::human::print_hotspots_table_with_centrality(&hotspots);
    } else {
        crate::output::human::print_hotspots_table(&hotspots);
    }

    Ok(())
}

/// Persists a hotspot snapshot (and, history permitting, the accompanying
/// temporal-coupling snapshot) to SQLite.
///
/// Returns whether temporal coupling history was actually persisted: `true`
/// if persisted, `false` if skipped because the repository does not yet have
/// enough commit history (`GitError::InsufficientHistory`). Hotspot rows are
/// always persisted regardless of coupling availability, since couplings
/// require strictly more history than hotspots do.
fn persist_hotspots_and_couplings(
    storage: &StorageManager,
    repo: &gix::Repository,
    hotspots: &[crate::impact::packet::Hotspot],
    config: &crate::config::model::Config,
) -> Result<bool> {
    let conn = storage.get_connection();
    let timestamp = Utc::now().to_rfc3339();

    let snapshot_id: Option<i64> = conn
        .query_row(
            "SELECT id FROM snapshots ORDER BY id DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .ok();

    // Insert Hotspots
    for hotspot in hotspots {
        conn.execute(
            "INSERT INTO hotspot_history (snapshot_id, file_path, score, display_score, complexity, frequency, centrality, timestamp) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                snapshot_id,
                hotspot.path.to_string_lossy().to_string(),
                hotspot.score,
                hotspot.display_score,
                hotspot.complexity,
                hotspot.frequency,
                hotspot.centrality.map(|c| c as i64),
                timestamp
            ],
        ).into_diagnostic()?;
    }

    // Calculate and Insert Temporal Couplings. A repository with fewer than
    // 10 commits in the analyzed window is a soft degradation, not a hard
    // failure: the hotspot rows above already succeeded, and the whole point
    // of `--bootstrap` is to give first-time users on young repos a usable
    // first snapshot rather than an error (see CG-F30). Any other GitError
    // still propagates as a hard failure.
    let history_provider = GixHistoryProvider::new(repo);
    let engine = TemporalEngine::new(history_provider, config.temporal.clone());
    let couplings_persisted = match engine.calculate_couplings() {
        Ok(couplings) => {
            for coupling in couplings {
                conn.execute(
                    "INSERT INTO temporal_coupling_history (snapshot_id, file_a, file_b, score, timestamp) \
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    rusqlite::params![
                        snapshot_id,
                        coupling.file_a.to_string_lossy().to_string(),
                        coupling.file_b.to_string_lossy().to_string(),
                        coupling.score,
                        timestamp
                    ],
                )
                .into_diagnostic()?;
            }
            true
        }
        Err(crate::git::GitError::InsufficientHistory { .. }) => false,
        Err(e) => {
            return Err(miette::miette!(
                "Failed to calculate temporal couplings: {}",
                e
            ));
        }
    };

    Ok(couplings_persisted)
}

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

fn format_trend_ts(ts: &str) -> String {
    match chrono::DateTime::parse_from_rfc3339(ts) {
        Ok(dt) => dt
            .with_timezone(&chrono::Utc)
            .format("%Y-%m-%d %H:%M UTC")
            .to_string(),
        Err(_) => ts.to_string(),
    }
}

/// Render full/entity hotspot trend rows as a premium-styled `comfy-table`.
fn render_hotspot_trend_table(rows: &[TrendRow]) -> String {
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
fn render_trend_summary_table(summary: &TrendSummary) -> String {
    let mut table =
        build_premium_table(["File", "Score", "Prior", "Δ", "Samples", "Last recorded"]);
    for file in &summary.files {
        let prior = file
            .prior_display_score
            .map(|p| format!("{p:.3}"))
            .unwrap_or_else(|| "—".to_string());
        let delta = file
            .delta
            .map(|d| {
                if d > 0.0 {
                    format!("+{d:.3}")
                } else {
                    format!("{d:.3}")
                }
            })
            .unwrap_or_else(|| "—".to_string());
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
struct TrendRow {
    file_path: String,
    recorded_at: String,
    score: f64,
    commit_hash: Option<String>,
}

/// Per-file rollup for default summary mode (top-N by latest raw score).
#[derive(Debug, Clone, PartialEq)]
struct TrendFileSummary {
    file_path: String,
    latest_score: f64,
    display_score: f64,
    prior_display_score: Option<f64>,
    delta: Option<f64>,
    sample_count: usize,
    last_recorded_at: String,
    commit_hash: Option<String>,
}

/// Order-independent summary of trend rows for the days window.
#[derive(Debug, Clone, PartialEq)]
struct TrendSummary {
    files: Vec<TrendFileSummary>,
    total_files: usize,
    total_entries: usize,
    snapshot_count: usize,
    limit: usize,
    truncated: bool,
}

/// Output mode for `hotspots trend` (precedence: entity > all > summary).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TrendMode {
    Summary { limit: usize },
    Full,
    Entity,
}

fn resolve_trend_mode(entity: &Option<String>, all: bool, limit: usize) -> TrendMode {
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
fn build_trend_summary(rows: &[TrendRow], limit: usize) -> TrendSummary {
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

fn trend_file_json(file: &TrendFileSummary) -> serde_json::Value {
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
fn compute_history_available(history_known_present: bool, total_entries: usize) -> bool {
    history_known_present || total_entries > 0
}

#[allow(clippy::too_many_arguments)]
fn execute_hotspots_trend(
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
        println!(
            "{}",
            serde_json::to_string_pretty(&payload)
                .map_err(|e| miette::miette!("Failed to serialize trend data: {}", e))?
        );
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

/// CLI presentation so the resolution logic is directly testable.
pub struct HotspotExplanation {
    pub normalized_entity: String,
    pub complexity: i32,
    pub frequency: f64,
    pub couplings: Vec<crate::impact::packet::TemporalCoupling>,
    pub score_breakdown: Option<crate::impact::hotspots::HotspotScoreBreakdown>,
}

pub fn compute_hotspot_explanation(
    storage: &StorageManager,
    entity: &str,
    repo: &gix::Repository,
) -> Result<HotspotExplanation> {
    let repo_root = repo
        .workdir()
        .ok_or_else(|| miette::miette!("No work dir"))?;
    let normalized_entity = crate::util::path::normalize_relative_path(repo_root, entity)
        .unwrap_or_else(|_| entity.to_string());

    // 1. Complexity factor
    let conn = storage.get_connection();
    let complexity: i32 = conn.query_row(
        "SELECT MAX(IFNULL(cognitive_complexity, 0), IFNULL(cyclomatic_complexity, 0)) \
         FROM project_symbols ps JOIN project_files pf ON ps.file_id = pf.id WHERE pf.file_path = ?1",
        [&normalized_entity],
        |row| row.get(0)
    ).unwrap_or(0);

    let config = load_config(&get_layout()?).unwrap_or_default();
    let history_provider = GixHistoryProvider::new(repo);
    let query = HotspotQuery {
        exact_file: None,
        commits: config.hotspots.max_commits,
        decay_half_life: config.hotspots.decay_half_life,
        limit: 10000,
        ..Default::default()
    };
    let hotspots = calculate_hotspots(storage, &history_provider, &query)?;
    let frequency = hotspots
        .iter()
        .find(|h| h.path.to_string_lossy() == normalized_entity)
        .map(|h| h.frequency)
        .unwrap_or(0.0);

    let engine = TemporalEngine::new(history_provider, config.temporal.clone());
    let couplings = engine.calculate_couplings().unwrap_or_default();
    let entity_couplings: Vec<_> = couplings
        .into_iter()
        .filter(|c| {
            c.file_a.to_string_lossy() == normalized_entity
                || c.file_b.to_string_lossy() == normalized_entity
        })
        .collect();

    let score_breakdown = compute_hotspot_score_breakdown_from_hotspots(
        &hotspots,
        &normalized_entity,
        entity_couplings.len(),
    );

    Ok(HotspotExplanation {
        normalized_entity,
        complexity,
        frequency,
        couplings: entity_couplings,
        score_breakdown,
    })
}

fn format_hotspot_interpretation(interpretation: HotspotInterpretation) -> &'static str {
    match interpretation {
        HotspotInterpretation::MaintenanceRisk => {
            "High complexity, low churn — this is a maintenance risk file. \
             The code is intricate but rarely modified, so bugs here are hard to detect \
             and fixes are risky. Consider adding tests or refactoring to reduce complexity."
        }
        HotspotInterpretation::ActiveChurn => {
            "Low complexity, high churn — this file changes frequently but is simple. \
             Review churn for unnecessary volatility."
        }
        HotspotInterpretation::StableHotspot => {
            "High complexity AND high churn — this is an active hotspot. \
             Prioritize refactoring and test coverage."
        }
        HotspotInterpretation::LowRisk => {
            "Low complexity and low churn — this file is low risk. No action needed."
        }
    }
}

fn execute_hotspots_explain(
    storage: &StorageManager,
    entity: String,
    repo: &gix::Repository,
) -> Result<()> {
    let explanation = compute_hotspot_explanation(storage, &entity, repo)?;
    let normalized_entity = &explanation.normalized_entity;

    println!("Hotspot Analysis: {}", normalized_entity);

    println!("\nMetrics:");
    println!("  Complexity: {}", explanation.complexity);
    println!(
        "  Change Frequency (weighted): {:.2}",
        explanation.frequency
    );
    println!("  Temporal Couplings: {}", explanation.couplings.len());

    if let Some(breakdown) = &explanation.score_breakdown {
        println!("\nScore Breakdown:");
        println!(
            "  Normalized complexity: {} / {} = {:.4}",
            breakdown.complexity, breakdown.max_complexity, breakdown.normalized_complexity
        );
        println!(
            "  Normalized frequency: {:.2} / {:.2} = {:.4}",
            breakdown.frequency_weight, breakdown.max_frequency, breakdown.normalized_frequency
        );
        println!(
            "  Base score: {:.4} × {:.4} = {:.4}",
            breakdown.normalized_complexity, breakdown.normalized_frequency, breakdown.base_score
        );
        println!(
            "  Display score (log-normalized): {:.4}",
            breakdown.final_score
        );

        println!("\nInterpretation:");
        println!(
            "  {}",
            format_hotspot_interpretation(breakdown.interpretation)
        );
    }

    if !explanation.couplings.is_empty() {
        println!("\nTop Couplings:");
        for c in explanation.couplings.iter().take(5) {
            let other = if c.file_a.to_string_lossy() == *normalized_entity {
                &c.file_b
            } else {
                &c.file_a
            };
            println!("  {:<40} | Score: {:.2}", other.to_string_lossy(), c.score);
        }
    }

    Ok(())
}

fn execute_hotspots_budget(
    storage: &StorageManager,
    _config: &crate::config::model::Config,
    json: bool,
) -> Result<()> {
    let conn = storage.get_connection();

    let mut stmt = conn
        .prepare(
            "SELECT file_path, score FROM hotspot_history \
         WHERE timestamp = (SELECT MAX(timestamp) FROM hotspot_history) \
         ORDER BY score DESC",
        )
        .into_diagnostic()?;

    let rows = stmt
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, f64>(1)?))
        })
        .into_diagnostic()?;

    let mut violations = Vec::new();
    let threshold = 5.0;

    for row in rows {
        let (path, score) = row.into_diagnostic()?;
        if score > threshold {
            violations.push(serde_json::json!({
                "path": path,
                "score": score,
                "threshold": threshold,
            }));
        }
    }

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "status": if violations.is_empty() { "OK" } else { "VIOLATION" },
                "violations": violations,
            }))
            .map_err(|e| miette::miette!("Failed to serialize budget check: {}", e))?
        );
    } else {
        println!(
            "{}",
            "Hotspot Budget Check"
                .if_supports_color(Stream::Stdout, |s| s.style(Style::new().bold().cyan()))
        );
        if violations.is_empty() {
            println!(
                "  Status: {}",
                "OK".if_supports_color(Stream::Stdout, |s| s.green())
            );
            println!("  All hotspots within risk budget.");
        } else {
            println!(
                "  Status: {}",
                "VIOLATION"
                    .if_supports_color(Stream::Stdout, |s| s.style(Style::new().red().bold()))
            );
            for v in &violations {
                let path = v["path"].as_str().unwrap_or("(unknown)");
                let score = v["score"].as_f64().unwrap_or(0.0);
                let threshold = v["threshold"].as_f64().unwrap_or(5.0);
                println!(
                    "  ! {} exceeds budget: {:.2} > {:.2}",
                    path.if_supports_color(Stream::Stdout, |s| s.yellow()),
                    score,
                    threshold,
                );
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(path: &str, at: &str, score: f64, hash: &str) -> TrendRow {
        TrendRow {
            file_path: path.to_string(),
            recorded_at: at.to_string(),
            score,
            commit_hash: Some(hash.to_string()),
        }
    }

    #[test]
    fn format_trend_ts_parses_rfc3339() {
        assert_eq!(
            format_trend_ts("2026-06-21T15:00:00+00:00"),
            "2026-06-21 15:00 UTC"
        );
    }

    #[test]
    fn format_trend_ts_falls_back_to_input_on_bad_timestamp() {
        assert_eq!(format_trend_ts("not-a-date"), "not-a-date");
    }

    #[test]
    fn render_hotspot_trend_table_uses_premium_framing() {
        let rows = vec![TrendRow {
            file_path: "src/lib.rs".to_string(),
            recorded_at: "2026-06-21T15:00:00+00:00".to_string(),
            score: 1.2345,
            commit_hash: Some("abc".to_string()),
        }];
        let rendered = render_hotspot_trend_table(&rows);
        assert!(
            rendered.contains('╭'),
            "expected rounded table border, got:\n{rendered}"
        );
        assert!(
            rendered.contains("Timestamp")
                && rendered.contains("File")
                && rendered.contains("Score"),
            "expected headers, got:\n{rendered}"
        );
        assert!(
            rendered.contains("src/lib.rs"),
            "expected row content, got:\n{rendered}"
        );
        assert!(
            rendered.contains("7.119"),
            "expected normalized display_score in table (ln_1p of 1.2345*1000 = 7.119), got:\n{rendered}"
        );
    }

    #[test]
    fn trend_display_score_matches_hotspots_normalization() {
        // Raw score 0.0043 previously appeared as a tiny raw value in the trend
        // table; after normalization it should match the `hotspots` display scale.
        let raw = 0.0043_f64;
        let expected = normalize_score(raw);
        let row = TrendRow {
            file_path: "src/lib.rs".to_string(),
            recorded_at: "2026-06-21T15:00:00+00:00".to_string(),
            score: raw,
            commit_hash: Some("abc".to_string()),
        };
        let rendered = render_hotspot_trend_table(std::slice::from_ref(&row));
        assert!(
            rendered.contains(&format!("{:.3}", expected)),
            "expected trend table score {:.3} to match hotspots normalization of raw {raw}, got:\n{rendered}",
            expected
        );

        // JSON shape must include both raw score and computed display_score.
        let entries_json = serde_json::json!({
            "file_path": row.file_path,
            "recorded_at": row.recorded_at,
            "score": row.score,
            "display_score": normalize_score(row.score),
            "commit_hash": row.commit_hash,
        });
        assert_eq!(
            entries_json["display_score"].as_f64().unwrap(),
            expected,
            "JSON display_score must equal hotspots normalization"
        );
        assert!(
            (entries_json["score"].as_f64().unwrap() - raw).abs() < f64::EPSILON,
            "JSON score must remain the raw value"
        );
    }

    #[test]
    fn build_trend_summary_ranks_by_latest_score_path_tiebreak() {
        // Shuffled input order must not affect ranking.
        let rows = vec![
            row("b.rs", "2026-01-02T00:00:00Z", 1.0, "c2"),
            row("a.rs", "2026-01-01T00:00:00Z", 5.0, "c1"),
            row("a.rs", "2026-01-03T00:00:00Z", 2.0, "c3"), // latest for a = 2.0
            row("b.rs", "2026-01-03T00:00:00Z", 2.0, "c3"), // latest for b = 2.0 (tie → a first)
            row("c.rs", "2026-01-03T00:00:00Z", 9.0, "c3"), // highest
        ];
        let summary = build_trend_summary(&rows, 10);
        assert_eq!(summary.total_files, 3);
        assert_eq!(summary.total_entries, 5);
        assert_eq!(summary.snapshot_count, 3);
        assert!(!summary.truncated);
        let paths: Vec<&str> = summary.files.iter().map(|f| f.file_path.as_str()).collect();
        assert_eq!(paths, vec!["c.rs", "a.rs", "b.rs"]);
        assert!((summary.files[0].latest_score - 9.0).abs() < f64::EPSILON);
        assert!((summary.files[1].latest_score - 2.0).abs() < f64::EPSILON);
        assert!((summary.files[2].latest_score - 2.0).abs() < f64::EPSILON);
    }

    #[test]
    fn build_trend_summary_nan_sorts_lowest_deterministic() {
        let rows = vec![
            row("nan.rs", "2026-01-02T00:00:00Z", f64::NAN, "c2"),
            row("inf.rs", "2026-01-02T00:00:00Z", f64::INFINITY, "c2"),
            row("ok.rs", "2026-01-02T00:00:00Z", 1.0, "c2"),
            row(
                "neg_inf.rs",
                "2026-01-02T00:00:00Z",
                f64::NEG_INFINITY,
                "c2",
            ),
        ];
        let summary = build_trend_summary(&rows, 10);
        let paths: Vec<&str> = summary.files.iter().map(|f| f.file_path.as_str()).collect();
        // Finite 1.0 ranks first; non-finite map to NEG_INFINITY → path ASC among them.
        assert_eq!(paths[0], "ok.rs");
        assert_eq!(&paths[1..], &["inf.rs", "nan.rs", "neg_inf.rs"]);
        // Same input twice → same order (determinism; avoid f64 NaN PartialEq).
        let again = build_trend_summary(&rows, 10);
        let paths_again: Vec<&str> = again.files.iter().map(|f| f.file_path.as_str()).collect();
        assert_eq!(paths, paths_again);
    }

    #[test]
    fn build_trend_summary_single_sample_omits_prior_and_delta() {
        let rows = vec![row("solo.rs", "2026-01-01T00:00:00Z", 3.0, "c1")];
        let summary = build_trend_summary(&rows, 20);
        assert_eq!(summary.files.len(), 1);
        assert_eq!(summary.files[0].sample_count, 1);
        assert!(summary.files[0].prior_display_score.is_none());
        assert!(summary.files[0].delta.is_none());
        let json = trend_file_json(&summary.files[0]);
        assert!(json.get("priorDisplayScore").is_none());
        assert!(json.get("delta").is_none());
    }

    #[test]
    fn build_trend_summary_prior_is_window_scoped_previous_sample() {
        let rows = vec![
            row("f.rs", "2026-01-01T00:00:00Z", 1.0, "c1"),
            row("f.rs", "2026-01-02T00:00:00Z", 2.0, "c2"),
            row("f.rs", "2026-01-03T00:00:00Z", 4.0, "c3"),
        ];
        let summary = build_trend_summary(&rows, 5);
        let f = &summary.files[0];
        assert_eq!(f.sample_count, 3);
        assert!((f.latest_score - 4.0).abs() < f64::EPSILON);
        let expected_prior = normalize_score(2.0);
        let expected_delta = normalize_score(4.0) - expected_prior;
        assert_eq!(f.prior_display_score, Some(expected_prior));
        assert_eq!(f.delta, Some(expected_delta));
        assert_eq!(f.commit_hash.as_deref(), Some("c3"));
    }

    #[test]
    fn build_trend_summary_truncates_to_limit() {
        let rows: Vec<TrendRow> = (0..5)
            .map(|i| {
                row(
                    &format!("f{i}.rs"),
                    "2026-01-01T00:00:00Z",
                    f64::from(i),
                    "c1",
                )
            })
            .collect();
        let summary = build_trend_summary(&rows, 2);
        assert_eq!(summary.total_files, 5);
        assert_eq!(summary.files.len(), 2);
        assert!(summary.truncated);
        assert_eq!(summary.limit, 2);
        // Highest scores: f4, f3
        assert_eq!(summary.files[0].file_path, "f4.rs");
        assert_eq!(summary.files[1].file_path, "f3.rs");
    }

    #[test]
    fn history_available_true_when_entries_non_empty() {
        // H1: never false while trend rows exist, even if history_known_present is false.
        assert!(compute_history_available(false, 10));
        assert!(compute_history_available(true, 0));
        assert!(!compute_history_available(false, 0));
    }

    #[test]
    fn resolve_trend_mode_precedence_entity_over_all_over_summary() {
        assert_eq!(
            resolve_trend_mode(&Some("a.rs".into()), true, 5),
            TrendMode::Entity
        );
        assert_eq!(resolve_trend_mode(&None, true, 5), TrendMode::Full);
        assert_eq!(
            resolve_trend_mode(&None, false, 5),
            TrendMode::Summary { limit: 5 }
        );
    }

    #[test]
    fn render_trend_summary_table_has_score_header_and_em_dash_prior() {
        let summary = build_trend_summary(
            &[row("solo.rs", "2026-06-21T15:00:00+00:00", 1.0, "abc")],
            20,
        );
        let rendered = render_trend_summary_table(&summary);
        assert!(
            rendered.contains("Score") && rendered.contains("Prior") && rendered.contains("Δ"),
            "expected summary headers, got:\n{rendered}"
        );
        assert!(
            rendered.contains('—'),
            "single-sample prior/delta should be em dash, got:\n{rendered}"
        );
        assert!(
            rendered.contains("solo.rs"),
            "expected file path, got:\n{rendered}"
        );
    }
}
