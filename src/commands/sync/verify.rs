use crate::state::storage::StorageManager;
use crate::sync::bundle::Bundle;
use miette::{IntoDiagnostic, Result, miette};
use rusqlite::OptionalExtension;
use std::collections::HashMap;
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
    let layout = crate::commands::helpers::get_layout()?;
    let peers_dir = layout.state_dir.join("sync").join("peers");

    let mut verify_keys = HashMap::new();
    if peers_dir.exists() {
        for entry in fs::read_dir(peers_dir).into_diagnostic()? {
            let entry = entry.into_diagnostic()?;
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "pub") {
                let Some(stem) = path.file_stem() else {
                    continue;
                };
                let device_id = stem.to_string_lossy().to_string();
                let key_bytes = fs::read(&path).into_diagnostic()?;
                if key_bytes.len() == 32 {
                    let mut arr = [0u8; 32];
                    arr.copy_from_slice(&key_bytes);
                    verify_keys.insert(device_id, arr);
                }
            }
        }
    }

    // Also include our own public key for self-verification (aligned with init/run/pair/status).
    let own_pub_path = layout.state_dir.join("sync").join("device.pub");
    if own_pub_path.exists() {
        let storage = StorageManager::init_with_layout(&layout)?;
        let device_id: String = storage
            .get_connection()
            .query_row("SELECT device_id FROM sync_state WHERE id = 1", [], |row| {
                row.get(0)
            })
            .optional()
            .map_err(|e| miette!("Failed to query sync_state device_id: {e}"))?
            .unwrap_or_else(|| "unknown".to_string());

        let key_bytes = fs::read(own_pub_path.as_std_path()).into_diagnostic()?;
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
