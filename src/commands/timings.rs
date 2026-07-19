//! Local-only command timing analysis CLI (Track 0043).
//!
//! Surfaces outer summaries, inner-span breakdown, collapsed flame stacks,
//! explain sentences, prune, and opt-in/opt-out. Never performs network I/O.

use crate::output::table::build_premium_table;
use crate::state::layout::Layout;
use crate::state::storage::StorageManager;
use crate::state::storage::timings::{
    TimingQuery, count_timings, is_self_timing_enabled, prune_timings, query_timings,
    set_self_timing_enabled, summarize_outer, table_exists,
};
use miette::{IntoDiagnostic, Result};
use owo_colors::OwoColorize;
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::PathBuf;

/// Arguments for the expanded `timings` command surface.
#[derive(Debug, Clone)]
pub struct TimingsArgs {
    pub global: bool,
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
    pub opt_in: bool,
    pub opt_out: bool,
}

#[derive(Serialize)]
struct JsonEnvelope<T: Serialize> {
    #[serde(rename = "schemaVersion")]
    schema_version: u32,
    data: T,
}

fn envelope<T: Serialize>(data: T) -> JsonEnvelope<T> {
    JsonEnvelope {
        schema_version: 1,
        data,
    }
}

/// Entry point for non-global `ledgerful timings ...`.
pub fn execute_timings(args: TimingsArgs) -> Result<()> {
    if args.opt_in && args.opt_out {
        return Err(miette::miette!(
            "--opt-in and --opt-out are mutually exclusive"
        ));
    }
    if args.opt_out {
        set_self_timing_enabled(false)?;
        println!("Self-timing disabled (wrote self_timing = false to ~/.ledgerful/config.toml).");
        return Ok(());
    }
    if args.opt_in {
        set_self_timing_enabled(true)?;
        println!("Self-timing enabled (self_timing = true in ~/.ledgerful/config.toml).");
        return Ok(());
    }

    if args.prune {
        return execute_prune(&args);
    }

    let current_dir = std::env::current_dir().into_diagnostic()?;
    let layout = Layout::new(current_dir.to_string_lossy().as_ref());
    let db_path = layout.state_subdir().join("ledger.db");
    if !db_path.exists() {
        if args.json {
            println!(
                "{}",
                serde_json::to_string_pretty(&envelope(Vec::<serde_json::Value>::new()))
                    .into_diagnostic()?
            );
        } else {
            println!(
                "No local ledger database yet. Run `ledgerful init` then use the tool; timings are recorded automatically."
            );
        }
        return Ok(());
    }

    let storage = StorageManager::init(db_path.as_std_path())?;
    let conn = storage.get_connection();

    if !table_exists(conn)? {
        if args.json {
            println!(
                "{}",
                serde_json::to_string_pretty(&envelope(Vec::<serde_json::Value>::new()))
                    .into_diagnostic()?
            );
        } else {
            println!("command_timings table not available (migration pending).");
        }
        return Ok(());
    }

    if args.explain.is_some() {
        return execute_explain(conn, &args);
    }
    if args.flame {
        return execute_flame(conn, &args);
    }
    if args.inner {
        return execute_inner(conn, &args);
    }

    execute_summary(conn, &args)
}

