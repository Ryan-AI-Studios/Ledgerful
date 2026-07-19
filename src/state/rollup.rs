use crate::commands::verify::enumerate_invalid_ledger_entries;
use crate::config::model::GlobalRollupConfig;
use crate::ledger::db::LedgerDb;
use crate::state::layout::Layout;
use crate::state::storage::StorageManager;
use camino::{Utf8Path, Utf8PathBuf};
use ignore::WalkBuilder;
use miette::{IntoDiagnostic, Result};
use owo_colors::OwoColorize;
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tracing::warn;

/// Per-repo posture summary emitted by `ledger status --global`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RepoPosture {
    pub repo_path: String,
    pub unsigned_entries: usize,
    pub pending_tx: usize,
    pub drift: usize,
    pub last_verify_result: Option<String>,
    pub last_verify_at: Option<String>,
}

/// Full JSON output shape for `ledger status --global --json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GlobalPostureOutput {
    pub total_repos: usize,
    pub skipped_repos: usize,
    pub repos: Vec<RepoPosture>,
    pub warnings: Vec<String>,
}

/// Persistent cache record stored in `~/.ledgerful/rollup/cache.sqlite`.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct CachedRepo {
    repo_path: String,
    db_path: String,
    unsigned_entries: usize,
    pending_tx: usize,
    drift: usize,
    last_verify_result: Option<String>,
    last_verify_at: Option<String>,
}

/// Run the global posture rollup for the configured roots.
///
/// `repo_filter` scopes to a single repo path when provided. `reindex` forces
/// a fresh walk even if the cache appears fresh. `json` controls the output
/// format. Returns `Ok(())` after printing the result.
pub fn execute_ledger_status_global(
    config: &GlobalRollupConfig,
    repo_filter: Option<&str>,
    reindex: bool,
    json: bool,
) -> Result<()> {
    if !config.enabled {
        println!("global rollup disabled — run `ledger status --global --opt-in` to re-enable");
        return Ok(());
    }

    let output = build_global_posture(config, repo_filter, reindex)?;

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&output).into_diagnostic()?
        );
    } else {
        print_global_posture_text(&output);
    }

    Ok(())
}

/// Build the global posture value without printing it. Useful for tests and for
/// callers that want to consume the result programmatically.
pub fn build_global_posture(
    config: &GlobalRollupConfig,
    repo_filter: Option<&str>,
    reindex: bool,
) -> Result<GlobalPostureOutput> {
    let roots = resolve_roots(config)?;
    let cache_path = global_rollup_cache_path()?;
    ensure_parent(&cache_path)?;

    let mut warnings = Vec::new();
    let (repo_map, cached_postures, walk_warnings) =
        discover_repos(&roots, config.timeout_secs, &cache_path, reindex, config)?;
    warnings.extend(walk_warnings);

    let mut postures: Vec<RepoPosture> = Vec::new();
    let mut skipped = 0usize;

    // Build a lookup of cached postures by repo_path. Cached postures come from
    // the cache when all roots (or all non-stale roots) are fresh; they let us
    // skip reopening every per-repo DB on a cache hit. Partial hits re-query
    // only stale-root repos.
    let cached_by_repo: BTreeMap<&str, &RepoPosture> = cached_postures
        .as_ref()
        .map(|vec| {
            vec.iter()
                .map(|p| (p.repo_path.as_str(), p))
                .collect::<BTreeMap<&str, &RepoPosture>>()
        })
        .unwrap_or_default();

    for (repo_path, db_path) in repo_map.iter() {
        if let Some(filter) = repo_filter
            && !repo_filter_matches(repo_path.as_std_path(), filter)
        {
            continue;
        }

        // If this root is fresh in the cache, use the cached posture without
        // reopening the repo DB. Stale roots and any newly discovered repos
        // during a partial re-walk fall through to query_repo_posture.
        let repo_path_str = repo_path.as_str();
        if let Some(cached) = cached_by_repo.get(repo_path_str) {
            postures.push((*cached).clone());
            continue;
        }

        match query_repo_posture(db_path) {
            Ok(posture) => postures.push(posture),
            Err(e) => {
                let msg = format!("skipped {}: {}", repo_path, e);
                warn!("{}", msg);
                warnings.push(msg);
                skipped += 1;
            }
        }
    }

    // Worst-first: unsigned desc, pending desc, drift desc, last_verify_at asc.
    postures.sort_by(|a, b| {
        b.unsigned_entries
            .cmp(&a.unsigned_entries)
            .then_with(|| b.pending_tx.cmp(&a.pending_tx))
            .then_with(|| b.drift.cmp(&a.drift))
            .then_with(|| {
                // Treat None as "oldest" so repos with no verification float down.
                match (&a.last_verify_at, &b.last_verify_at) {
                    (None, None) => std::cmp::Ordering::Equal,
                    (None, Some(_)) => std::cmp::Ordering::Greater,
                    (Some(_), None) => std::cmp::Ordering::Less,
                    (Some(a_ts), Some(b_ts)) => a_ts.cmp(b_ts),
                }
            })
    });

    // Persist cache for subsequent fast paths. Cache is derived only.
    if let Err(e) = write_cache(&cache_path, &roots, &postures) {
        warn!("failed to write rollup cache: {}", e);
    }

    Ok(GlobalPostureOutput {
        total_repos: postures.len(),
        skipped_repos: skipped,
        repos: postures,
        warnings,
    })
}

