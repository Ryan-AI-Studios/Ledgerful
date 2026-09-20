use crate::state::layout::Layout;
use crate::state::storage::StorageManager;
use crate::sync::hlc::HLC;
use miette::{Result, miette};
use rusqlite::OptionalExtension;
use std::io::Write;
use std::str::FromStr;

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
        println!(
            "HLC watermarks are Hybrid Logical Clocks (`physical_ms-logical-node_id`), not wall-clock lag."
        );
        println!(
            "Extract is the last bundle this device published; Apply is the last bundle this device consumed."
        );
        if let Some(line) = human_compare_line(
            nonempty_hlc(extract_hlc.as_deref()),
            nonempty_hlc(apply_hlc.as_deref()),
        ) {
            println!("{line}");
        }
        let initialized = sync_initialized(&layout)?;
        let extract = nonempty_hlc(extract_hlc.as_deref());
        let apply = nonempty_hlc(apply_hlc.as_deref());
        println!(
            "Lag is unknown (`{}`). Next: {}",
            lag_reason(initialized, extract, apply),
            cursor_log_next_action(initialized)
        );
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

/// Emit matrix: omit when either watermark is missing; `incomparable` when
/// both present but unparseable; otherwise `Ord` on parsed HLCs.
pub(crate) fn watermark_compare(
    extract: Option<&str>,
    apply: Option<&str>,
) -> Option<&'static str> {
    let (Some(extract), Some(apply)) = (extract, apply) else {
        return None;
    };
    match (HLC::from_str(extract), HLC::from_str(apply)) {
        (Ok(eh), Ok(ah)) => Some(if eh > ah {
            "extractAhead"
        } else if ah > eh {
            "applyAhead"
        } else {
            "equal"
        }),
        _ => Some("incomparable"),
    }
}

fn human_compare_line(extract: Option<&str>, apply: Option<&str>) -> Option<&'static str> {
    match watermark_compare(extract, apply)? {
        "extractAhead" => Some("Extract is ahead of Apply."),
        "applyAhead" => Some("Apply is ahead of Extract."),
        "equal" => Some("Extract and Apply watermarks are equal."),
        _ => None,
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
    let mut envelope = serde_json::json!({
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
    if let Some(cmp) = watermark_compare(extract, apply) {
        envelope["watermarkCompare"] = serde_json::Value::String(cmp.to_string());
    }
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
    fn watermark_compare_omit_when_either_missing() {
        assert_eq!(watermark_compare(None, None), None);
        assert_eq!(watermark_compare(Some("1700000000000-0000-a"), None), None);
        assert_eq!(watermark_compare(None, Some("1700000000000-0000-a")), None);
    }

    #[test]
    fn watermark_compare_incomparable_when_both_present_unparseable() {
        assert_eq!(
            watermark_compare(Some("hlc-a"), Some("hlc-b")),
            Some("incomparable")
        );
    }

    #[test]
    fn watermark_compare_orders_parseable_hlcs() {
        assert_eq!(
            watermark_compare(Some("1700000000001-0000-a"), Some("1700000000000-0000-a")),
            Some("extractAhead")
        );
        assert_eq!(
            watermark_compare(Some("1700000000000-0000-a"), Some("1700000000001-0000-a")),
            Some("applyAhead")
        );
        assert_eq!(
            watermark_compare(Some("1700000000000-0000-a"), Some("1700000000000-0000-a")),
            Some("equal")
        );
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
