use crate::state::storage::StorageManager;
use miette::{Result, miette};
use rusqlite::OptionalExtension;
use std::path::PathBuf;

/// Show Experimental team sync status (CLI-first; peers not available until 0111).
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
    let initialized = key_path.exists() || device_id.is_some();
    let device_display = device_id
        .clone()
        .unwrap_or_else(|| "not set (run sync init)".to_string());

    let sync_target = config.sync.target.clone();
    let target_display = if sync_target.trim().is_empty() {
        "(empty)".to_string()
    } else {
        sync_target.clone()
    };

    // Count inbox/outbox if target is a directory and we know our device_id.
    let mut inbox_count = 0;
    let mut outbox_count = 0;
    let mut last_bundle = String::from("None");

    if let Some(ref did) = device_id
        && let Some(path_str) = sync_target.strip_prefix("dir://")
    {
        let base_path = PathBuf::from(path_str);

        let outbox_path = base_path.join("devices").join(did);
        if outbox_path.exists()
            && let Ok(entries) = std::fs::read_dir(outbox_path)
        {
            outbox_count = entries.filter(|e| e.is_ok()).count();
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
                        if peer_entry
                            .path()
                            .extension()
                            .is_some_and(|ext| ext == "gpg")
                        {
                            inbox_count += 1;
                            let name = peer_entry.file_name().to_string_lossy().into_owned();
                            if name > last_bundle {
                                last_bundle = name;
                            }
                        }
                    }
                }
            }
        }
    }

    println!("Team Sync Status [Experimental]");
    println!("  Enabled:        {}", config.sync.enabled);
    println!(
        "  Initialized:    {} (keys or SoT device_id)",
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
    // Peers are not listed until 0111 peer store — do not claim pair lists devices.
    println!("  Peers:          not available (0) — pairing accept lands in track 0111");

    Ok(())
}