fn print_global_posture_text(output: &GlobalPostureOutput) {
    println!("{}", "Ledgerful Global Posture".bold().underline());
    println!(
        "{} repo(s) queried, {} skipped",
        output.repos.len().to_string().cyan(),
        output.skipped_repos.to_string().yellow()
    );
    if !output.warnings.is_empty() {
        println!(
            "\n{} {}",
            "Warnings:".yellow().bold(),
            "(per-repo failures are non-fatal)".dimmed()
        );
        for w in &output.warnings {
            println!("  {} {}", "⚠".yellow(), w.dimmed());
        }
    }

    if output.repos.is_empty() {
        println!("\n  No Ledgerful repos discovered.");
    } else {
        let mut table = crate::output::table::build_table(vec![
            "Repo",
            "Unsigned",
            "Pending",
            "Drift",
            "Last Verify",
        ]);
        for p in &output.repos {
            let verify_cell = match (&p.last_verify_result, &p.last_verify_at) {
                (Some(result), Some(at)) => format!("{} {}", result, at.dimmed()),
                (Some(result), None) => result.clone(),
                (None, Some(at)) => format!("— {}", at.dimmed()),
                (None, None) => "—".to_string(),
            };
            table.add_row(vec![
                p.repo_path.cyan().to_string(),
                p.unsigned_entries.to_string().yellow().to_string(),
                p.pending_tx.to_string().yellow().to_string(),
                p.drift.to_string().red().to_string(),
                verify_cell,
            ]);
        }
        println!("\n{}", table);
    }
}

/// Arguments for `timings --global` (Track 0044 Phase C).
///
/// Mutating flags that write per-repo DBs (`--prune`) are refused here.
/// `--opt-in` / `--opt-out` are handled in the CLI dispatcher before this path
/// (they only touch user config).
#[derive(Debug, Clone, Default)]
pub struct GlobalTimingsArgs {
    pub json: bool,
    pub top: Option<u32>,
    pub days: Option<u32>,
    pub export: Option<PathBuf>,
    pub inner: bool,
    pub command: Option<String>,
    pub flame: bool,
    pub explain: Option<String>,
    pub prune: bool,
    pub older_than: Option<String>,
}

/// Per-repo command timing contribution (pooled into the global summary).
///
/// Nested JSON keys use snake_case (matching local `CommandTimingSummary` /
/// `data[]`): `repo_path`, `p50_ms`, `p95_ms`, `p99_ms`, `total_ms`. The parent
/// `GlobalTimingsSummary` envelope remains camelCase.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RepoCommandTiming {
    pub repo_path: String,
    pub command: String,
    pub runs: u64,
    pub p50_ms: i64,
    pub p95_ms: i64,
    pub p99_ms: i64,
    pub total_ms: i64,
}

/// Aggregated inner-span row across repos.
///
/// Snake_case JSON keys match local `timings --inner` (`span_name`, `total_ms`, …).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GlobalInnerAgg {
    pub span_name: String,
    pub samples: u64,
    pub total_ms: i64,
    pub max_ms: i64,
}

/// Outer-command union summary for `timings --global`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GlobalTimingsSummary {
    pub schema_version: u32,
    pub total_repos: usize,
    pub repos_with_timings: usize,
    pub skipped_repos: usize,
    /// Repos opened successfully but missing the `command_timings` table.
    pub timings_absent: usize,
    pub warnings: Vec<String>,
    /// Honest empty-state message when `data` is empty.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// Pooled outer summaries (or empty when no rows / no table).
    pub data: Vec<crate::state::storage::timings::CommandTimingSummary>,
    /// Per-repo breakdown for honesty (same window/filters as `data`).
    pub repos: Vec<RepoCommandTiming>,
}

/// Entry point for `ledgerful timings --global`.
///
/// Discovers repos via the same roots/cache as `ledger status --global`, opens
/// each per-repo DB read-only, unions `command_timings` rows, and never writes
/// to those DBs. Exit 0 with an honest message when nothing is available.
pub fn execute_timings_global(config: &GlobalRollupConfig, args: GlobalTimingsArgs) -> Result<()> {
    if args.prune {
        return Err(miette::miette!(
            "`timings --global` is read-only and cannot prune per-repo databases; \
             run `ledgerful timings --prune --older-than Nd` inside a repo"
        ));
    }

    if !config.enabled {
        println!("global rollup disabled — run `ledger status --global --opt-in` to re-enable");
        return Ok(());
    }

    if let Some(ref command) = args.explain {
        return execute_timings_global_explain(config, &args, command);
    }
    if args.flame {
        return execute_timings_global_flame(config, &args);
    }
    if args.inner {
        return execute_timings_global_inner(config, &args);
    }

    let summary = build_global_timings_summary(config, &args)?;

    if let Some(ref path) = args.export {
        let json = serde_json::to_string_pretty(&summary).into_diagnostic()?;
        std::fs::write(path, json).into_diagnostic()?;
        if !args.json {
            println!(
                "Exported {} command summaries across {} repo(s) to {}.",
                summary.data.len(),
                summary.repos_with_timings,
                path.display()
            );
        }
    }

    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&summary).into_diagnostic()?
        );
        return Ok(());
    }

    print_global_timings_text(&summary, &args);
    Ok(())
}

