use crate::state::storage::StorageManager;
use crate::sync::bundle::Bundle;
use crate::sync::peers::load_peer_keys;
use miette::{Result, miette};
use rusqlite::OptionalExtension;
use std::fs;
use std::path::Path;
use zeroize::Zeroizing;

pub fn handle(bundle_path: &str) -> Result<()> {
    let path = Path::new(bundle_path);
    if !path.exists() {
        return Err(miette!("Bundle file not found: {}", bundle_path));
    }

    let team_secret: Zeroizing<String> = Zeroizing::new(
        std::env::var("LEDGERFUL_SYNC_SECRET").map_err(|_| {
            miette!(
                "LEDGERFUL_SYNC_SECRET environment variable not set. It is required to verify bundles."
            )
        })?,
    );

    println!("Verifying bundle: {}", bundle_path);

    let data = fs::read(path).map_err(|e| miette!("Failed to read bundle: {}", e))?;

    // 1. Decrypt bundle
    let zip_bytes = Bundle::decrypt(&data, team_secret.as_bytes())
        .map_err(|e| miette!("Failed to decrypt bundle: {}", e))?;

    // 2. Load known peer keys via shared fallible path (curve-checked; no copy_from_slice panic).
    let layout = crate::commands::helpers::get_layout()?;
    let sync_dir = layout.state_dir.join("sync");
    let mut verify_keys = load_peer_keys(sync_dir.as_std_path())
        .map_err(|e| miette!("Failed to load peer keys: {e}"))?;

    // Self-insert from SoT device.pub (aligned with run) — not folded into load_peer_keys.
    let own_pub_path = sync_dir.join("device.pub");
    if own_pub_path.exists() {
        let storage = StorageManager::init_with_layout(&layout)?;
        let device_id: Option<String> = storage
            .get_connection()
            .query_row("SELECT device_id FROM sync_state WHERE id = 1", [], |row| {
                row.get(0)
            })
            .optional()
            .map_err(|e| miette!("Failed to query sync_state device_id: {e}"))?;

        if let Some(device_id) = device_id
            && !device_id.is_empty()
            && device_id != "unknown"
        {
            let key_bytes = fs::read(own_pub_path.as_std_path())
                .map_err(|e| miette!("Failed to read device.pub: {e}"))?;
            if key_bytes.len() == 32 {
                let mut arr = [0u8; 32];
                arr.copy_from_slice(&key_bytes);
                // Curve-check before insert (same posture as trust_peer / load_peer_keys).
                if ed25519_dalek::VerifyingKey::from_bytes(&arr).is_ok() {
                    verify_keys.insert(device_id, arr);
                }
            }
            // Wrong-length keys are skipped (no panic) — consistent with load_peer_keys.
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