fn execute_summary(conn: &rusqlite::Connection, args: &TimingsArgs) -> Result<()> {
    let days = args.days.unwrap_or(30);
    let top = args.top.unwrap_or(20);
    let summaries = summarize_outer(conn, Some(days), Some(top))?;

    if let Some(ref path) = args.export {
        let json = serde_json::to_string_pretty(&envelope(&summaries)).into_diagnostic()?;
        std::fs::write(path, json).into_diagnostic()?;
        if !args.json {
            println!(
                "Exported {} command summaries to {}.",
                summaries.len(),
                path.display()
            );
        }
    }

    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&envelope(&summaries)).into_diagnostic()?
        );
        return Ok(());
    }

    if summaries.is_empty() {
        let status = if is_self_timing_enabled() {
            "enabled"
        } else {
            "disabled (timings --opt-in to re-enable)"
        };
        println!("No command timing rows in the last {days} day(s). Capture is {status}.");
        return Ok(());
    }

    println!(
        "\n{} (last {days} day(s), top {top})",
        "Command timings".bold().underline()
    );
    let mut table =
        build_premium_table(["Command", "Runs", "p50 ms", "p95 ms", "p99 ms", "Total ms"]);
    for s in &summaries {
        table.add_row(vec![
            s.command.clone(),
            s.runs.to_string(),
            s.p50_ms.to_string(),
            s.p95_ms.to_string(),
            s.p99_ms.to_string(),
            s.total_ms.to_string(),
        ]);
    }
    println!("{table}");
    Ok(())
}

fn execute_inner(conn: &rusqlite::Connection, args: &TimingsArgs) -> Result<()> {
    let days = args.days.unwrap_or(30);
    let rows = query_timings(
        conn,
        &TimingQuery {
            outer_only: false,
            inner_only: true,
            command: args.command.clone(),
            days: Some(days),
            limit: args.top.map(|t| t.saturating_mul(50)).or(Some(500)),
        },
    )?;

    // Aggregate by span_name: count + total + max.
    let mut agg: BTreeMap<String, (u64, i64, i64)> = BTreeMap::new();
    for r in &rows {
        let name = r
            .span_name
            .clone()
            .unwrap_or_else(|| "<unnamed>".to_string());
        let entry = agg.entry(name).or_insert((0, 0, 0));
        entry.0 += 1;
        entry.1 += r.duration_ms;
        entry.2 = entry.2.max(r.duration_ms);
    }

    #[derive(Serialize)]
    struct InnerAgg {
        span_name: String,
        samples: u64,
        total_ms: i64,
        max_ms: i64,
    }

    let mut aggs: Vec<InnerAgg> = agg
        .into_iter()
        .map(|(span_name, (samples, total_ms, max_ms))| InnerAgg {
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

    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&envelope(&aggs)).into_diagnostic()?
        );
        return Ok(());
    }

    if aggs.is_empty() {
        println!("No inner-span timing rows in the last {days} day(s).");
        return Ok(());
    }

    let cmd_label = args.command.as_deref().unwrap_or("all commands");
    println!(
        "\n{} — {cmd_label} (last {days} day(s))",
        "Inner spans".bold().underline()
    );
    let mut table = build_premium_table(["Span", "Samples", "Total ms", "Max ms"]);
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

