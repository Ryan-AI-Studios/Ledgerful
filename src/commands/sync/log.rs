use super::cursor::{cursor_log_next_action, sync_initialized};
use miette::{Result, miette};
use std::fs::File;
use std::io::{BufRead, BufReader, Write};

pub fn handle(tail: Option<usize>, json: bool) -> Result<()> {
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
            emit_log_json(state, log_path.as_str(), 0, 0, &[], next_action)?;
            return Ok(());
        }
        println!("No sync log found at {log_path} ({state}). Next: {next_action}");
        return Ok(());
    }

    let file = match File::open(log_path.as_std_path()) {
        Ok(f) => f,
        Err(e) => {
            if json {
                emit_log_json("unreadable", log_path.as_str(), 0, 0, &[], next_action)?;
                crate::output::requested_exit::request_exit(1);
                return Err(miette!("Failed to open log file: {e}"));
            }
            return Err(miette!("Failed to open log file: {e}"));
        }
    };
    let reader = BufReader::new(file);
    let mut decoded: Vec<String> = Vec::new();
    let mut skipped: u64 = 0;
    for line in reader.lines() {
        match line {
            Ok(s) => decoded.push(s),
            Err(_) => skipped += 1,
        }
    }

    let limit = tail.unwrap_or(20);
    let start = if decoded.len() > limit {
        decoded.len() - limit
    } else {
        0
    };
    let tailed = &decoded[start..];
    let log_state = if skipped > 0 { "partial" } else { "ok" };

    if json {
        emit_log_json(
            log_state,
            log_path.as_str(),
            decoded.len() as u64,
            skipped,
            tailed,
            next_action,
        )?;
        return Ok(());
    }

    println!("Recent Sync Logs ({log_path}):");
    for line in tailed {
        println!("{}", line);
    }

    Ok(())
}

fn emit_log_json(
    log_state: &str,
    path: &str,
    line_count: u64,
    skipped_lines: u64,
    lines: &[String],
    next_action: &str,
) -> Result<()> {
    let envelope = serde_json::json!({
        "schemaVersion": 1,
        "logState": log_state,
        "path": path,
        "lineCount": line_count,
        "skippedLines": skipped_lines,
        "lines": lines,
        "nextAction": next_action,
    });
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
    }
}
