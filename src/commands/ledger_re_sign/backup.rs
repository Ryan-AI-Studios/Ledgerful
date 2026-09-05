use miette::{Result, miette};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

/// Create a WAL-safe, integrity-checked backup of the ledger database.
///
/// Uses SQLite's Online Backup API (`rusqlite::backup::Backup`) over a live connection.
/// After the backup is written, we open it read-only and run `PRAGMA integrity_check`.
/// The operation aborts with an actionable error if the backup is corrupt.
pub(crate) fn backup_ledger_db(
    src_conn: &rusqlite::Connection,
    db_path: &Path,
) -> Result<std::path::PathBuf> {
    let timestamp = nanos_since_epoch();
    let base_name = db_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("ledger.db");
    let backup_path = db_path
        .parent()
        .unwrap_or(Path::new("."))
        .join(format!("{}.{}.bak", base_name, timestamp));

    // Online Backup API copies the live database into a new file.
    {
        let mut dst = rusqlite::Connection::open(&backup_path).map_err(|e| {
            miette!(
                "Failed to open backup database at {}: {}",
                backup_path.display(),
                e
            )
        })?;
        let backup = rusqlite::backup::Backup::new(src_conn, &mut dst)
            .map_err(|e| miette!("Failed to initialize SQLite online backup: {}", e))?;
        backup
            .step(-1)
            .map_err(|e| miette!("SQLite online backup failed: {}", e))?;
    }

    // Verify the backup is openable and passes integrity_check before any mutation.
    let integrity = verify_backup_integrity(&backup_path).map_err(|e| {
        miette!(
            "Backup integrity check failed for {}: {}",
            backup_path.display(),
            e
        )
    })?;
    if !integrity {
        return Err(miette!(
            "Backup at {} failed PRAGMA integrity_check. Refusing to mutate the ledger.",
            backup_path.display()
        ));
    }

    Ok(backup_path)
}

pub(crate) fn verify_backup_integrity(backup_path: &Path) -> Result<bool> {
    let conn = rusqlite::Connection::open(backup_path)
        .map_err(|e| miette!("Could not open backup for integrity check: {}", e))?;
    let result: String = conn
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .map_err(|e| miette!("integrity_check query failed: {}", e))?;
    Ok(result.eq_ignore_ascii_case("ok"))
}

pub(crate) fn nanos_since_epoch() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64
}