/// Build the pooled outer-command summary without printing (tests + callers).
pub fn build_global_timings_summary(
    config: &GlobalRollupConfig,
    args: &GlobalTimingsArgs,
) -> Result<GlobalTimingsSummary> {
    let days = args.days.unwrap_or(30);
    let top = args.top.unwrap_or(20);
    let collected = collect_global_timings(config, Some(days), args.command.as_deref())?;

    // Pool outer duration samples by command across all repos.
    let mut by_cmd: BTreeMap<String, Vec<i64>> = BTreeMap::new();
    let mut per_repo: Vec<RepoCommandTiming> = Vec::new();

    for repo in &collected.repos {
        let mut repo_by_cmd: BTreeMap<String, Vec<i64>> = BTreeMap::new();
        for row in &repo.outer {
            by_cmd
                .entry(row.command.clone())
                .or_default()
                .push(row.duration_ms);
            repo_by_cmd
                .entry(row.command.clone())
                .or_default()
                .push(row.duration_ms);
        }
        for (command, durs) in repo_by_cmd {
            let s = crate::state::storage::timings::summarize_from_samples(command, &durs);
            per_repo.push(RepoCommandTiming {
                repo_path: repo.repo_path.clone(),
                command: s.command,
                runs: s.runs,
                p50_ms: s.p50_ms,
                p95_ms: s.p95_ms,
                p99_ms: s.p99_ms,
                total_ms: s.total_ms,
            });
        }
    }

    let mut data: Vec<crate::state::storage::timings::CommandTimingSummary> = by_cmd
        .into_iter()
        .map(|(command, durs)| {
            crate::state::storage::timings::summarize_from_samples(command, &durs)
        })
        .collect();
    data.sort_by(|a, b| {
        b.total_ms
            .cmp(&a.total_ms)
            .then_with(|| a.command.cmp(&b.command))
    });
    data.truncate(top as usize);

    per_repo.sort_by(|a, b| {
        b.total_ms
            .cmp(&a.total_ms)
            .then_with(|| a.repo_path.cmp(&b.repo_path))
            .then_with(|| a.command.cmp(&b.command))
    });

    let message = empty_timings_message(&collected, data.is_empty());

    Ok(GlobalTimingsSummary {
        schema_version: 1,
        total_repos: collected.total_repos,
        repos_with_timings: collected.repos_with_timings,
        skipped_repos: collected.skipped_repos,
        timings_absent: collected.timings_absent,
        warnings: collected.warnings,
        message,
        data,
        repos: per_repo,
    })
}

fn print_global_timings_text(summary: &GlobalTimingsSummary, args: &GlobalTimingsArgs) {
    let days = args.days.unwrap_or(30);
    let top = args.top.unwrap_or(20);

    if let Some(ref msg) = summary.message {
        println!("{msg}");
        if !summary.warnings.is_empty() {
            for w in &summary.warnings {
                println!("  {} {}", "⚠".yellow(), w.dimmed());
            }
        }
        return;
    }

    println!("{}", "Ledgerful Global Timings".bold().underline());
    println!(
        "{} repo(s) with timings, {} absent table, {} skipped (last {days} day(s), top {top})",
        summary.repos_with_timings.to_string().cyan(),
        summary.timings_absent.to_string().yellow(),
        summary.skipped_repos.to_string().yellow()
    );
    if !summary.warnings.is_empty() {
        println!(
            "\n{} {}",
            "Warnings:".yellow().bold(),
            "(per-repo failures are non-fatal)".dimmed()
        );
        for w in &summary.warnings {
            println!("  {} {}", "⚠".yellow(), w.dimmed());
        }
    }

    let mut table = crate::output::table::build_table(vec![
        "Command", "Runs", "p50 ms", "p95 ms", "p99 ms", "Total ms",
    ]);
    for s in &summary.data {
        table.add_row(vec![
            s.command.clone(),
            s.runs.to_string(),
            s.p50_ms.to_string(),
            s.p95_ms.to_string(),
            s.p99_ms.to_string(),
            s.total_ms.to_string(),
        ]);
    }
    println!("\n{table}");
}

fn empty_timings_message(collected: &CollectedGlobalTimings, data_empty: bool) -> Option<String> {
    if !data_empty {
        return None;
    }
    if collected.repos_with_timings == 0 {
        Some(
            "per-repo timing not enabled (no command_timings table found; see 0043 / self-timing)"
                .to_string(),
        )
    } else {
        Some(
            "no global timing rows (tables present but empty in window; run `ledgerful timings` inside a repo)"
                .to_string(),
        )
    }
}

// ── Collection helpers ──────────────────────────────────────────────────────

struct RepoTimingRows {
    repo_path: String,
    outer: Vec<crate::state::storage::timings::TimingRow>,
    inner: Vec<crate::state::storage::timings::TimingRow>,
    /// All rows (outer + inner) for flame output.
    all: Vec<crate::state::storage::timings::TimingRow>,
}

struct CollectedGlobalTimings {
    total_repos: usize,
    repos_with_timings: usize,
    skipped_repos: usize,
    timings_absent: usize,
    warnings: Vec<String>,
    repos: Vec<RepoTimingRows>,
}

/// Discover repos and pull timing rows read-only. Sequential open→query→close.
fn collect_global_timings(
    config: &GlobalRollupConfig,
    days: Option<u32>,
    command: Option<&str>,
) -> Result<CollectedGlobalTimings> {
    let roots = resolve_roots(config)?;
    let cache_path = global_rollup_cache_path()?;
    ensure_parent(&cache_path)?;

    let mut warnings = Vec::new();
    let (repo_map, _cached_postures, walk_warnings) =
        discover_repos(&roots, config.timeout_secs, &cache_path, false, config)?;
    warnings.extend(walk_warnings);

    let mut repos = Vec::new();
    let mut skipped = 0usize;
    let mut timings_absent = 0usize;
    let mut repos_with_timings = 0usize;
    let total_repos = repo_map.len();

    for (repo_path, db_path) in repo_map.iter() {
        match query_repo_timings(db_path, days, command) {
            Ok(None) => {
                timings_absent += 1;
            }
            Ok(Some(rows)) => {
                repos_with_timings += 1;
                repos.push(RepoTimingRows {
                    repo_path: repo_path.to_string(),
                    outer: rows.outer,
                    inner: rows.inner,
                    all: rows.all,
                });
            }
            Err(e) => {
                let msg = format!("skipped {}: {}", repo_path, e);
                warn!("{}", msg);
                warnings.push(msg);
                skipped += 1;
            }
        }
    }

    // Deterministic order by repo path.
    repos.sort_by(|a, b| a.repo_path.cmp(&b.repo_path));

    Ok(CollectedGlobalTimings {
        total_repos,
        repos_with_timings,
        skipped_repos: skipped,
        timings_absent,
        warnings,
        repos,
    })
}

