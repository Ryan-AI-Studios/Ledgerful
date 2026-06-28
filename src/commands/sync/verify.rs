use crate::state::layout::Layout;
use crate::sync::bundle::Bundle;
use miette::{IntoDiagnostic, Result, miette};
use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::Path;

pub fn handle(bundle_path: &str) -> Result<()> {
    let path = Path::new(bundle_path);
    if !path.exists() {
        return Err(miette!("Bundle file not found: {}", bundle_path));
    }

    let team_secret = std::env::var("LEDGERFUL_SYNC_SECRET").map_err(|_| {
        miette!(
            "LEDGERFUL_SYNC_SECRET environment variable not set. It is required to verify bundles."
        )
    })?;

    println!("Verifying bundle: {}", bundle_path);

    let data = fs::read(path).map_err(|e| miette!("Failed to read bundle: {}", e))?;

    // 1. Decrypt bundle
    let zip_bytes = Bundle::decrypt(&data, team_secret.as_bytes())
        .map_err(|e| miette!("Failed to decrypt bundle: {}", e))?;

    // 2. Load known peer keys
    let current_dir = env::current_dir().into_diagnostic()?;
    let layout = Layout::new(current_dir.to_string_lossy().as_ref());
    let peers_dir = layout.root.join(".ledgerful").join("sync").join("peers");

    let mut verify_keys = HashMap::new();
    if peers_dir.exists() {
        for entry in fs::read_dir(peers_dir).into_diagnostic()? {
            let entry = entry.into_diagnostic()?;
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "pub") {
                let device_id = path.file_stem().unwrap().to_string_lossy().to_string();
                let key_bytes = fs::read(&path).into_diagnostic()?;
                if key_bytes.len() == 32 {
                    let mut arr = [0u8; 32];
                    arr.copy_from_slice(&key_bytes);
                    verify_keys.insert(device_id, arr);
                }
            }
        }
    }

    // Also include our own public key for self-verification
    let own_pub_path = layout
        .root
        .join(".ledgerful")
        .join("sync")
        .join("device.pub");
    if own_pub_path.exists() {
        // We need the device_id too. Get it from DB.
        let storage_path = layout.state_subdir().join("ledger.db");
        let storage = storage_manager_init_minimal(storage_path.as_std_path())?;
        let device_id: String = storage
            .query_row("SELECT device_id FROM sync_state WHERE id = 1", [], |row| {
                row.get(0)
            })
            .unwrap_or_else(|_| "unknown".to_string());

        let key_bytes = fs::read(&own_pub_path).into_diagnostic()?;
        if key_bytes.len() == 32 {
            let mut arr = [0u8; 32];
            arr.copy_from_slice(&key_bytes);
            verify_keys.insert(device_id, arr);
        }
    }

    // 3. Parse and verify bundle
    let bundle = Bundle::parse(&zip_bytes, &verify_keys)
        .map_err(|e| miette!("Failed to verify bundle signature or integrity: {}", e))?;

    println!("Bundle Verification Success:");
    println!("  Version:        {}", bundle.manifest.version);
    println!("  Device ID:      {}", bundle.manifest.device_id);
    println!("  Bundle HLC:     {}", bundle.manifest.bundle_hlc);
    println!("  Entry Count:    {}", bundle.manifest.entry_count);
    println!("  Signature:      Valid (Ed25519)");
    println!("  Integrity:      Valid (SHA-256)");

    Ok(())
}

// Helper to avoid full StorageManager init which might be overkill or cause issues if already initialized
fn storage_manager_init_minimal(db_path: &Path) -> Result<rusqlite::Connection> {
    rusqlite::Connection::open(db_path).into_diagnostic()
}