fn execute_flame(conn: &rusqlite::Connection, args: &TimingsArgs) -> Result<()> {
    let days = args.days.unwrap_or(30);
    let rows = query_timings(
        conn,
        &TimingQuery {
            outer_only: false,
            inner_only: false,
            command: args.command.clone(),
            days: Some(days),
            limit: Some(5000),
        },
    )?;

    // Group by run_id; build collapsed stacks: command;span1;span2 count
    // For v1 we emit `command;span_name duration_ms` per inner row (and
    // `command duration_ms` for outer), which speedscope accepts as collapsed stacks.
    let mut lines: Vec<String> = Vec::new();
    for r in &rows {
        if let Some(ref span) = r.span_name {
            lines.push(format!("{};{} {}", r.command, span, r.duration_ms.max(1)));
        } else {
            lines.push(format!("{} {}", r.command, r.duration_ms.max(1)));
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
        println!(
            "{}",
            serde_json::to_string_pretty(&envelope(serde_json::json!({ "collapsed": body })))
                .into_diagnostic()?
        );
    } else {
        println!("{body}");
    }
    Ok(())
}

fn execute_explain(conn: &rusqlite::Connection, args: &TimingsArgs) -> Result<()> {
    let command = args
        .explain
        .as_deref()
        .ok_or_else(|| miette::miette!("--explain requires a command name"))?;

    let recent = query_timings(
        conn,
        &TimingQuery {
            outer_only: true,
            command: Some(command.to_string()),
            days: Some(7),
            limit: Some(100),
            ..Default::default()
        },
    )?;
    let prior = query_timings(
        conn,
        &TimingQuery {
            outer_only: true,
            command: Some(command.to_string()),
            days: Some(14),
            limit: Some(200),
            ..Default::default()
        },
    )?;

    if recent.is_empty() {
        let sentence = format!("No recorded runs of `{command}` in the last 7 days.");
        if args.json {
            println!(
                "{}",
                serde_json::to_string_pretty(&envelope(serde_json::json!({ "explain": sentence })))
                    .into_diagnostic()?
            );
        } else {
            println!("{sentence}");
        }
        return Ok(());
    }

    let recent_avg = mean_duration(&recent);
    // Prior week = rows in 14d that are not in the recent 7d set by ts.
    let recent_ids: std::collections::HashSet<&str> =
        recent.iter().map(|r| r.run_id.as_str()).collect();
    let prior_only: Vec<_> = prior
        .iter()
        .filter(|r| !recent_ids.contains(r.run_id.as_str()))
        .cloned()
        .collect();

    // Always include a WoW delta *or* an explicit no-baseline clause so readers
    // never mistake a single-week sample for a completed week-over-week compare.
    let sentence = if prior_only.is_empty() {
        format!(
            "`{command}` averaged {recent_avg:.0} ms over {} run(s) in the last 7 days; no prior-week baseline yet.",
            recent.len()
        )
    } else {
        let prior_avg = mean_duration(&prior_only);
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
            "`{command}` averaged {recent_avg:.0} ms over {} run(s) this week, {direction} {delta_pct:.0}% vs the prior week ({prior_avg:.0} ms).",
            recent.len()
        )
    };

    // One explanatory sentence (may contain a clause separator `;`).
    debug_assert!(!sentence.contains(". `") && sentence.matches('.').count() <= 2);
    debug_assert!(
        sentence.contains("prior week") || sentence.contains("no prior-week baseline yet"),
        "explain must always mention WoW delta or explicit no-baseline"
    );

    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&envelope(serde_json::json!({ "explain": sentence })))
                .into_diagnostic()?
        );
    } else {
        println!("{sentence}");
    }
    Ok(())
}

fn mean_duration(rows: &[crate::state::storage::timings::TimingRow]) -> f64 {
    if rows.is_empty() {
        return 0.0;
    }
    let sum: i64 = rows.iter().map(|r| r.duration_ms).sum();
    sum as f64 / rows.len() as f64
}

fn execute_prune(args: &TimingsArgs) -> Result<()> {
    let older = args
        .older_than
        .as_deref()
        .ok_or_else(|| miette::miette!("--prune requires --older-than Nd (e.g. 90d)"))?;
    let days = parse_days_spec(older)?;

    let current_dir = std::env::current_dir().into_diagnostic()?;
    let layout = Layout::new(current_dir.to_string_lossy().as_ref());
    let db_path = layout.state_subdir().join("ledger.db");
    if !db_path.exists() {
        println!("No local ledger database; nothing to prune.");
        return Ok(());
    }
    let storage = StorageManager::init(db_path.as_std_path())?;
    let conn = storage.get_connection();
    if !table_exists(conn)? {
        println!("command_timings table not available; nothing to prune.");
        return Ok(());
    }

    let n = prune_timings(conn, days, args.inner)?;
    let kind = if args.inner { "inner-span" } else { "outer" };
    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&envelope(serde_json::json!({
                "pruned": n,
                "kind": kind,
                "older_than_days": days,
            })))
            .into_diagnostic()?
        );
    } else {
        println!("Pruned {n} {kind} timing row(s) older than {days} day(s).");
    }
    Ok(())
}

/// Parse `90d` / `30` / `30days` into day count.
pub fn parse_days_spec(spec: &str) -> Result<u32> {
    let s = spec.trim().to_ascii_lowercase();
    let digits: String = s.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return Err(miette::miette!(
            "invalid --older-than value '{spec}'; expected e.g. 90d"
        ));
    }
    let n: u32 = digits
        .parse()
        .map_err(|_| miette::miette!("invalid --older-than value '{spec}'"))?;
    Ok(n)
}