struct QueriedTimings {
    outer: Vec<crate::state::storage::timings::TimingRow>,
    inner: Vec<crate::state::storage::timings::TimingRow>,
    all: Vec<crate::state::storage::timings::TimingRow>,
}

/// Open one repo DB read-only. Returns `Ok(None)` when `command_timings` is absent.
fn query_repo_timings(
    db_path: &Path,
    days: Option<u32>,
    command: Option<&str>,
) -> Result<Option<QueriedTimings>> {
    use crate::state::storage::timings::{TimingQuery, query_timings, table_exists};

    let storage = StorageManager::open_read_only_from_path(db_path)?;
    let conn = storage.get_connection();

    if !table_exists(conn)? {
        let _ = storage.shutdown();
        return Ok(None);
    }

    let cmd = command.map(|s| s.to_string());
    let outer = query_timings(
        conn,
        &TimingQuery {
            outer_only: true,
            inner_only: false,
            command: cmd.clone(),
            days,
            limit: None,
        },
    )?;
    let inner = query_timings(
        conn,
        &TimingQuery {
            outer_only: false,
            inner_only: true,
            command: cmd.clone(),
            days,
            limit: None,
        },
    )?;
    let all = query_timings(
        conn,
        &TimingQuery {
            outer_only: false,
            inner_only: false,
            command: cmd,
            days,
            limit: Some(5000),
        },
    )?;

    // Close the RO connection before returning.
    storage.shutdown()?;

    Ok(Some(QueriedTimings { outer, inner, all }))
}

fn execute_timings_global_inner(
    config: &GlobalRollupConfig,
    args: &GlobalTimingsArgs,
) -> Result<()> {
    let days = args.days.unwrap_or(30);
    let collected = collect_global_timings(config, Some(days), args.command.as_deref())?;

    let mut agg: BTreeMap<String, (u64, i64, i64)> = BTreeMap::new();
    for repo in &collected.repos {
        for r in &repo.inner {
            let name = r
                .span_name
                .clone()
                .unwrap_or_else(|| "<unnamed>".to_string());
            let entry = agg.entry(name).or_insert((0, 0, 0));
            entry.0 += 1;
            entry.1 += r.duration_ms;
            entry.2 = entry.2.max(r.duration_ms);
        }
    }

    let mut aggs: Vec<GlobalInnerAgg> = agg
        .into_iter()
        .map(|(span_name, (samples, total_ms, max_ms))| GlobalInnerAgg {
            span_name,
            samples,
            total_ms,
            max_ms,
        })
        .collect();
    aggs.sort_by(|a, b| {
        b.total_ms
            .cmp(&a.total_ms)
            .then_with(|| a.span_name.cmp(&b.span_name))
    });
    if let Some(top) = args.top {
        aggs.truncate(top as usize);
    }

    let message = empty_timings_message(&collected, aggs.is_empty());
    let envelope = serde_json::json!({
        "schemaVersion": 1,
        "totalRepos": collected.total_repos,
        "reposWithTimings": collected.repos_with_timings,
        "skippedRepos": collected.skipped_repos,
        "timingsAbsent": collected.timings_absent,
        "warnings": collected.warnings,
        "message": message,
        "data": aggs,
    });

    if let Some(ref path) = args.export {
        let json = serde_json::to_string_pretty(&envelope).into_diagnostic()?;
        std::fs::write(path, json).into_diagnostic()?;
        if !args.json {
            println!("Exported inner-span aggregates to {}.", path.display());
        }
    }

    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&envelope).into_diagnostic()?
        );
        return Ok(());
    }

    if let Some(msg) = message {
        println!("{msg}");
        return Ok(());
    }

    let cmd_label = args.command.as_deref().unwrap_or("all commands");
    println!(
        "\n{} — {cmd_label} (last {days} day(s), global)",
        "Inner spans".bold().underline()
    );
    let mut table =
        crate::output::table::build_table(vec!["Span", "Samples", "Total ms", "Max ms"]);
    for a in &aggs {
        table.add_row(vec![
            a.span_name.clone(),
            a.samples.to_string(),
            a.total_ms.to_string(),
            a.max_ms.to_string(),
        ]);
    }
    println!("{table}");
    Ok(())
}

