//! Local-only command timing storage (Track 0043).
//!
//! Persistence surface for self-timing: batch insert, query, prune, and
//! opt-out helpers. Capture lives in `observability::self_timing`; this
//! module never touches the network.

use miette::{IntoDiagnostic, Result};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

/// One row in `command_timings` (outer or inner span).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TimingRow {
    pub run_id: String,
    pub ts_utc: String,
    pub command: String,
    pub duration_ms: i64,
    pub exit_code: i32,
    pub repo_size_bytes: Option<i64>,
    pub argv_hash: Option<String>,
    pub ledger_tx_id: Option<String>,
    pub parent_span_id: Option<String>,
    pub span_name: Option<String>,
    pub notes: Option<String>,
}

/// Filters for querying timing rows.
#[derive(Debug, Clone, Default)]
pub struct TimingQuery {
    /// Only outer rows (span_name IS NULL). Default true for summary views.
    pub outer_only: bool,
    /// Only inner rows (span_name IS NOT NULL).
    pub inner_only: bool,
    /// Filter by command name (exact match).
    pub command: Option<String>,
    /// Only rows with ts_utc >= now - days.
    pub days: Option<u32>,
    /// Limit result count (applied after ordering by ts_utc DESC).
    pub limit: Option<u32>,
}

/// Insert all rows for one invocation in a single transaction.
///
/// Never call this from a span `on_close` path — only from `TimedCommand`
/// drop / explicit batch flush.
pub fn insert_timing_batch(conn: &mut Connection, rows: &[TimingRow]) -> Result<usize> {
    if rows.is_empty() {
        return Ok(0);
    }
    let tx = conn.transaction().into_diagnostic()?;
    {
        let mut stmt = tx
            .prepare_cached(
                "INSERT INTO command_timings (
                    run_id, ts_utc, command, duration_ms, exit_code,
                    repo_size_bytes, argv_hash, ledger_tx_id,
                    parent_span_id, span_name, notes
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            )
            .into_diagnostic()?;
        for row in rows {
            stmt.execute(params![
                row.run_id,
                row.ts_utc,
                row.command,
                row.duration_ms,
                row.exit_code,
                row.repo_size_bytes,
                row.argv_hash,
                row.ledger_tx_id,
                row.parent_span_id,
                row.span_name,
                row.notes,
            ])
            .into_diagnostic()?;
        }
    }
    tx.commit().into_diagnostic()?;
    Ok(rows.len())
}

/// Query timing rows with optional filters. Results are sorted by `ts_utc` DESC, `id` DESC.
pub fn query_timings(conn: &Connection, query: &TimingQuery) -> Result<Vec<TimingRow>> {
    let mut sql = String::from(
        "SELECT run_id, ts_utc, command, duration_ms, exit_code,
                repo_size_bytes, argv_hash, ledger_tx_id,
                parent_span_id, span_name, notes
         FROM command_timings WHERE 1=1",
    );
    let mut binds: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

    if query.outer_only {
        sql.push_str(" AND span_name IS NULL");
    }
    if query.inner_only {
        sql.push_str(" AND span_name IS NOT NULL");
    }
    if let Some(ref cmd) = query.command {
        sql.push_str(" AND command = ?");
        binds.push(Box::new(cmd.clone()));
    }
    if let Some(days) = query.days {
        // ISO-8601 UTC strings sort lexicographically; subtract days via julianday.
        sql.push_str(" AND ts_utc >= strftime('%Y-%m-%dT%H:%M:%fZ', 'now', ?)");
        binds.push(Box::new(format!("-{days} days")));
    }

    sql.push_str(" ORDER BY ts_utc DESC, id DESC");

    if let Some(limit) = query.limit {
        sql.push_str(" LIMIT ?");
        binds.push(Box::new(limit as i64));
    }

    let mut stmt = conn.prepare(&sql).into_diagnostic()?;
    let params_refs: Vec<&dyn rusqlite::types::ToSql> = binds.iter().map(|b| b.as_ref()).collect();
    let rows = stmt
        .query_map(params_refs.as_slice(), |row| {
            Ok(TimingRow {
                run_id: row.get(0)?,
                ts_utc: row.get(1)?,
                command: row.get(2)?,
                duration_ms: row.get(3)?,
                exit_code: row.get(4)?,
                repo_size_bytes: row.get(5)?,
                argv_hash: row.get(6)?,
                ledger_tx_id: row.get(7)?,
                parent_span_id: row.get(8)?,
                span_name: row.get(9)?,
                notes: row.get(10)?,
            })
        })
        .into_diagnostic()?
        .collect::<std::result::Result<Vec<_>, _>>()
        .into_diagnostic()?;
    Ok(rows)
}

