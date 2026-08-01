//! Team sync status — readiness, next-action, bounded target probe (0113).
//!
//! All `dir://` path work uses [`crate::sync::transport::SyncTarget::parse`]
//! (Windows `dir:///C:/…` form). Never prompts for the team secret.

use super::readiness::{
    InboxOutboxScan, ReadinessReport, TARGET_REACHABLE_TIMEOUT, TargetReachable, collect_readiness,
    count_inbox_outbox_bounded,
};
use crate::state::storage::StorageManager;
use crate::sync::transport::SyncTarget;
use miette::{Result, miette};
use rusqlite::OptionalExtension;
use std::io::Write;

/// Show team sync status (CLI-first; peer list from local trust store).
pub fn handle(json: bool) -> Result<()> {
    let layout = crate::commands::helpers::get_layout()?;
    let config = crate::config::load::load_config(&layout)?;

    let report = collect_readiness(&layout, &config)?;

    let storage = StorageManager::init_with_layout(&layout)?;
    let conn = storage.get_connection();

    let (last_extract_hlc, last_apply_hlc): (Option<String>, Option<String>) = conn
        .query_row(
            "SELECT last_extract_hlc, last_apply_hlc FROM sync_state WHERE id = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(|e| miette!("Failed to query sync_state: {e}"))?
        .unwrap_or((None, None));

    // Inbox/outbox only via SyncTarget::parse (never naive strip_prefix).
    // Scan shared root only when target_reachable==Yes; hang-bounded.
    let scan = if let Some(ref did) = report.device_id
        && report.target_reachable.is_reachable()
        && report.target_parse_ok
        && let Ok(SyncTarget::Dir(base)) = SyncTarget::parse(&report.target)
    {
        count_inbox_outbox_bounded(base, did.clone(), TARGET_REACHABLE_TIMEOUT)
    } else {
        InboxOutboxScan {
            inbox: 0,
            outbox: 0,
            last_bundle: None,
            note: None,
        }
    };
    let last_bundle = scan
        .last_bundle
        .clone()
        .unwrap_or_else(|| "None".to_string());

    if json {
        emit_status_json(
            &report,
            &config.sync.schedule,
            last_extract_hlc.as_deref(),
            last_apply_hlc.as_deref(),
            scan.inbox,
            scan.outbox,
            &last_bundle,
        )?;
        return Ok(());
    }

    let device_display = report
        .device_id
        .clone()
        .unwrap_or_else(|| "not set (run sync init)".to_string());
    let target_display = if report.target.trim().is_empty() {
        "(empty)".to_string()
    } else {
        report.target.clone()
    };

    println!("Team Sync Status [Available — opt-in shared-folder v1]");
    println!("  Enabled:        {}", report.enabled);
    println!(
        "  Initialized:    {} (device.key + device.pub + SoT device_id)",
        if report.initialized { "yes" } else { "no" }
    );
    println!("  Device ID (SoT): {device_display}");
    println!("  Target:         {target_display}");
    println!(
        "  Target reachable: {}",
        reachable_label(report.target_reachable)
    );
    println!(
        "  Schedule:       {} (display-only; not auto-installed)",
        config.sync.schedule
    );
    println!(
        "  Last Extract:   {}",
        last_extract_hlc.unwrap_or_else(|| "Never".to_string())
    );
    println!(
        "  Last Apply:     {}",
        last_apply_hlc.unwrap_or_else(|| "Never".to_string())
    );
    if let Some(ref note) = scan.note {
        println!("  Outbox Count:   {note}");
        println!("  Inbox Count:    {note}");
        println!("  Last Received:  {note}");
    } else {
        println!("  Outbox Count:   {}", scan.outbox);
        println!("  Inbox Count:    {}", scan.inbox);
        println!("  Last Received:  {last_bundle}");
    }
    match report.quarantine_note.as_deref() {
        Some(note) => println!("  Quarantined (this device): {note}"),
        None => println!("  Quarantined (this device): {}", report.quarantine_count),
    }

    // Peer trust store — do not mask list errors as "0 peers".
    match (report.peer_count, report.peers_error.as_ref()) {
        (_, Some(e)) => println!("  Peers:          error: {e}"),
        (Some(0), None) => {
            println!("  Peers:          0 (pair with `ledgerful sync pair` / accept invite)");
        }
        (Some(n), None) => {
            // Re-list for ids (sorted by list_peers).
            let sync_dir = layout.state_dir.join("sync");
            match crate::sync::peers::list_peers(sync_dir.as_std_path()) {
                Ok(peers) => {
                    println!("  Peers:          {} ({})", n, peers.join(", "));
                }
                Err(e) => println!("  Peers:          error: {e}"),
            }
        }
        (None, None) => println!("  Peers:          unknown"),
    }

    println!("  Readiness:      {}", report.readiness.as_str());
    println!("  Next:           {}", report.next_action);

    Ok(())
}

fn reachable_label(r: TargetReachable) -> &'static str {
    r.as_str()
}

fn emit_status_json(
    report: &ReadinessReport,
    schedule: &str,
    last_extract: Option<&str>,
    last_apply: Option<&str>,
    inbox_count: u64,
    outbox_count: u64,
    last_bundle: &str,
) -> Result<()> {
    let mut value = report.to_json_value();
    if let Some(obj) = value.as_object_mut() {
        obj.insert("schedule".into(), serde_json::json!(schedule));
        obj.insert("lastExtractHlc".into(), serde_json::json!(last_extract));
        obj.insert("lastApplyHlc".into(), serde_json::json!(last_apply));
        obj.insert("inboxCount".into(), serde_json::json!(inbox_count));
        obj.insert("outboxCount".into(), serde_json::json!(outbox_count));
        obj.insert("lastReceived".into(), serde_json::json!(last_bundle));
    }
    let mut stdout = std::io::stdout().lock();
    serde_json::to_writer(&mut stdout, &value)
        .map_err(|e| miette!("Failed to write status JSON: {e}"))?;
    stdout
        .write_all(b"\n")
        .map_err(|e| miette!("Failed to write status JSON newline: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn status_module_never_prompts_for_secret() {
        let src = include_str!("status.rs");
        let prod = src.split("#[cfg(test)]").next().unwrap_or(src);
        assert!(
            !prod.contains("rpassword::")
                && !prod.contains("prompt_password")
                && !prod.contains("read_password"),
            "status production code must never prompt for secret"
        );
        // No naive strip_prefix for dir:// (must use SyncTarget::parse).
        assert!(!prod.contains("strip_prefix(\"dir://\")"));
    }
}
