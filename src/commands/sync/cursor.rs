use crate::state::storage::StorageManager;
use miette::{Result, miette};
use rusqlite::OptionalExtension;

pub fn handle(set: Option<String>) -> Result<()> {
    let layout = crate::commands::helpers::get_layout()?;

    let storage = StorageManager::init_with_layout(&layout)?;
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