/// Delete timing rows older than `older_than_days`.
///
/// When `inner_only` is true, only inner-span rows are pruned; otherwise only
/// outer rows (span_name IS NULL) are pruned — matching the retention split
/// (outer 90d / inner 30d defaults at the CLI layer).
pub fn prune_timings(conn: &Connection, older_than_days: u32, inner_only: bool) -> Result<usize> {
    let span_clause = if inner_only {
        "span_name IS NOT NULL"
    } else {
        "span_name IS NULL"
    };
    let sql = format!(
        "DELETE FROM command_timings
         WHERE {span_clause}
           AND ts_utc < strftime('%Y-%m-%dT%H:%M:%fZ', 'now', ?1)"
    );
    let offset = format!("-{older_than_days} days");
    let n = conn.execute(&sql, params![offset]).into_diagnostic()?;
    Ok(n)
}

/// Total row count in `command_timings`.
pub fn count_timings(conn: &Connection) -> Result<i64> {
    let n: i64 = conn
        .query_row("SELECT COUNT(*) FROM command_timings", [], |row| row.get(0))
        .into_diagnostic()?;
    Ok(n)
}

/// Distinct span_name count for inner rows in the last `days` days.
pub fn count_distinct_span_names(conn: &Connection, days: u32) -> Result<i64> {
    let offset = format!("-{days} days");
    let n: i64 = conn
        .query_row(
            "SELECT COUNT(DISTINCT span_name) FROM command_timings
             WHERE span_name IS NOT NULL
               AND ts_utc >= strftime('%Y-%m-%dT%H:%M:%fZ', 'now', ?1)",
            params![offset],
            |row| row.get(0),
        )
        .into_diagnostic()?;
    Ok(n)
}

/// Whether the `command_timings` table exists.
pub fn table_exists(conn: &Connection) -> Result<bool> {
    let n: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='command_timings'",
            [],
            |row| row.get(0),
        )
        .into_diagnostic()?;
    Ok(n > 0)
}

/// One outer-row sample for comparability summaries (Track 0330).
#[derive(Debug, Clone)]
pub struct TimingSample {
    pub duration_ms: i64,
    pub exit_code: i32,
    pub argv_hash: Option<String>,
}

impl TimingSample {
    pub fn from_row(row: &TimingRow) -> Self {
        Self {
            duration_ms: row.duration_ms,
            exit_code: row.exit_code,
            argv_hash: row.argv_hash.clone(),
        }
    }
}

/// Per-workload slice on a command summary (`workloads[]`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkloadSlice {
    pub argv_hash: String,
    pub runs: u64,
    pub p50_ms: i64,
}

/// Aggregate stats for one command (outer rows only).
///
/// `runs` is the **success** count (`exit_code == 0`). `comparable` always
/// serializes (never omit-empty).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandTimingSummary {
    pub command: String,
    pub runs: u64,
    pub p50_ms: i64,
    pub p95_ms: i64,
    pub p99_ms: i64,
    pub total_ms: i64,
    pub comparable: bool,
    pub workload_count: u64,
    pub nonzero_exit_runs: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_spread: Option<String>,
    pub window_days: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub incomparable_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sample_note: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workloads: Option<Vec<WorkloadSlice>>,
}

pub const UNHASHED_ARGV: &str = "<unhashed>";

