use super::cursor::{cursor_log_next_action, sync_initialized};
use crate::sync::event_log::{SyncLogEvent, parse_event_line};
use miette::{Result, miette};
use std::fs::File;
use std::io::{BufRead, BufReader, Write};

pub fn handle(tail: Option<usize>, json: bool, failed: bool) -> Result<()> {
    let layout = crate::commands::helpers::get_layout()?;
    let initialized = sync_initialized(&layout)?;
    let log_path = layout.state_dir.join("sync").join("sync.log");
    let next_action = cursor_log_next_action(initialized);

    if !log_path.exists() {
        let state = if initialized {
            "noLog"
        } else {
            "neverInitialized"
        };
        if json {
            emit_log_json(&LogJson {
                log_state: state,
                path: log_path.as_str(),
                line_count: 0,
                skipped_lines: 0,
                lines: &[],
                events: &[],
                failed,
                next_action,
            })?;
            return Ok(());
        }
        println!("No sync log found at {log_path} ({state}). Next: {next_action}");
        return Ok(());
    }

    let file = match File::open(log_path.as_std_path()) {
        Ok(f) => f,
        Err(e) => {
            return unreadable_log(json, log_path.as_str(), next_action, e.to_string(), failed);
        }
    };
    let is_file = file.metadata().map(|m| m.is_file()).unwrap_or(false);
    if !is_file {
        return unreadable_log(
            json,
            log_path.as_str(),
            next_action,
            "sync.log is not a regular file".to_string(),
            failed,
        );
    }
    let reader = BufReader::new(file);
    let mut decoded: Vec<String> = Vec::new();
    let mut skipped: u64 = 0;
    for line in reader.lines() {
        match line {
            Ok(s) => decoded.push(s),
            Err(_) => skipped += 1,
        }
    }
    let line_count = decoded.len() as u64;

    let selected: Vec<String> = if failed {
        decoded
            .into_iter()
            .filter(|line| parse_event_line(line).is_some_and(|ev| !ev.ok))
            .collect()
    } else {
        decoded
    };

    let limit = tail.unwrap_or(20);
    let start = if selected.len() > limit {
        selected.len() - limit
    } else {
        0
    };
    let tailed = &selected[start..];
    let events: Vec<SyncLogEvent> = tailed.iter().filter_map(|l| parse_event_line(l)).collect();
    let log_state = if skipped > 0 { "partial" } else { "ok" };

    if json {
        emit_log_json(&LogJson {
            log_state,
            path: log_path.as_str(),
            line_count,
            skipped_lines: skipped,
            lines: tailed,
            events: &events,
            failed,
            next_action,
        })?;
        return Ok(());
    }

    println!("Recent Sync Logs ({log_path}):");
    for line in tailed {
        if let Some(ev) = parse_event_line(line) {
            match ev.bundle.as_deref() {
                Some(bundle) => println!("{} {} {} ok={}", ev.ts, ev.event, bundle, ev.ok),
                None => println!("{} {} ok={}", ev.ts, ev.event, ev.ok),
            }
        } else {
            println!("{}", line);
        }
    }

    Ok(())
}

fn unreadable_log(
    json: bool,
    path: &str,
    next_action: &str,
    err: String,
    failed: bool,
) -> Result<()> {
    if json {
        emit_log_json(&LogJson {
            log_state: "unreadable",
            path,
            line_count: 0,
            skipped_lines: 0,
            lines: &[],
            events: &[],
            failed,
            next_action,
        })?;
        crate::output::requested_exit::request_exit(1);
        return Err(miette!("Failed to open log file: {err}"));
    }
    Err(miette!("Failed to open log file: {err}"))
}

struct LogJson<'a> {
    log_state: &'a str,
    path: &'a str,
    line_count: u64,
    skipped_lines: u64,
    lines: &'a [String],
    events: &'a [SyncLogEvent],
    failed: bool,
    next_action: &'a str,
}

fn emit_log_json(args: &LogJson<'_>) -> Result<()> {
    let mut envelope = serde_json::json!({
        "schemaVersion": 1,
        "logState": args.log_state,
        "path": args.path,
        "lineCount": args.line_count,
        "skippedLines": args.skipped_lines,
        "lines": args.lines,
        "nextAction": args.next_action,
    });
    if !args.events.is_empty() {
        envelope["events"] = serde_json::to_value(args.events)
            .map_err(|e| miette!("Failed to serialize log events: {e}"))?;
    }
    if args.failed {
        envelope["failed"] = serde_json::Value::Bool(true);
    }
    let mut stdout = std::io::stdout().lock();
    serde_json::to_writer(&mut stdout, &envelope)
        .map_err(|e| miette!("Failed to write log JSON: {e}"))?;
    stdout
        .write_all(b"\n")
        .map_err(|e| miette!("Failed to write log JSON newline: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn log_read_path_does_not_probe_or_mutate() {
        let src = include_str!("log.rs");
        let prod = src.split("#[cfg(test)]").next().unwrap_or(src);
        assert!(!prod.contains("collect_readiness"));
        assert!(!prod.contains("probe_target_reachable"));
        assert!(!prod.contains("UPDATE sync_state"));
        assert!(!prod.contains("execute_federate_scan"));
        assert!(!prod.contains("map_while"));
        assert!(prod.contains("is_file"));
    }
}