fn execute_timings_global_flame(
    config: &GlobalRollupConfig,
    args: &GlobalTimingsArgs,
) -> Result<()> {
    let days = args.days.unwrap_or(30);
    let collected = collect_global_timings(config, Some(days), args.command.as_deref())?;

    // Collapsed stacks with repo basename prefix for cross-repo disambiguation:
    //   {repo_basename};{command} duration
    //   {repo_basename};{command};{span} duration
    let mut lines: Vec<String> = Vec::new();
    for repo in &collected.repos {
        let basename = Path::new(&repo.repo_path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("repo");
        for r in &repo.all {
            if let Some(ref span) = r.span_name {
                lines.push(format!(
                    "{basename};{};{} {}",
                    r.command,
                    span,
                    r.duration_ms.max(1)
                ));
            } else {
                lines.push(format!("{basename};{} {}", r.command, r.duration_ms.max(1)));
            }
        }
    }
    lines.sort();
    let body = lines.join("\n");

    if let Some(ref path) = args.export {
        std::fs::write(path, &body).into_diagnostic()?;
        if !args.json {
            println!("Wrote collapsed stacks to {}.", path.display());
        }
        return Ok(());
    }

    if args.json {
        let message = empty_timings_message(&collected, body.is_empty());
        let envelope = serde_json::json!({
            "schemaVersion": 1,
            "totalRepos": collected.total_repos,
            "reposWithTimings": collected.repos_with_timings,
            "skippedRepos": collected.skipped_repos,
            "timingsAbsent": collected.timings_absent,
            "warnings": collected.warnings,
            "message": message,
            "data": { "collapsed": body },
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&envelope).into_diagnostic()?
        );
        return Ok(());
    }

    if body.is_empty() {
        if let Some(msg) = empty_timings_message(&collected, true) {
            println!("{msg}");
        } else {
            println!("no global timing rows for flame output");
        }
        return Ok(());
    }

    println!("{body}");
    Ok(())
}

fn execute_timings_global_explain(
    config: &GlobalRollupConfig,
    args: &GlobalTimingsArgs,
    command: &str,
) -> Result<()> {
    // Pool last 7d + prior 7d outer samples for the command across all repos.
    let collected_14d = collect_global_timings(config, Some(14), Some(command))?;

    let mut recent: Vec<crate::state::storage::timings::TimingRow> = Vec::new();
    let mut prior_all: Vec<crate::state::storage::timings::TimingRow> = Vec::new();
    for repo in &collected_14d.repos {
        for row in &repo.outer {
            prior_all.push(row.clone());
        }
    }

    // Re-collect 7d for the recent window (SQLite day filter is relative to now).
    let collected_7d = collect_global_timings(config, Some(7), Some(command))?;
    for repo in &collected_7d.repos {
        for row in &repo.outer {
            recent.push(row.clone());
        }
    }

    let sentence = build_explain_sentence(command, &recent, &prior_all);

    if args.json {
        let message = if recent.is_empty() && collected_7d.repos_with_timings == 0 {
            empty_timings_message(&collected_7d, true)
        } else {
            None
        };
        let envelope = serde_json::json!({
            "schemaVersion": 1,
            "totalRepos": collected_7d.total_repos,
            "reposWithTimings": collected_7d.repos_with_timings,
            "skippedRepos": collected_7d.skipped_repos,
            "timingsAbsent": collected_7d.timings_absent,
            "warnings": collected_7d.warnings,
            "message": message,
            "data": { "explain": sentence },
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&envelope).into_diagnostic()?
        );
    } else {
        println!("{sentence}");
    }
    Ok(())
}

fn build_explain_sentence(
    command: &str,
    recent: &[crate::state::storage::timings::TimingRow],
    prior_14d: &[crate::state::storage::timings::TimingRow],
) -> String {
    if recent.is_empty() {
        return format!(
            "No recorded runs of `{command}` in the last 7 days across discovered repos."
        );
    }

    let recent_avg = mean_duration_ms(recent);
    let recent_ids: std::collections::HashSet<&str> =
        recent.iter().map(|r| r.run_id.as_str()).collect();
    let prior_only: Vec<&crate::state::storage::timings::TimingRow> = prior_14d
        .iter()
        .filter(|r| !recent_ids.contains(r.run_id.as_str()))
        .collect();

    if prior_only.is_empty() {
        format!(
            "`{command}` averaged {recent_avg:.0} ms over {} run(s) in the last 7 days across repos; no prior-week baseline yet.",
            recent.len()
        )
    } else {
        let prior_avg = mean_duration_ms_refs(&prior_only);
        let delta_pct = if prior_avg > 0.0 {
            ((recent_avg - prior_avg) / prior_avg) * 100.0
        } else {
            0.0
        };
        let direction = if delta_pct > 1.0 {
            "up"
        } else if delta_pct < -1.0 {
            "down"
        } else {
            "flat"
        };
        format!(
            "`{command}` averaged {recent_avg:.0} ms over {} run(s) this week across repos, {direction} {delta_pct:.0}% vs the prior week ({prior_avg:.0} ms).",
            recent.len()
        )
    }
}

fn mean_duration_ms(rows: &[crate::state::storage::timings::TimingRow]) -> f64 {
    if rows.is_empty() {
        return 0.0;
    }
    let sum: i64 = rows.iter().map(|r| r.duration_ms).sum();
    sum as f64 / rows.len() as f64
}

fn mean_duration_ms_refs(rows: &[&crate::state::storage::timings::TimingRow]) -> f64 {
    if rows.is_empty() {
        return 0.0;
    }
    let sum: i64 = rows.iter().map(|r| r.duration_ms).sum();
    sum as f64 / rows.len() as f64
}

/// Returns true if the user's `--repo` filter should select `repo_path`.
/// Supports both bare repo names (`foo`) and absolute/relative paths.
fn repo_filter_matches(repo_path: &Path, filter: &str) -> bool {
    // Canonical filter path (if it exists and is absolute).
    let filter_normalized = normalize_filter(filter);
    let repo_normalized = normalize_path_for_match(repo_path);

    // Exact full-path match after normalization.
    if repo_normalized == filter_normalized {
        return true;
    }

    // Last-component match: `--repo foo` matches .../foo but not .../foobar.
    let repo_file_name = repo_path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let filter_file_name = std::path::Path::new(&filter_normalized)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(&filter_normalized);

    repo_file_name == filter_file_name
}

/// Normalize a path string for matching: forward-slash form, no trailing slash.
fn normalize_path_for_match(path: &Path) -> String {
    let s = path.to_string_lossy().replace('\\', "/");
    s.trim_end_matches('/').to_string()
}

/// Normalize the filter the same way, but fall back to the literal input.
fn normalize_filter(filter: &str) -> String {
    let candidate = PathBuf::from(filter);
    match std::fs::canonicalize(&candidate) {
        Ok(canonical) => normalize_path_for_match(&canonical),
        Err(_) => filter.replace('\\', "/").trim_end_matches('/').to_string(),
    }
}

/// Resolve configured roots, expanding leading `~` to the user's home dir.
fn resolve_roots(config: &GlobalRollupConfig) -> Result<Vec<PathBuf>> {
    let home =
        dirs::home_dir().ok_or_else(|| miette::miette!("could not determine home directory"))?;
    let mut resolved = Vec::new();
    for root in &config.roots {
        let path = if let Some(s) = root.to_str()
            && s.starts_with("~/")
        {
            home.join(&s[2..])
        } else {
            root.clone()
        };
        let canonical = match std::fs::canonicalize(&path) {
            Ok(c) => c,
            Err(e) => {
                warn!(
                    "global rollup: root '{}' could not be resolved, skipping: {}",
                    path.display(),
                    e
                );
                continue;
            }
        };
        resolved.push(canonical);
    }
    Ok(resolved)
}

/// Path to the derived rollup cache: `~/.ledgerful/rollup/cache.sqlite`.
///
/// Tests and power users may override this with `LEDGERFUL_ROLLUP_CACHE`.
fn global_rollup_cache_path() -> Result<PathBuf> {
    if let Some(env_path) = std::env::var_os("LEDGERFUL_ROLLUP_CACHE") {
        return Ok(PathBuf::from(env_path));
    }
    let config_dir = user_config_dir()?;
    Ok(config_dir.join("rollup").join("cache.sqlite"))
}

/// Return the Ledgerful user config directory (`~/.ledgerful`), respecting
/// `LEDGERFUL_CONFIG_HOME` for tests and relocated installs.
pub fn user_config_dir() -> Result<PathBuf> {
    // Integration/unit tests may inject a path without mutating process env.
    if let Ok(guard) = TEST_CONFIG_HOME.lock()
        && let Some(ref path) = *guard
    {
        return Ok(path.clone());
    }
    if let Some(env_path) = std::env::var_os("LEDGERFUL_CONFIG_HOME") {
        return Ok(PathBuf::from(env_path));
    }
    let home =
        dirs::home_dir().ok_or_else(|| miette::miette!("could not determine home directory"))?;
    Ok(home.join(".ledgerful"))
}

/// Test inject for `user_config_dir()` (no process-env mutation — avoids
/// `unsafe` `set_var` / Semgrep). Production never calls this; default is None.
static TEST_CONFIG_HOME: std::sync::Mutex<Option<PathBuf>> = std::sync::Mutex::new(None);

/// Install a test config home; returns the previous value for restoration.
pub fn set_test_config_home(path: Option<PathBuf>) -> Option<PathBuf> {
    let mut guard = TEST_CONFIG_HOME.lock().unwrap_or_else(|e| e.into_inner());
    std::mem::replace(&mut *guard, path)
}

fn ensure_parent(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).into_diagnostic()?;
    }
    Ok(())
}

