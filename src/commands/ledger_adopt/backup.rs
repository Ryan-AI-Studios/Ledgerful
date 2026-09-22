//! Adoption backup: online snapshot, integrity, and foreign keys.

use miette::{Result, miette};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

pub struct CheckedBackup {
    pub path: PathBuf,
    pub digest: String,
}

pub fn backup_for_adoption(src: &rusqlite::Connection, db_path: &Path) -> Result<CheckedBackup> {
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let base_name = db_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("ledger.db");
    let parent = std::env::var("LEDGERFUL_TEST_BACKUP_DIR")
        .ok()
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| db_path.parent().unwrap_or(Path::new(".")).to_path_buf());
    let backup_path = parent.join(format!("{base_name}.{timestamp}.bak"));

    {
        let mut dst = rusqlite::Connection::open(&backup_path).map_err(|e| {
            miette!(
                "Failed to open backup database at {}: {e}",
                backup_path.display()
            )
        })?;
        let backup = rusqlite::backup::Backup::new(src, &mut dst)
            .map_err(|e| miette!("Failed to initialize SQLite online backup: {e}"))?;
        backup
            .step(-1)
            .map_err(|e| miette!("SQLite online backup failed: {e}"))?;
    }

    let check = rusqlite::Connection::open(&backup_path)
        .map_err(|e| miette!("Could not open backup for integrity check: {e}"))?;
    let integrity: String = check
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .map_err(|e| miette!("integrity_check query failed: {e}"))?;
    if !integrity.eq_ignore_ascii_case("ok") {
        return Err(miette!(
            "Backup at {} failed PRAGMA integrity_check.",
            backup_path.display()
        ));
    }
    let fk_rows: i64 = check
        .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
            row.get(0)
        })
        .map_err(|e| miette!("foreign_key_check query failed: {e}"))?;
    if fk_rows != 0 {
        return Err(miette!(
            "Backup at {} failed PRAGMA foreign_key_check ({fk_rows} rows).",
            backup_path.display()
        ));
    }

    let bytes = std::fs::read(&backup_path)
        .map_err(|e| miette!("read backup {}: {e}", backup_path.display()))?;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    Ok(CheckedBackup {
        path: backup_path,
        digest: hex::encode(hasher.finalize()),
    })
}