pub const REASON_NO_RECENT: &str = "noRecentRuns";
pub const REASON_NO_SUCCESS: &str = "noSuccessSamples";
pub const REASON_MIXED_WORKLOADS: &str = "mixedWorkloads";
pub const REASON_MIXED_DURATIONS: &str = "mixedDurations";
pub const REASON_SMALL_SAMPLE: &str = "smallSample";
pub const REASON_WORKLOAD_MISMATCH: &str = "workloadMismatch";
pub const REASON_NO_PRIOR: &str = "noPriorBaseline";
pub const REASON_ZERO_BASELINE: &str = "zeroBaseline";

fn argv_bucket(hash: &Option<String>) -> String {
    match hash {
        Some(s) if !s.is_empty() => s.clone(),
        _ => UNHASHED_ARGV.to_string(),
    }
}

/// Zero-safe spread: homogeneous 0 ms is single; never divide by p50==0.
pub fn duration_spread_single(p50_ms: i64, p95_ms: i64) -> bool {
    p95_ms < 10 * p50_ms.max(1)
}

/// Summarize outer timings grouped by command, ordered by total_ms DESC.
pub fn summarize_outer(
    conn: &Connection,
    days: Option<u32>,
    top: Option<u32>,
) -> Result<Vec<CommandTimingSummary>> {
    let mut sql = String::from(
        "SELECT command, duration_ms, exit_code, argv_hash FROM command_timings
         WHERE span_name IS NULL",
    );
    let mut binds: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
    if let Some(days) = days {
        sql.push_str(" AND ts_utc >= strftime('%Y-%m-%dT%H:%M:%fZ', 'now', ?)");
        binds.push(Box::new(format!("-{days} days")));
    }
    sql.push_str(" ORDER BY command, duration_ms");

    let mut stmt = conn.prepare(&sql).into_diagnostic()?;
    let params_refs: Vec<&dyn rusqlite::types::ToSql> = binds.iter().map(|b| b.as_ref()).collect();
    let rows: Vec<(String, i64, i32, Option<String>)> = stmt
        .query_map(params_refs.as_slice(), |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .into_diagnostic()?
        .collect::<std::result::Result<Vec<_>, _>>()
        .into_diagnostic()?;

    let mut by_cmd: std::collections::BTreeMap<String, Vec<TimingSample>> =
        std::collections::BTreeMap::new();
    for (cmd, duration_ms, exit_code, argv_hash) in rows {
        by_cmd.entry(cmd).or_default().push(TimingSample {
            duration_ms,
            exit_code,
            argv_hash,
        });
    }

    let window_days = u64::from(days.unwrap_or(0));
    let mut summaries: Vec<CommandTimingSummary> = by_cmd
        .into_iter()
        .map(|(command, samples)| summarize_from_samples(command, &samples, window_days))
        .collect();

    summaries.sort_by(|a, b| {
        b.total_ms
            .cmp(&a.total_ms)
            .then_with(|| a.command.cmp(&b.command))
    });
    if let Some(top) = top {
        summaries.truncate(top as usize);
    }
    Ok(summaries)
}

/// Build a summary from outer samples (local or multi-repo union).
///
/// Success-only percentiles (`exit_code == 0`). Empty success cohort yields
/// `runs=0`, zeroed percentiles, `comparable: false`, `noSuccessSamples`.
pub fn summarize_from_samples(
    command: String,
    samples: &[TimingSample],
    window_days: u64,
) -> CommandTimingSummary {
    let nonzero_exit_runs = samples.iter().filter(|s| s.exit_code != 0).count() as u64;
    let success: Vec<&TimingSample> = samples.iter().filter(|s| s.exit_code == 0).collect();

    if success.is_empty() {
        return CommandTimingSummary {
            command,
            runs: 0,
            p50_ms: 0,
            p95_ms: 0,
            p99_ms: 0,
            total_ms: 0,
            comparable: false,
            workload_count: 0,
            nonzero_exit_runs,
            duration_spread: None,
            window_days,
            incomparable_reason: Some(REASON_NO_SUCCESS.to_string()),
            sample_note: Some("no successful runs in window".to_string()),
            workloads: None,
        };
    }

    let mut durs: Vec<i64> = success.iter().map(|s| s.duration_ms).collect();
    durs.sort_unstable();
    let runs = durs.len() as u64;
    let total_ms: i64 = durs.iter().sum();
    let p50_ms = percentile_sorted(&durs, 50);
    let p95_ms = percentile_sorted(&durs, 95);
    let p99_ms = percentile_sorted(&durs, 99);
    let spread_single = duration_spread_single(p50_ms, p95_ms);
    let duration_spread = if spread_single { "single" } else { "mixed" };

    let mut by_hash: std::collections::BTreeMap<String, Vec<i64>> =
        std::collections::BTreeMap::new();
    for s in &success {
        by_hash
            .entry(argv_bucket(&s.argv_hash))
            .or_default()
            .push(s.duration_ms);
    }
    let workload_count = by_hash.len() as u64;

    let mut workloads: Vec<WorkloadSlice> = by_hash
        .into_iter()
        .map(|(argv_hash, mut hdurs)| {
            hdurs.sort_unstable();
            WorkloadSlice {
                argv_hash,
                runs: hdurs.len() as u64,
                p50_ms: percentile_sorted(&hdurs, 50),
            }
        })
        .collect();
    workloads.sort_by(|a, b| {
        b.runs
            .cmp(&a.runs)
            .then_with(|| a.argv_hash.cmp(&b.argv_hash))
    });
    workloads.truncate(5);
    let workloads = if workload_count > 1 {
        Some(workloads)
    } else {
        None
    };

    let incomparable_reason = if workload_count > 1 {
        Some(REASON_MIXED_WORKLOADS.to_string())
    } else if !spread_single {
        Some(REASON_MIXED_DURATIONS.to_string())
    } else if runs < 5 {
        Some(REASON_SMALL_SAMPLE.to_string())
    } else {
        None
    };
    let comparable = incomparable_reason.is_none();
    let sample_note = incomparable_reason.as_ref().map(|r| match r.as_str() {
        REASON_MIXED_WORKLOADS => format!("{workload_count} workloads (argv_hash)"),
        REASON_MIXED_DURATIONS => "duration mix on the same argv_hash".to_string(),
        REASON_SMALL_SAMPLE => format!("n={runs} too small for p95"),
        other => other.to_string(),
    });

    CommandTimingSummary {
        command,
        runs,
        p50_ms,
        p95_ms,
        p99_ms,
        total_ms,
        comparable,
        workload_count,
        nonzero_exit_runs,
        duration_spread: Some(duration_spread.to_string()),
        window_days,
        incomparable_reason,
        sample_note,
        workloads,
    }
}

fn single_argv_hash(samples: &[TimingSample]) -> Option<String> {
    let success: Vec<&TimingSample> = samples.iter().filter(|s| s.exit_code == 0).collect();
    if success.is_empty() {
        return None;
    }
    let first = argv_bucket(&success[0].argv_hash);
    if success.iter().all(|s| argv_bucket(&s.argv_hash) == first) {
        Some(first)
    } else {
        None
    }
}

/// Structured `--explain` result (local and global share this).
#[derive(Debug, Clone, Serialize)]
pub struct ExplainReport {
    pub sentence: String,
    pub command: String,
    pub runs: u64,
    pub comparable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub p50_ms: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prior_p50_ms: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub incomparable_reason: Option<String>,
}

fn incomparable_sentence(
    command: &str,
    recent_p50: i64,
    n: u64,
    prior_p50: i64,
    reason: &str,
) -> String {
    format!(
        "`{command}` p50 {recent_p50} ms over {n} run(s) this week vs prior week ({prior_p50} ms); not comparable ({reason})."
    )
}

/// Explain last-7d vs prior-7d using p50 of the success cohort.
///
/// `prior_14d` is the 14-day outer pool; prior-week is that pool minus recent
/// `run_id`s. Calendar windows are the caller's responsibility (`limit: None`).
pub fn explain_command(
    command: &str,
    recent: &[TimingRow],
    prior_14d: &[TimingRow],
    across_repos: bool,
) -> ExplainReport {
    if recent.is_empty() {
        let sentence = if across_repos {
            format!("No recorded runs of `{command}` in the last 7 days across discovered repos.")
        } else {
            format!("No recorded runs of `{command}` in the last 7 days.")
        };
        return ExplainReport {
            sentence,
            command: command.to_string(),
            runs: 0,
            comparable: false,
            p50_ms: None,
            prior_p50_ms: None,
            incomparable_reason: Some(REASON_NO_RECENT.to_string()),
        };
    }

    let recent_samples: Vec<TimingSample> = recent.iter().map(TimingSample::from_row).collect();
    let recent_sum = summarize_from_samples(command.to_string(), &recent_samples, 7);

    let recent_ids: std::collections::HashSet<&str> =
        recent.iter().map(|r| r.run_id.as_str()).collect();
    let prior_only: Vec<&TimingRow> = prior_14d
        .iter()
        .filter(|r| !recent_ids.contains(r.run_id.as_str()))
        .collect();
    let prior_samples: Vec<TimingSample> = prior_only
        .iter()
        .map(|r| TimingSample::from_row(r))
        .collect();
    let prior_sum = if prior_samples.is_empty() {
        None
    } else {
        Some(summarize_from_samples(
            command.to_string(),
            &prior_samples,
            7,
        ))
    };
    let prior_p50 = prior_sum.as_ref().map(|s| s.p50_ms);

    let (comparable, reason) = if let Some(ref r) = recent_sum.incomparable_reason {
        (false, Some(r.clone()))
    } else if prior_sum.is_none() {
        (false, Some(REASON_NO_PRIOR.to_string()))
    } else if let Some(ref p) = prior_sum
        && let Some(ref pr) = p.incomparable_reason
    {
        (false, Some(pr.clone()))
    } else {
        let recent_hash = single_argv_hash(&recent_samples);
        let prior_hash = single_argv_hash(&prior_samples);
        if recent_hash.is_none() || prior_hash.is_none() || recent_hash != prior_hash {
            (false, Some(REASON_WORKLOAD_MISMATCH.to_string()))
        } else if prior_p50 == Some(0) {
            (false, Some(REASON_ZERO_BASELINE.to_string()))
        } else {
            (true, None)
        }
    };

    let n = recent_sum.runs;
    let recent_p50 = recent_sum.p50_ms;
    let prior_p50_val = prior_p50.unwrap_or(0);

    let sentence = if comparable {
        let prior = prior_p50_val;
        let delta_pct = if prior > 0 {
            ((recent_p50 - prior) as f64 / prior as f64) * 100.0
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
        let across = if across_repos { " across repos" } else { "" };
        format!(
            "`{command}` p50 {recent_p50} ms over {n} run(s) this week{across}, {direction} {delta_pct:.0}% vs the prior week ({prior} ms)."
        )
    } else if reason.as_deref() == Some(REASON_NO_PRIOR) {
        let across = if across_repos { " across repos" } else { "" };
        format!(
            "`{command}` p50 {recent_p50} ms over {n} run(s) in the last 7 days{across}; no prior-week baseline yet."
        )
    } else {
        incomparable_sentence(
            command,
            recent_p50,
            n,
            prior_p50_val,
            reason.as_deref().unwrap_or("unknown"),
        )
    };

    ExplainReport {
        sentence,
        command: command.to_string(),
        runs: n,
        comparable,
        p50_ms: Some(recent_p50),
        prior_p50_ms: prior_p50,
        incomparable_reason: reason,
    }
}

/// JSON object for `data` on `timings --explain --json`.
pub fn explain_report_json(report: &ExplainReport) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    map.insert(
        "explain".to_string(),
        serde_json::Value::String(report.sentence.clone()),
    );
    map.insert(
        "command".to_string(),
        serde_json::Value::String(report.command.clone()),
    );
    map.insert("runs".to_string(), serde_json::Value::from(report.runs));
    map.insert(
        "comparable".to_string(),
        serde_json::Value::Bool(report.comparable),
    );
    if let Some(v) = report.p50_ms {
        map.insert("p50_ms".to_string(), serde_json::Value::from(v));
    }
    if let Some(v) = report.prior_p50_ms {
        map.insert("prior_p50_ms".to_string(), serde_json::Value::from(v));
    }
    if let Some(ref r) = report.incomparable_reason {
        map.insert(
            "incomparable_reason".to_string(),
            serde_json::Value::String(r.clone()),
        );
    }
    serde_json::Value::Object(map)
}

/// Nearest-rank percentile for a pre-sorted non-empty slice. Empty → 0.
pub fn percentile_sorted(sorted: &[i64], pct: u8) -> i64 {
    if sorted.is_empty() {
        return 0;
    }
    let rank = ((pct as f64 / 100.0) * (sorted.len() as f64 - 1.0)).round() as usize;
    sorted[rank.min(sorted.len() - 1)]
}

/// Read a single outer row by run_id (for tests / association checks).
pub fn get_outer_by_run_id(conn: &Connection, run_id: &str) -> Result<Option<TimingRow>> {
    let row = conn
        .query_row(
            "SELECT run_id, ts_utc, command, duration_ms, exit_code,
                    repo_size_bytes, argv_hash, ledger_tx_id,
                    parent_span_id, span_name, notes
             FROM command_timings
             WHERE run_id = ?1 AND span_name IS NULL
             LIMIT 1",
            params![run_id],
            |row| {
                Ok(TimingRow {
                    run_id: row.get(0)?,
                    ts_utc: row.get(1)?,
                    command: row.get(2)?,
                    duration_ms: row.get(3)?,
                    exit_code: row.get(4)?,
                    repo_size_bytes: row.get(5)?,
                    argv_hash: row.get(6)?,
                    ledger_tx_id: row.get(7)?,
                    parent_span_id: row.get(8)?,
                    span_name: row.get(9)?,
                    notes: row.get(10)?,
                })
            },
        )
        .optional()
        .into_diagnostic()?;
    Ok(row)
}

// ── Opt-out (user config, not per-repo DB) ──────────────────────────────────

/// Write `self_timing = false|true` to `~/.ledgerful/config.toml`.
pub fn set_self_timing_enabled(enabled: bool) -> Result<()> {
    let config_dir = crate::state::rollup::user_config_dir()?;
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

    doc.as_table_mut()
        .insert("self_timing", toml_edit::value(enabled));

    std::fs::write(&config_path, doc.to_string()).into_diagnostic()?;
    Ok(())
}

/// Default-on: absent key or parse failure means enabled.
pub fn is_self_timing_enabled() -> bool {
    let Ok(config_dir) = crate::state::rollup::user_config_dir() else {
        return true;
    };
    let config_path = config_dir.join("config.toml");
    if !config_path.exists() {
        return true;
    }
    let Ok(content) = std::fs::read_to_string(&config_path) else {
        return true;
    };
    let Ok(doc) = content.parse::<toml_edit::DocumentMut>() else {
        return true;
    };
    match doc.get("self_timing") {
        Some(item) => item.as_bool().unwrap_or(true),
        None => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::migrations::get_migrations;
    use rusqlite::Connection;

    fn setup() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        get_migrations().to_latest(&mut conn).unwrap();
        conn
    }

    fn sample_outer(run_id: &str, command: &str, duration_ms: i64) -> TimingRow {
        TimingRow {
            run_id: run_id.to_string(),
            ts_utc: "2026-07-19T12:00:00.000Z".to_string(),
            command: command.to_string(),
            duration_ms,
            exit_code: 0,
            repo_size_bytes: None,
            argv_hash: Some("abc".to_string()),
            ledger_tx_id: None,
            parent_span_id: None,
            span_name: None,
            notes: None,
        }
    }

    fn sample_inner(run_id: &str, command: &str, span: &str, duration_ms: i64) -> TimingRow {
        TimingRow {
            run_id: run_id.to_string(),
            ts_utc: "2026-07-19T12:00:00.000Z".to_string(),
            command: command.to_string(),
            duration_ms,
            exit_code: 0,
            repo_size_bytes: None,
            argv_hash: None,
            ledger_tx_id: None,
            parent_span_id: Some("parent-1".to_string()),
            span_name: Some(span.to_string()),
            notes: None,
        }
    }

    #[test]
    fn batch_insert_round_trip() {
        let mut conn = setup();
        let rows = vec![
            sample_outer("r1", "verify", 100),
            sample_inner("r1", "verify", "run_tests", 80),
        ];
        let n = insert_timing_batch(&mut conn, &rows).unwrap();
        assert_eq!(n, 2);

        let outer = query_timings(
            &conn,
            &TimingQuery {
                outer_only: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(outer.len(), 1);
        assert_eq!(outer[0].command, "verify");
        assert!(outer[0].repo_size_bytes.is_none());

        let inner = query_timings(
            &conn,
            &TimingQuery {
                inner_only: true,
                outer_only: false,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(inner.len(), 1);
        assert_eq!(inner[0].span_name.as_deref(), Some("run_tests"));
    }

    #[test]
    fn prune_outer_and_inner_separately() {
        let mut conn = setup();
        // Old outer
        let mut old_outer = sample_outer("old", "scan", 50);
        old_outer.ts_utc = "2020-01-01T00:00:00.000Z".to_string();
        // Old inner
        let mut old_inner = sample_inner("old", "scan", "walk", 10);
        old_inner.ts_utc = "2020-01-01T00:00:00.000Z".to_string();
        // Recent outer must stay inside the 30-day prune window vs SQLite `now`.
        let mut recent = sample_outer("new", "scan", 40);
        recent.ts_utc = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);

        insert_timing_batch(&mut conn, &[old_outer, old_inner, recent]).unwrap();

        let pruned_outer = prune_timings(&conn, 30, false).unwrap();
        assert_eq!(pruned_outer, 1);
        assert_eq!(count_timings(&conn).unwrap(), 2);

        let pruned_inner = prune_timings(&conn, 30, true).unwrap();
        assert_eq!(pruned_inner, 1);
        assert_eq!(count_timings(&conn).unwrap(), 1);
    }

    #[test]
    fn empty_batch_is_noop() {
        let mut conn = setup();
        assert_eq!(insert_timing_batch(&mut conn, &[]).unwrap(), 0);
    }

    #[test]
    fn summarize_outer_percentiles() {
        let mut conn = setup();
        let mut rows = Vec::new();
        for (i, d) in [10i64, 20, 30, 40, 50, 60, 70, 80, 90, 100]
            .into_iter()
            .enumerate()
        {
            let mut r = sample_outer(&format!("r{i}"), "verify", d);
            r.ts_utc = format!("2026-07-19T12:00:{i:02}.000Z");
            rows.push(r);
        }
        insert_timing_batch(&mut conn, &rows).unwrap();
        let summaries = summarize_outer(&conn, None, Some(5)).unwrap();
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].runs, 10);
        assert_eq!(summaries[0].total_ms, 550);
        assert!(summaries[0].p50_ms >= 40 && summaries[0].p50_ms <= 60);
        assert!(summaries[0].comparable);
        assert_eq!(summaries[0].nonzero_exit_runs, 0);
        let json = serde_json::to_value(&summaries[0]).unwrap();
        assert_eq!(json["comparable"], true);
    }

    fn sample_with(
        _run_id: &str,
        _command: &str,
        duration_ms: i64,
        exit_code: i32,
        argv_hash: Option<&str>,
    ) -> TimingSample {
        TimingSample {
            duration_ms,
            exit_code,
            argv_hash: argv_hash.map(str::to_string),
        }
    }

    #[test]
    fn mixed_hash_is_not_comparable() {
        let samples = vec![
            sample_with("a", "hotspots", 100, 0, Some("aaa")),
            sample_with("b", "hotspots", 110, 0, Some("aaa")),
            sample_with("c", "hotspots", 120, 0, Some("aaa")),
            sample_with("d", "hotspots", 200, 0, Some("bbb")),
            sample_with("e", "hotspots", 210, 0, Some("bbb")),
        ];
        let s = summarize_from_samples("hotspots".into(), &samples, 30);
        assert!(!s.comparable);
        assert_eq!(s.workload_count, 2);
        assert_eq!(
            s.incomparable_reason.as_deref(),
            Some(REASON_MIXED_WORKLOADS)
        );
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains("\"comparable\":false"));
    }

    #[test]
    fn nonzero_exit_excluded_from_percentiles() {
        let samples = vec![
            sample_with("a", "verify", 10, 0, Some("h")),
            sample_with("b", "verify", 20, 0, Some("h")),
            sample_with("c", "verify", 30, 0, Some("h")),
            sample_with("d", "verify", 40, 0, Some("h")),
            sample_with("e", "verify", 50, 0, Some("h")),
            sample_with("f", "verify", 9_000_000, 1, Some("h")),
        ];
        let s = summarize_from_samples("verify".into(), &samples, 30);
        assert_eq!(s.runs, 5);
        assert_eq!(s.nonzero_exit_runs, 1);
        assert_eq!(s.total_ms, 150);
        assert!(s.comparable);
    }

    #[test]
    fn uniform_zero_ms_is_single_and_comparable() {
        let samples: Vec<TimingSample> = (0..8)
            .map(|i| sample_with(&format!("z{i}"), "fast", 0, 0, Some("h")))
            .collect();
        let s = summarize_from_samples("fast".into(), &samples, 7);
        assert!(s.comparable);
        assert_eq!(s.duration_spread.as_deref(), Some("single"));
        assert_eq!(s.p50_ms, 0);
        assert_eq!(s.p95_ms, 0);
    }

    #[test]
    fn duration_spread_mixed_same_hash() {
        let samples = vec![
            sample_with("a", "hotspots", 200, 0, Some("h")),
            sample_with("b", "hotspots", 210, 0, Some("h")),
            sample_with("c", "hotspots", 220, 0, Some("h")),
            sample_with("d", "hotspots", 230, 0, Some("h")),
            sample_with("e", "hotspots", 450_000, 0, Some("h")),
        ];
        let s = summarize_from_samples("hotspots".into(), &samples, 30);
        assert!(!s.comparable);
        assert_eq!(
            s.incomparable_reason.as_deref(),
            Some(REASON_MIXED_DURATIONS)
        );
    }

    #[test]
    fn all_nonzero_exit_is_no_success() {
        let samples = vec![
            sample_with("a", "x", 10, 1, Some("h")),
            sample_with("b", "x", 20, 2, Some("h")),
        ];
        let s = summarize_from_samples("x".into(), &samples, 30);
        assert!(!s.comparable);
        assert_eq!(s.runs, 0);
        assert_eq!(s.incomparable_reason.as_deref(), Some(REASON_NO_SUCCESS));
        assert!(s.duration_spread.is_none());
    }

    #[test]
    fn explain_refuses_percent_on_duration_mix() {
        let mut recent = Vec::new();
        for i in 0..6 {
            let mut r = sample_outer(
                &format!("r{i}"),
                "hotspots",
                if i == 5 { 400_000 } else { 200 },
            );
            r.ts_utc = "2026-09-12T00:00:00.000Z".into();
            r.argv_hash = Some("h".into());
            recent.push(r);
        }
        let mut prior = Vec::new();
        for i in 0..6 {
            let mut r = sample_outer(&format!("p{i}"), "hotspots", 270);
            r.ts_utc = "2026-09-04T00:00:00.000Z".into();
            r.argv_hash = Some("h".into());
            prior.push(r);
        }
        let mut pool = prior.clone();
        pool.extend(recent.clone());
        let report = explain_command("hotspots", &recent, &pool, false);
        assert!(!report.comparable);
        assert!(report.sentence.contains("not comparable"));
        assert!(!report.sentence.contains("up "));
        assert!(!report.sentence.contains("down "));
        assert!(report.sentence.contains("prior week"));
        assert!(report.sentence.matches('.').count() <= 2);
    }

    #[test]
    fn explain_no_recent_json_has_comparable_false() {
        let report = explain_command("hotspots", &[], &[], false);
        assert!(!report.comparable);
        assert_eq!(
            report.incomparable_reason.as_deref(),
            Some(REASON_NO_RECENT)
        );
        let v = explain_report_json(&report);
        assert_eq!(v["comparable"], false);
        assert_eq!(v["incomparable_reason"], REASON_NO_RECENT);
    }

    #[test]
    fn table_exists_after_migration() {
        let conn = setup();
        assert!(table_exists(&conn).unwrap());
    }
}