/// Discover per-repo `ledger.db` files under the given roots.
///
/// Returns a map of repo_root → db_path, an optional vec of cached postures
/// for roots that were still fresh (None when a full re-walk happened), plus
/// any walk warnings. Honors the timeout, skips hidden dirs except `.ledgerful`,
/// skips heavy trees, and swallows I/O/permission errors per-entry. When
/// `cached_postures` is `Some`, the caller can use those posture summaries for
/// fresh roots instead of reopening every repo DB.
fn discover_repos(
    roots: &[PathBuf],
    timeout_secs: u64,
    cache_path: &Path,
    reindex: bool,
    config: &GlobalRollupConfig,
) -> Result<DiscoveryResult> {
    // If cache is fresh and --reindex is not set, return cached map directly.
    if !reindex {
        match try_load_cache(roots, cache_path, config.staleness_secs) {
            Ok(Some((cached_map, cached_postures, stale_roots))) if stale_roots.is_empty() => {
                return Ok((cached_map, Some(cached_postures), Vec::new()));
            }
            Ok(Some((cached_map, cached_postures, stale_roots))) => {
                // Re-walk only stale roots, then merge with cached entries.
                let deadline = Instant::now() + Duration::from_secs(timeout_secs);
                let (walked, warnings) = walk_roots(&stale_roots, deadline, config)?;
                let mut merged = cached_map;
                // Remove stale-root entries that are being refreshed, then add new.
                merged.retain(|repo_path, _| {
                    stale_roots
                        .iter()
                        .all(|stale| !root_contains_repo(stale, repo_path.as_str()))
                });
                merged.extend(walked);
                // Drop cached postures that belong to stale roots; they will be
                // re-queried during posture assembly. Fresh-root cached postures
                // are preserved and returned so the cache hit path can avoid
                // reopening those DBs.
                let fresh_postures: Vec<RepoPosture> = cached_postures
                    .into_iter()
                    .filter(|p| {
                        stale_roots
                            .iter()
                            .all(|stale| !root_contains_repo(stale, &p.repo_path))
                    })
                    .collect();
                return Ok((merged, Some(fresh_postures), warnings));
            }
            Ok(None) => {}
            Err(e) => {
                warn!("rollup cache load failed, falling back to full walk: {}", e);
            }
        }
    }

    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    let (map, warnings) = walk_roots(roots, deadline, config)?;
    Ok((map, None, warnings))
}

fn walk_roots(
    roots: &[PathBuf],
    deadline: Instant,
    config: &GlobalRollupConfig,
) -> Result<(BTreeMap<Utf8PathBuf, PathBuf>, Vec<String>)> {
    let mut map: BTreeMap<Utf8PathBuf, PathBuf> = BTreeMap::new();
    let mut warnings = Vec::new();

    for root in roots {
        if Instant::now() >= deadline {
            warnings.push(format!(
                "timeout reached; skipped remaining roots after {}",
                root.display()
            ));
            break;
        }

        let mut builder = WalkBuilder::new(root);
        builder
            .follow_links(false)
            .hidden(false)
            .git_ignore(false)
            .git_global(false)
            .git_exclude(false)
            .parents(false)
            .ignore(false);
        if let Some(depth) = config.max_depth {
            builder.max_depth(Some(depth));
        }
        builder.filter_entry(|entry| {
            // Skip hidden dirs except .ledgerful; skip heavy/common junk trees.
            let path = entry.path();
            !should_prune_path(path)
        });

        let timeout_deadline = deadline;

        let walker = builder.build();
        for entry in walker {
            if Instant::now() >= timeout_deadline {
                warnings.push(format!("timeout reached while walking {}", root.display()));
                break;
            }

            match entry {
                Ok(entry) => {
                    let path = entry.path();
                    let _depth = entry.depth();
                    if let Some(name) = path.file_name()
                        && name == "ledger.db"
                        && let Some(repo_root) = ledger_db_to_repo_root(path)
                        && map.insert(repo_root.clone(), path.to_path_buf()).is_some()
                    {
                        warnings.push(format!("duplicate repo path discovered: {}", repo_root));
                    }
                }
                Err(e) => {
                    // Swallow per-entry errors (PermissionDenied, I/O, etc.).
                    warnings.push(format!("{}: {}", root.display(), e));
                }
            }
        }
    }

    Ok((map, warnings))
}

