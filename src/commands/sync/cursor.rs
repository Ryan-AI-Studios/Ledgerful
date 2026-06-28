use crate::state::layout::Layout;
use crate::state::storage::StorageManager;
use miette::{IntoDiagnostic, Result, miette};
use rusqlite::OptionalExtension;
use std::env;

pub fn handle(set: Option<String>) -> Result<()> {
    let current_dir = env::current_dir().into_diagnostic()?;
    let layout = Layout::new(current_dir.to_string_lossy().as_ref());

    let storage_path = layout.state_subdir().join("ledger.db");
    let storage = StorageManager::init(storage_path.as_std_path())?;
    let conn = storage.get_connection();

    if let Some(new_hlc) = set {
        // Set the cursor (manually override)
        conn.execute(
            "UPDATE sync_state SET last_extract_hlc = ?1 WHERE id = 1",
            [new_hlc.clone()],
        )
        .map_err(|e| miette!("Failed to update last_extract_hlc: {}", e))?;
        println!("Sync extract cursor updated to: {}", new_hlc);
    } else {
        // Print the cursor
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
            extract_hlc.unwrap_or_else(|| "None".to_string())
        );
        println!(
            "  Last Apply HLC:   {}",
            apply_hlc.unwrap_or_else(|| "None".to_string())
        );
    }

    Ok(())
}
