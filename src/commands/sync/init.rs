use crate::commands::helpers::get_layout;
use crate::state::storage::StorageManager;
use miette::{Result, miette};
use std::fs;

use ed25519_dalek::SigningKey;
use rand::rngs::OsRng;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use zeroize::Zeroize;

/// Initialize team sync keys + SoT device_id for this device.
///
/// - Layout-aware (`get_layout()`): keys under `layout.state_dir/sync/`.
/// - Always upserts `sync_state.device_id` (SoT, row id=1).
/// - Mirrors `sync.device_id` into config via `toml_edit` helpers.
/// - **Never** sets `[sync].enabled = true`.
/// - `--force`: new key material + new device_id written to SoT and config together.
pub fn handle(force: bool, with_secret: Option<String>) -> Result<()> {
    let layout = get_layout()?;
    layout.ensure_state_dir()?;

    let sync_dir = layout.state_dir.join("sync");
    if !sync_dir.exists() {
        fs::create_dir_all(sync_dir.as_std_path())
            .map_err(|e| miette!("Failed to create sync dir {}: {e}", sync_dir))?;
    }

    let key_path = sync_dir.join("device.key");
    if key_path.exists() && !force {
        return Err(miette!(
            "device.key already exists at {}. Use --force to overwrite (new keys + new device_id).",
            key_path
        ));
    }

    let signing_key = SigningKey::generate(&mut OsRng);
    let key_bytes = signing_key.to_bytes();

    fs::write(key_path.as_std_path(), key_bytes)
        .map_err(|e| miette!("Failed to write device.key: {e}"))?;

    #[cfg(unix)]
    {
        let meta = fs::metadata(key_path.as_std_path())
            .map_err(|e| miette!("Failed to read device.key metadata for permissions: {e}"))?;
        let mut perms = meta.permissions();
        perms.set_mode(0o600);
        fs::set_permissions(key_path.as_std_path(), perms)
            .map_err(|e| miette!("Failed to set device.key permissions (0o600): {e}"))?;
    }

    let pub_path = sync_dir.join("device.pub");
    let pub_key = signing_key.verifying_key().to_bytes();
    fs::write(pub_path.as_std_path(), pub_key)
        .map_err(|e| miette!("Failed to write device.pub: {e}"))?;

    #[cfg(unix)]
    {
        let meta = fs::metadata(pub_path.as_std_path())
            .map_err(|e| miette!("Failed to read device.pub metadata for permissions: {e}"))?;
        let mut perms = meta.permissions();
        perms.set_mode(0o644);
        fs::set_permissions(pub_path.as_std_path(), perms)
            .map_err(|e| miette!("Failed to set device.pub permissions (0o644): {e}"))?;
    }

    // Secret is validated for presence (pair/run need it later) but not stored on disk.
    let mut secret = match with_secret {
        Some(s) => s,
        None => {
            if let Ok(s) = std::env::var("LEDGERFUL_SYNC_SECRET") {
                s
            } else {
                rpassword::prompt_password("Enter 12-word team secret: ")
                    .map_err(|e| miette!("Failed to read secret: {e}"))?
            }
        }
    };
    if secret.trim().is_empty() {
        secret.zeroize();
        return Err(miette!("Team secret cannot be empty."));
    }
    secret.zeroize();

    let device_id = format!(
        "device-{}",
        uuid::Uuid::new_v4()
            .to_string()
            .chars()
            .take(8)
            .collect::<String>()
    );

    // SoT: always upsert sync_state.device_id (preserve HLC columns on conflict).
    let storage = StorageManager::init_with_layout(&layout)?;
    let conn = storage.get_connection();
    conn.execute(
        "INSERT INTO sync_state (id, device_id) VALUES (1, ?1)
         ON CONFLICT(id) DO UPDATE SET device_id = excluded.device_id",
        [&device_id],
    )
    .map_err(|e| miette!("Failed to upsert sync_state.device_id (SoT): {e}"))?;

    // Optional config mirror via toml_edit — never sets enabled=true.
    crate::commands::config::execute_config_set_in(
        &layout,
        &format!("sync.device_id=\"{device_id}\""),
    )?;

    println!("[Experimental] Team sync initialized for this device.");
    println!("  Device ID (SoT): {device_id}");
    println!("  Keys:            {key_path}");
    println!("  Config mirror:   sync.device_id (enabled stays false)");
    println!();
    println!("Next steps (Experimental ladder):");
    println!("  1. ledgerful sync status");
    println!("  2. ledgerful sync pair          # provisional code (accept is NYI — 0111)");
    println!("  3. Set [sync].target and [sync].enabled = true only when ready to merge");
    println!("See docs/team-sync.md");
    Ok(())
}