fn should_prune_path(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    if name == ".ledgerful" {
        return false;
    }
    if name.starts_with('.') {
        return true;
    }
    name == "node_modules" || name == ".git" || name == "target" || name == "vendor"
}

fn ledger_db_to_repo_root(path: &Path) -> Option<Utf8PathBuf> {
    let state_dir = path.parent()?;
    let ledgerful_dir = state_dir.parent()?;
    if ledgerful_dir.file_name() != Some(std::ffi::OsStr::new(".ledgerful")) {
        return None;
    }
    let repo_root = ledgerful_dir.parent()?;
    Utf8PathBuf::from_path_buf(repo_root.to_path_buf()).ok()
}

/// Query a single repo's posture. Opens the DB read-only, runs the posture
/// queries, and closes the connection. Any error is returned so the caller can
/// warn-and-skip.
fn query_repo_posture(db_path: &Path) -> Result<RepoPosture> {
    let storage = StorageManager::open_read_only_from_path(db_path)?;
    let repo_path = storage.root_path().to_string();
    let conn = storage.get_connection();
    let db = LedgerDb::new(conn);

    let entries = db
        .get_all_committed_ledger_entries()
        .map_err(|e| miette::miette!("failed to read ledger entries: {}", e))?;
    // The rollup counts all entries lacking a valid signature (both missing-sig
    // and invalid-sig) as `unsigned_entries` to surface trust risk, regardless of
    // whether the per-repo config enforces signing. `signing_required = true`
    // here means "count missing signatures as invalid" for the rollup view, not
    // "signing is enforced for this repo".
    let invalid = enumerate_invalid_ledger_entries(&entries, true);

    let pending = db
        .get_all_pending()
        .map_err(|e| miette::miette!("failed to read pending transactions: {}", e))?;
    let unaudited = db
        .get_all_unaudited()
        .map_err(|e| miette::miette!("failed to read unaudited drift: {}", e))?;

    let (last_result, last_at) = match storage.get_latest_verification_run() {
        Ok(Some((_, ts, pass))) => {
            let result = if pass {
                "PASS".to_string()
            } else {
                "FAIL".to_string()
            };
            (Some(result), Some(ts))
        }
        _ => (None, None),
    };

    storage.shutdown()?;

    Ok(RepoPosture {
        repo_path,
        unsigned_entries: invalid.len(),
        pending_tx: pending.len(),
        drift: unaudited.len(),
        last_verify_result: last_result,
        last_verify_at: last_at,
    })
}

/// Cache schema: a single table holding a JSON blob of discovered repos plus
/// root mtimes for staleness checks.
fn ensure_cache_schema(conn: &rusqlite::Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS rollup_cache (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );",
    )
    .into_diagnostic()?;
    Ok(())
}

/// Cache key for the discovery roots list.
const CACHE_KEY_ROOTS: &str = "roots";
/// Cache key prefix for per-repo posture records.
const CACHE_KEY_REPOS: &str = "repos";

fn root_mtime(root: &Path) -> Option<u64> {
    let ledgerful = root.join(".ledgerful");
    let target = if ledgerful.exists() {
        ledgerful
    } else {
        root.to_path_buf()
    };
    std::fs::metadata(&target)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok().map(|d| d.as_secs()))
}

fn root_contains_repo(root: &Path, repo_path: &str) -> bool {
    let repo = Utf8PathBuf::from(repo_path);
    let root_utf8 = match Utf8PathBuf::from_path_buf(root.to_path_buf()) {
        Ok(p) => p,
        Err(_) => return false,
    };
    let root_str = root_utf8.as_str();
    let repo_str = repo.as_str();
    if !repo_str.starts_with(root_str) {
        return false;
    }
    if repo_str.len() == root_str.len() {
        return true;
    }
    // Treat both `/` and `\` as path separators (Windows + POSIX).
    let next = repo_str[root_str.len()..].chars().next();
    next == Some('/') || next == Some('\\')
}

/// Load the cache if it exists, is uncorrupted, and is not stale.
/// Returns Some((map, cached_postures, stale_roots)) if at least a partial
/// cached result is usable. `cached_postures` holds the posture summaries for
/// repos whose root is still fresh; `stale_roots` lists roots that need
/// re-walking. Returns None if the cache is missing, corrupted, or too stale to
/// trust.
///
/// Edge case: a repo recorded in the cache may no longer exist on disk (e.g.
/// the user deleted it since the last walk). On a fully-fresh cache it will
/// still be returned here because the staleness window — not an on-disk
/// re-walk — bounds cache validity. Callers should handle missing DBs during
/// posture assembly just as they handle any other per-repo failure.
type DiscoveredRepos = BTreeMap<Utf8PathBuf, PathBuf>;
/// Discovery result tuple: repo_root → db_path map, optional cached postures
/// for fresh roots, and walk warnings.
type DiscoveryResult = (DiscoveredRepos, Option<Vec<RepoPosture>>, Vec<String>);
/// Cache load tuple: repo map, cached postures for fresh roots, and stale roots.
type CacheLoadResult = (DiscoveredRepos, Vec<RepoPosture>, Vec<PathBuf>);

