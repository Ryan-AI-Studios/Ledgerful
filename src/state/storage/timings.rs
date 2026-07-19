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

/// Aggregate stats for one command (outer rows only).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandTimingSummary {
    pub command: String,
    pub runs: u64,
    pub p50_ms: i64,
    pub p95_ms: i64,
    pub p99_ms: i64,
    pub total_ms: i64,
}

/// Summarize outer timings grouped by command, ordered by total_ms DESC.
pub fn summarize_outer(
    conn: &Connection,
    days: Option<u32>,
    top: Option<u32>,
) -> Result<Vec<CommandTimingSummary>> {
    let mut sql = String::from(
        "SELECT command, duration_ms FROM command_timings
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
    let pairs: Vec<(String, i64)> = stmt
        .query_map(params_refs.as_slice(), |row| Ok((row.get(0)?, row.get(1)?)))
        .into_diagnostic()?
        .collect::<std::result::Result<Vec<_>, _>>()
        .into_diagnostic()?;

    let mut by_cmd: std::collections::BTreeMap<String, Vec<i64>> =
        std::collections::BTreeMap::new();
    for (cmd, dur) in pairs {
        by_cmd.entry(cmd).or_default().push(dur);
    }

    let mut summaries: Vec<CommandTimingSummary> = by_cmd
        .into_iter()
        .map(|(command, durs)| summarize_from_samples(command, &durs))
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

/// Build a summary from pooled duration samples (any source, e.g. multi-repo union).
///
/// Samples are sorted ascending before nearest-rank percentiles are computed.
/// Empty samples yield runs=0 and zeroed percentiles/total.
pub fn summarize_from_samples(command: String, durations: &[i64]) -> CommandTimingSummary {
    let mut durs = durations.to_vec();
    durs.sort_unstable();
    let runs = durs.len() as u64;
    let total_ms: i64 = durs.iter().sum();
    CommandTimingSummary {
        command,
        runs,
        p50_ms: percentile_sorted(&durs, 50),
        p95_ms: percentile_sorted(&durs, 95),
        p99_ms: percentile_sorted(&durs, 99),
        total_ms,
    }
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
        // Recent outer
        let recent = sample_outer("new", "scan", 40);

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
    }

    #[test]
    fn table_exists_after_migration() {
        let conn = setup();
        assert!(table_exists(&conn).unwrap());
    }
}