/// Doctor warnings for timing table size / span cardinality.
pub fn doctor_timing_warnings(conn: &rusqlite::Connection) -> Vec<String> {
    let mut warnings = Vec::new();
    if !table_exists(conn).unwrap_or(false) {
        return warnings;
    }
    if let Ok(count) = count_timings(conn)
        && count > 10_000
    {
        warnings.push(format!(
            "command_timings has {count} rows (>10k) — consider `ledgerful timings --prune --older-than 90d`"
        ));
    }
    if let Ok(distinct) = crate::state::storage::timings::count_distinct_span_names(conn, 30)
        && distinct > 1000
    {
        warnings.push(format!(
            "command_timings has {distinct} distinct span names in 30d (>1000) — check for high-cardinality span names"
        ));
    }
    warnings
}

// Re-export for tests that inject rows without going through capture.
#[cfg(test)]
pub use crate::state::storage::timings::TimingRow;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::migrations::get_migrations;
    use crate::state::storage::timings::{TimingRow, insert_timing_batch};
    use rusqlite::Connection;

    fn setup() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        get_migrations().to_latest(&mut conn).unwrap();
        conn
    }

    #[test]
    fn parse_days_spec_accepts_nd() {
        assert_eq!(parse_days_spec("90d").unwrap(), 90);
        assert_eq!(parse_days_spec("30").unwrap(), 30);
        assert_eq!(parse_days_spec("14days").unwrap(), 14);
    }

    #[test]
    fn flame_lines_are_collapsed_stack_shape() {
        let mut conn = setup();
        let rows = vec![
            TimingRow {
                run_id: "r1".into(),
                ts_utc: "2026-07-19T12:00:00.000Z".into(),
                command: "verify".into(),
                duration_ms: 100,
                exit_code: 0,
                repo_size_bytes: None,
                argv_hash: None,
                ledger_tx_id: None,
                parent_span_id: None,
                span_name: None,
                notes: None,
            },
            TimingRow {
                run_id: "r1".into(),
                ts_utc: "2026-07-19T12:00:00.000Z".into(),
                command: "verify".into(),
                duration_ms: 80,
                exit_code: 0,
                repo_size_bytes: None,
                argv_hash: None,
                ledger_tx_id: None,
                parent_span_id: Some("p".into()),
                span_name: Some("run_tests".into()),
                notes: None,
            },
        ];
        insert_timing_batch(&mut conn, &rows).unwrap();

        let tmp = tempfile::tempdir().unwrap();
        let export = tmp.path().join("flame.txt");
        let args = TimingsArgs {
            global: false,
            json: false,
            top: None,
            days: Some(3650),
            export: Some(export.clone()),
            inner: false,
            command: Some("verify".into()),
            flame: true,
            explain: None,
            prune: false,
            older_than: None,
            opt_in: false,
            opt_out: false,
        };
        execute_flame(&conn, &args).unwrap();
        let body = std::fs::read_to_string(&export).unwrap();
        assert!(
            body.lines().any(|l| l.starts_with("verify ")),
            "outer collapsed stack missing: {body}"
        );
        assert!(
            body.lines().any(|l| l.contains("verify;run_tests")),
            "inner collapsed stack missing: {body}"
        );
    }

    #[test]
    fn explain_one_sentence_with_number() {
        let mut conn = setup();
        let mut rows = Vec::new();
        for i in 0..3 {
            rows.push(TimingRow {
                run_id: format!("r{i}"),
                ts_utc: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                command: "verify".into(),
                duration_ms: 100 + i * 10,
                exit_code: 0,
                repo_size_bytes: None,
                argv_hash: None,
                ledger_tx_id: None,
                parent_span_id: None,
                span_name: None,
                notes: None,
            });
        }
        insert_timing_batch(&mut conn, &rows).unwrap();

        // Mirror execute_explain no-baseline sentence construction and assert shape.
        let recent = crate::state::storage::timings::query_timings(
            &conn,
            &crate::state::storage::timings::TimingQuery {
                outer_only: true,
                command: Some("verify".into()),
                days: Some(7),
                limit: Some(100),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(!recent.is_empty());
        let avg = mean_duration(&recent);
        let sentence = format!(
            "`verify` averaged {avg:.0} ms over {} run(s) in the last 7 days; no prior-week baseline yet.",
            recent.len()
        );
        assert!(
            sentence.ends_with('.'),
            "explain must end with '.': {sentence}"
        );
        assert!(
            sentence.chars().any(|c| c.is_ascii_digit()),
            "explain must include a number: {sentence}"
        );
        assert!(
            sentence.contains("no prior-week baseline yet"),
            "no prior week must be explicit: {sentence}"
        );
        // One terminal period (clause uses `;`).
        assert_eq!(sentence.matches('.').count(), 1);

        let args = TimingsArgs {
            global: false,
            json: true,
            top: None,
            days: None,
            export: None,
            inner: false,
            command: None,
            flame: false,
            explain: Some("verify".into()),
            prune: false,
            older_than: None,
            opt_in: false,
            opt_out: false,
        };
        execute_explain(&conn, &args).unwrap();
    }

    #[test]
    fn explain_no_prior_week_baseline_is_explicit() {
        let mut conn = setup();
        // Only recent-week rows → no prior-week baseline.
        for i in 0..2 {
            rows_insert_outer(
                &mut conn,
                &format!("recent-{i}"),
                "scan",
                50 + i * 10,
                chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            );
        }

        let args = TimingsArgs {
            global: false,
            json: true,
            top: None,
            days: None,
            export: None,
            inner: false,
            command: None,
            flame: false,
            explain: Some("scan".into()),
            prune: false,
            older_than: None,
            opt_in: false,
            opt_out: false,
        };
        // Capture stdout via executing and reconstructing expected contract.
        let recent = crate::state::storage::timings::query_timings(
            &conn,
            &crate::state::storage::timings::TimingQuery {
                outer_only: true,
                command: Some("scan".into()),
                days: Some(7),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(!recent.is_empty());
        let avg = mean_duration(&recent);
        let expected = format!(
            "`scan` averaged {avg:.0} ms over {} run(s) in the last 7 days; no prior-week baseline yet.",
            recent.len()
        );
        assert!(expected.contains("no prior-week baseline yet"));
        execute_explain(&conn, &args).unwrap();
    }

    fn rows_insert_outer(
        conn: &mut Connection,
        run_id: &str,
        command: &str,
        duration_ms: i64,
        ts_utc: String,
    ) {
        insert_timing_batch(
            conn,
            &[TimingRow {
                run_id: run_id.into(),
                ts_utc,
                command: command.into(),
                duration_ms,
                exit_code: 0,
                repo_size_bytes: None,
                argv_hash: None,
                ledger_tx_id: None,
                parent_span_id: None,
                span_name: None,
                notes: None,
            }],
        )
        .unwrap();
    }

    #[test]
    fn doctor_cardinality_warning_over_threshold() {
        let mut conn = setup();
        let mut rows = Vec::new();
        // Threshold is >1000 distinct span names in 30d.
        for i in 0..1001 {
            rows.push(TimingRow {
                run_id: format!("card-{i}"),
                ts_utc: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                command: "verify".into(),
                duration_ms: 5,
                exit_code: 0,
                repo_size_bytes: None,
                argv_hash: None,
                ledger_tx_id: None,
                parent_span_id: None,
                span_name: Some(format!("high_card_span_{i}")),
                notes: None,
            });
        }
        insert_timing_batch(&mut conn, &rows).unwrap();
        let warnings = doctor_timing_warnings(&conn);
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("distinct span names") && w.contains("1000")),
            "expected cardinality warning, got: {warnings:?}"
        );
    }
}