fn try_load_cache(
    roots: &[PathBuf],
    cache_path: &Path,
    staleness_secs: u64,
) -> Result<Option<CacheLoadResult>> {
    if !cache_path.exists() {
        return Ok(None);
    }

    let conn = rusqlite::Connection::open(cache_path).into_diagnostic()?;
    let integrity: String = match conn.query_row("PRAGMA integrity_check", [], |row| row.get(0)) {
        Ok(s) => s,
        Err(e) => {
            warn!("rollup cache integrity check failed: {}; re-walking", e);
            return Ok(None);
        }
    };
    if integrity != "ok" {
        warn!(
            "rollup cache integrity check returned non-ok: {}; re-walking",
            integrity
        );
        return Ok(None);
    }
    ensure_cache_schema(&conn)?;

    let cached_roots_json: Option<String> = conn
        .query_row(
            "SELECT value FROM rollup_cache WHERE key = ?1",
            [CACHE_KEY_ROOTS],
            |row| row.get(0),
        )
        .optional()
        .into_diagnostic()?;
    let cached_roots: Vec<PathBuf> = match cached_roots_json {
        Some(json) => serde_json::from_str(&json).into_diagnostic()?,
        None => return Ok(None),
    };
    if cached_roots != roots {
        // Roots changed; full re-walk required.
        return Ok(None);
    }

    let cached_repos_json: Option<String> = conn
        .query_row(
            "SELECT value FROM rollup_cache WHERE key = ?1",
            [CACHE_KEY_REPOS],
            |row| row.get(0),
        )
        .optional()
        .into_diagnostic()?;
    let cached_repos: Vec<CachedRepo> = match cached_repos_json {
        Some(json) => serde_json::from_str(&json).into_diagnostic()?,
        None => return Ok(None),
    };

    let mut map = BTreeMap::new();
    let mut cached_postures = Vec::new();
    for repo in cached_repos {
        let repo_path = Utf8PathBuf::from(repo.repo_path.clone());
        map.insert(repo_path, PathBuf::from(repo.db_path));
        cached_postures.push(RepoPosture {
            repo_path: repo.repo_path,
            unsigned_entries: repo.unsigned_entries,
            pending_tx: repo.pending_tx,
            drift: repo.drift,
            last_verify_result: repo.last_verify_result,
            last_verify_at: repo.last_verify_at,
        });
    }

    // Staleness decision: a root is fresh if BOTH
    //   cache_mtime + staleness_secs >= now   (cache isn't too old), AND
    //   cache_mtime >= root_mtime               (root hasn't changed since cache).
    // Re-walk when either condition fails.
    let cache_mtime: u64 = std::fs::metadata(cache_path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok().map(|d| d.as_secs()))
        .unwrap_or(0);
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let mut stale_roots = Vec::new();
    for root in roots {
        let root_mtime_val = root_mtime(root).unwrap_or(0);
        let cache_fresh_by_age = cache_mtime.saturating_add(staleness_secs) >= now;
        let cache_fresh_by_root = cache_mtime >= root_mtime_val;
        if !cache_fresh_by_age || !cache_fresh_by_root {
            stale_roots.push(root.clone());
        }
    }

    Ok(Some((map, cached_postures, stale_roots)))
}

fn write_cache(cache_path: &Path, roots: &[PathBuf], postures: &[RepoPosture]) -> Result<()> {
    let conn = rusqlite::Connection::open(cache_path).into_diagnostic()?;
    ensure_cache_schema(&conn)?;

    let cached: Vec<CachedRepo> = postures
        .iter()
        .map(|p| CachedRepo {
            repo_path: p.repo_path.clone(),
            db_path: {
                let layout = Layout::new(Utf8Path::new(&p.repo_path));
                layout.state_subdir().join("ledger.db").to_string()
            },
            unsigned_entries: p.unsigned_entries,
            pending_tx: p.pending_tx,
            drift: p.drift,
            last_verify_result: p.last_verify_result.clone(),
            last_verify_at: p.last_verify_at.clone(),
        })
        .collect();

    let roots_json = serde_json::to_string(roots).into_diagnostic()?;
    let repos_json = serde_json::to_string(&cached).into_diagnostic()?;

    conn.execute(
        "INSERT OR REPLACE INTO rollup_cache (key, value) VALUES (?1, ?2)",
        [CACHE_KEY_ROOTS, &roots_json],
    )
    .into_diagnostic()?;
    conn.execute(
        "INSERT OR REPLACE INTO rollup_cache (key, value) VALUES (?1, ?2)",
        [CACHE_KEY_REPOS, &repos_json],
    )
    .into_diagnostic()?;

    Ok(())
}

/// Set the `[global_rollup] enabled` flag in the user config at
/// `~/.ledgerful/config.toml`, creating the file/table as needed.
pub fn set_global_rollup_enabled(enabled: bool) -> Result<()> {
    let config_dir = user_config_dir()?;
    std::fs::create_dir_all(&config_dir).into_diagnostic()?;
    let config_path = config_dir.join("config.toml");

    let mut doc = if config_path.exists() {
        let content = std::fs::read_to_string(&config_path).into_diagnostic()?;
        content
            .parse::<toml_edit::DocumentMut>()
            .map_err(|e| miette::miette!("failed to parse user config: {}", e))?
    } else {
        toml_edit::DocumentMut::new()
    };

    let root = doc.as_table_mut();
    let rollup = root.entry("global_rollup").or_insert_with(|| {
        let mut t = toml_edit::Table::new();
        t.set_implicit(false);
        toml_edit::Item::Table(t)
    });
    let table = rollup
        .as_table_mut()
        .ok_or_else(|| miette::miette!("global_rollup is not a table"))?;
    table.insert("enabled", toml_edit::value(enabled));

    std::fs::write(&config_path, doc.to_string()).into_diagnostic()?;
    Ok(())
}
