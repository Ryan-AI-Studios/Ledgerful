use crate::state::storage::StorageManager;
use miette::{Result, miette};
use rusqlite::OptionalExtension;
use std::path::PathBuf;

/// Show Experimental team sync status (CLI-first; peer list from local trust store).
pub fn handle() -> Result<()> {
    let layout = crate::commands::helpers::get_layout()?;
    let config = crate::config::load::load_config(&layout)?;

    let storage = StorageManager::init_with_layout(&layout)?;
    let conn = storage.get_connection();

    let (last_extract_hlc, last_apply_hlc, device_id): (
        Option<String>,
        Option<String>,
        Option<String>,
    ) = conn
        .query_row(
            "SELECT last_extract_hlc, last_apply_hlc, device_id FROM sync_state WHERE id = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .map_err(|e| miette!("Failed to query sync_state: {e}"))?
        .map(|(a, b, c)| (a, b, Some(c)))
        .unwrap_or((None, None, None));

    let key_path = layout.state_dir.join("sync").join("device.key");
    let pub_path = layout.state_dir.join("sync").join("device.pub");
    // Initialized only when keys AND a non-empty SoT device_id are present (0110 codex P2-02).
    let sot_ok = device_id
        .as_ref()
        .is_some_and(|id| !id.trim().is_empty() && id != "unknown");
    let initialized = key_path.exists() && pub_path.exists() && sot_ok;
    let device_display = device_id
        .clone()
        .filter(|id| !id.trim().is_empty())
        .unwrap_or_else(|| "not set (run sync init)".to_string());

    let sync_target = config.sync.target.clone();
    let target_display = if sync_target.trim().is_empty() {
        "(empty)".to_string()
    } else {
        sync_target.clone()
    };

    // Count inbox/outbox if target is a directory and we know our device_id.
    // Use the same bundle filter as transport (`.lfbundle` write + dual-read `gpg`).
    // Only count files that match `is_bundle_filename` — never directories (e.g. `.tmp`).
    let mut inbox_count = 0;
    let mut outbox_count = 0;
    // Use Option so max name is not biased by a sentinel like "None"
    // (HLC filenames start with digits and compare less than "None").
    let mut last_bundle_name: Option<String> = None;

    if let Some(ref did) = device_id
        && let Some(path_str) = sync_target.strip_prefix("dir://")
    {
        let base_path = PathBuf::from(path_str);

        let outbox_path = base_path.join("devices").join(did);
        if outbox_path.exists()
            && let Ok(entries) = std::fs::read_dir(outbox_path)
        {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_file()
                    && let Some(name) = path.file_name().and_then(|n| n.to_str())
                    && crate::sync::transport::is_bundle_filename(name)
                {
                    outbox_count += 1;
                }
            }
        }

        let devices_path = base_path.join("devices");
        if devices_path.exists()
            && let Ok(entries) = std::fs::read_dir(devices_path)
        {
            for entry in entries.flatten() {
                if entry.file_name() != did.as_str()
                    && let Ok(peer_entries) = std::fs::read_dir(entry.path())
                {
                    for peer_entry in peer_entries.flatten() {
                        let path = peer_entry.path();
                        if path.is_file()
                            && let Some(name) = path.file_name().and_then(|n| n.to_str())
                            && crate::sync::transport::is_bundle_filename(name)
                        {
                            inbox_count += 1;
                            match &last_bundle_name {
                                Some(cur) if name <= cur.as_str() => {}
                                _ => last_bundle_name = Some(name.to_string()),
                            }
                        }
                    }
                }
            }
        }
    }
    let last_bundle = last_bundle_name.unwrap_or_else(|| "None".to_string());

    println!("Team Sync Status [Experimental]");
    println!("  Enabled:        {}", config.sync.enabled);
    println!(
        "  Initialized:    {} (device.key + device.pub + SoT device_id)",
        if initialized { "yes" } else { "no" }
    );
    println!("  Device ID (SoT): {device_display}");
    println!("  Target:         {target_display}");
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
    println!("  Outbox Count:   {outbox_count}");
    println!("  Inbox Count:    {inbox_count}");
    println!("  Last Received:  {last_bundle}");

    // Peer trust store: `{state_dir}/sync/peers/*.pub` (0111).
    // Do not mask list errors as "0 peers" — surface the failure clearly.
    let sync_dir = layout.state_dir.join("sync");
    match crate::sync::peers::list_peers(sync_dir.as_std_path()) {
        Ok(peers) if peers.is_empty() => {
            println!("  Peers:          0 (pair with `ledgerful sync pair` / accept invite)");
        }
        Ok(peers) => {
            println!("  Peers:          {} ({})", peers.len(), peers.join(", "));
        }
        Err(e) => {
            println!("  Peers:          error: {e}");
        }
    }

    Ok(())
}
