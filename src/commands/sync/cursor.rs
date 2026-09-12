use crate::state::layout::Layout;
use crate::state::storage::StorageManager;
use miette::{Result, miette};
use rusqlite::OptionalExtension;
use std::io::Write;

pub fn handle(set: Option<String>, json: bool) -> Result<()> {
    let layout = crate::commands::helpers::get_layout()?;

    if json {
        return emit_cursor_json(&layout);
    }

    let storage = StorageManager::init_with_layout(&layout)?;
    let conn = storage.get_connection();

    if let Some(new_hlc) = set {
        conn.execute(
            "UPDATE sync_state SET last_extract_hlc = ?1 WHERE id = 1",
            [new_hlc.clone()],
        )
        .map_err(|e| miette!("Failed to update last_extract_hlc: {}", e))?;
        println!("Sync extract cursor updated to: {}", new_hlc);
    } else {
        let (extract_hlc, apply_hlc): (Option<String>, Option<String>) = conn
            .query_row(
                "SELECT last_extract_hlc, last_apply_hlc FROM sync_state WHERE id = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(|e| miette!("Failed to query sync_state: {}", e))?
            .unwrap_or((None, None));

        println!("Sync Cursors:");
        println!(
            "  Last Extract HLC: {}",
            display_hlc(extract_hlc.as_deref())
        );
        println!("  Last Apply HLC:   {}", display_hlc(apply_hlc.as_deref()));
    }

    Ok(())
}

fn display_hlc(hlc: Option<&str>) -> &str {
    match hlc {
        Some(s) if !s.trim().is_empty() => s,
        _ => "Never",
    }
}

fn nonempty_hlc(hlc: Option<&str>) -> Option<&str> {
    hlc.filter(|s| !s.trim().is_empty())
}

pub(crate) fn lag_reason(
    initialized: bool,
    extract: Option<&str>,
    apply: Option<&str>,
) -> &'static str {
    if !initialized {
        "notInitialized"
    } else if nonempty_hlc(extract).is_none() || nonempty_hlc(apply).is_none() {
        "neverRun"
    } else {
        "hlcNotWallClock"
    }
}

pub(crate) fn cursor_log_next_action(initialized: bool) -> &'static str {
    if initialized {
        "ledgerful sync setup"
    } else {
        "ledgerful sync init"
    }
}

pub(crate) fn sync_initialized(layout: &Layout) -> Result<bool> {
    let storage = StorageManager::init_with_layout(layout)?;
    let conn = storage.get_connection();
    let device_id: Option<String> = conn
        .query_row("SELECT device_id FROM sync_state WHERE id = 1", [], |row| {
            row.get(0)
        })
        .optional()
        .map_err(|e| miette!("Failed to query sync_state: {e}"))?
        .filter(|id: &String| !id.trim().is_empty() && id != "unknown");
    let key_path = layout.state_dir.join("sync").join("device.key");
    let pub_path = layout.state_dir.join("sync").join("device.pub");
    Ok(key_path.exists() && pub_path.exists() && device_id.is_some())
}

fn emit_cursor_json(layout: &Layout) -> Result<()> {
    let initialized = sync_initialized(layout)?;
    let storage = StorageManager::init_with_layout(layout)?;
    let conn = storage.get_connection();
    let (extract_hlc, apply_hlc): (Option<String>, Option<String>) = conn
        .query_row(
            "SELECT last_extract_hlc, last_apply_hlc FROM sync_state WHERE id = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(|e| miette!("Failed to query sync_state: {}", e))?
        .unwrap_or((None, None));
    let extract = nonempty_hlc(extract_hlc.as_deref());
    let apply = nonempty_hlc(apply_hlc.as_deref());
    let envelope = serde_json::json!({
        "schemaVersion": 1,
        "initialized": initialized,
        "lastExtractHlc": extract,
        "lastApplyHlc": apply,
        "lag": {
            "status": "unknown",
            "reason": lag_reason(initialized, extract, apply),
        },
        "nextAction": cursor_log_next_action(initialized),
    });
    let mut stdout = std::io::stdout().lock();
    serde_json::to_writer(&mut stdout, &envelope)
        .map_err(|e| miette!("Failed to write cursor JSON: {e}"))?;
    stdout
        .write_all(b"\n")
        .map_err(|e| miette!("Failed to write cursor JSON newline: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lag_reason_not_initialized() {
        assert_eq!(lag_reason(false, None, None), "notInitialized");
        assert_eq!(lag_reason(false, Some("a"), Some("b")), "notInitialized");
    }

    #[test]
    fn lag_reason_never_run() {
        assert_eq!(lag_reason(true, None, None), "neverRun");
        assert_eq!(lag_reason(true, Some("a"), None), "neverRun");
        assert_eq!(lag_reason(true, None, Some("b")), "neverRun");
        assert_eq!(lag_reason(true, Some("  "), Some("b")), "neverRun");
    }

    #[test]
    fn lag_reason_hlc_not_wall_clock() {
        assert_eq!(lag_reason(true, Some("a"), Some("b")), "hlcNotWallClock");
    }

    #[test]
    fn cursor_read_path_does_not_probe_or_scan() {
        let src = include_str!("cursor.rs");
        let prod = src.split("#[cfg(test)]").next().unwrap_or(src);
        assert!(!prod.contains("collect_readiness"));
        assert!(!prod.contains("probe_target_reachable"));
        assert!(!prod.contains("execute_federate_scan"));
        assert!(!prod.contains("execute_federate_export"));
    }
}
