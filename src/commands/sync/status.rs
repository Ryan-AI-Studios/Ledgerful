use crate::state::layout::Layout;
use crate::state::storage::StorageManager;
use miette::{IntoDiagnostic, Result, miette};
use rusqlite::OptionalExtension;
use std::env;
use std::path::PathBuf;

pub fn handle() -> Result<()> {
    let current_dir = env::current_dir().into_diagnostic()?;
    let layout = Layout::new(current_dir.to_string_lossy().as_ref());
    let config = crate::config::load::load_config(&layout)?;

    let storage_path = layout.state_subdir().join("ledger.db");
    let storage = StorageManager::init(storage_path.as_std_path())?;
    let conn = storage.get_connection();

    let (last_extract_hlc, last_apply_hlc, device_id): (Option<String>, Option<String>, String) =
        conn.query_row(
            "SELECT last_extract_hlc, last_apply_hlc, device_id FROM sync_state WHERE id = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .map_err(|e| miette!("Failed to query sync_state: {}", e))?
        .unwrap_or((None, None, "unknown".to_string()));

    let sync_target = config.sync.target.clone();

    // Count inbox/outbox if target is a directory
    let mut inbox_count = 0;
    let mut outbox_count = 0;
    let mut last_bundle = String::from("None");

    if let Some(path_str) = sync_target.strip_prefix("dir://") {
        let base_path = PathBuf::from(path_str);

        let outbox_path = base_path.join("devices").join(&device_id);
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
                if entry.file_name() != device_id.as_str()
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

    println!("Sync Status:");
    println!("  Enabled:        {}", config.sync.enabled);
    println!("  Device ID:      {}", device_id);
    println!("  Target:         {}", sync_target);
    println!("  Schedule:       {}", config.sync.schedule);
    println!(
        "  Last Extract:   {}",
        last_extract_hlc.unwrap_or_else(|| "Never".to_string())
    );
    println!(
        "  Last Apply:     {}",
        last_apply_hlc.unwrap_or_else(|| "Never".to_string())
    );
    println!("  Outbox Count:   {}", outbox_count);
    println!("  Inbox Count:    {}", inbox_count);
    println!("  Last Received:  {}", last_bundle);

    // Peer list from known device keys or directory structure
    println!("  Peers:          (run `sync pair` to see paired devices)");

    Ok(())
}
