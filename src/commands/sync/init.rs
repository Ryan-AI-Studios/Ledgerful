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
///
/// Order of operations (atomicity for failure modes codex P1-01):
/// 1. Resolve + validate team secret **before** any key writes.
/// 2. Generate key material + device_id in memory.
/// 3. Write keys to `*.tmp` then rename into place (so force keeps old keys until
///    the temp write succeeds).
/// 4. Upsert SoT; on failure after rename, best-effort note in error (keys+SoT
///    intended as a unit; config mirror is non-authoritative).
/// 5. Config mirror via toml_edit (never enables sync).
pub fn handle(force: bool, with_secret: Option<String>) -> Result<()> {
    let layout = get_layout()?;
    layout.ensure_state_dir()?;

    let sync_dir = layout.state_dir.join("sync");
    if !sync_dir.exists() {
        fs::create_dir_all(sync_dir.as_std_path())
            .map_err(|e| miette!("Failed to create sync dir {}: {e}", sync_dir))?;
    }

    let key_path = sync_dir.join("device.key");
    let pub_path = sync_dir.join("device.pub");
    if key_path.exists() && !force {
        return Err(miette!(
            "device.key already exists at {}. Use --force to overwrite (new keys + new device_id).",
            key_path
        ));
    }

    // 1. Secret first — never write keys when secret is missing/empty.
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
    // Secret is only validated for presence (pair/run need it later); not stored.
    secret.zeroize();

    // 2. Generate material in memory.
    let signing_key = SigningKey::generate(&mut OsRng);
    let key_bytes = signing_key.to_bytes();
    let pub_key = signing_key.verifying_key().to_bytes();
    let device_id = format!(
        "device-{}",
        uuid::Uuid::new_v4()
            .to_string()
            .chars()
            .take(8)
            .collect::<String>()
    );

    // 3. Atomic-ish key install: write temps, then rename over final paths.
    let key_tmp = sync_dir.join("device.key.tmp");
    let pub_tmp = sync_dir.join("device.pub.tmp");

    if let Err(e) = fs::write(key_tmp.as_std_path(), key_bytes) {
        return Err(miette!("Failed to write device.key temp: {e}"));
    }
    #[cfg(unix)]
    {
        set_mode(&key_tmp, 0o600)?;
    }

    if let Err(e) = fs::write(pub_tmp.as_std_path(), pub_key) {
        let _ = fs::remove_file(key_tmp.as_std_path());
        return Err(miette!("Failed to write device.pub temp: {e}"));
    }
    #[cfg(unix)]
    {
        if let Err(e) = set_mode(&pub_tmp, 0o644) {
            let _ = fs::remove_file(key_tmp.as_std_path());
            let _ = fs::remove_file(pub_tmp.as_std_path());
            return Err(e);
        }
    }

    // Promote temps → final. On Windows, rename over existing may need remove first.
    if let Err(e) = promote_temp(&key_tmp, &key_path) {
        let _ = fs::remove_file(key_tmp.as_std_path());
        let _ = fs::remove_file(pub_tmp.as_std_path());
        return Err(miette!("Failed to install device.key: {e}"));
    }
    if let Err(e) = promote_temp(&pub_tmp, &pub_path) {
        // Keys may be split (new key, old pub) — remove new key so we do not leave
        // a half-installed pair; force callers re-run.
        let _ = fs::remove_file(key_path.as_std_path());
        let _ = fs::remove_file(pub_tmp.as_std_path());
        return Err(miette!(
            "Failed to install device.pub after device.key: {e}. Re-run `ledgerful sync init --force`."
        ));
    }

    // 4. SoT upsert immediately after keys land.
    let storage = StorageManager::init_with_layout(&layout).map_err(|e| {
        miette!(
            "Keys written but failed to open storage for SoT device_id upsert: {e}. \
             Re-run `ledgerful sync init --force` to realign keys + SoT."
        )
    })?;
    let conn = storage.get_connection();
    if let Err(e) = conn.execute(
        "INSERT INTO sync_state (id, device_id) VALUES (1, ?1)
         ON CONFLICT(id) DO UPDATE SET device_id = excluded.device_id",
        [&device_id],
    ) {
        return Err(miette!(
            "Keys written but failed to upsert sync_state.device_id (SoT): {e}. \
             Re-run `ledgerful sync init --force` to realign keys + SoT."
        ));
    }

    // 5. Optional config mirror via toml_edit — never sets enabled=true.
    // Mirror failure does not roll back keys/SoT (SoT is authoritative).
    if let Err(e) = crate::commands::config::execute_config_set_in(
        &layout,
        &format!("sync.device_id=\"{device_id}\""),
    ) {
        return Err(miette!(
            "Keys + SoT device_id written, but config mirror failed: {e}. \
             SoT is authoritative; set sync.device_id={device_id} manually or re-run init --force."
        ));
    }

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

fn promote_temp(tmp: &camino::Utf8Path, final_path: &camino::Utf8Path) -> std::io::Result<()> {
    if final_path.exists() {
        fs::remove_file(final_path.as_std_path())?;
    }
    fs::rename(tmp.as_std_path(), final_path.as_std_path())
}

#[cfg(unix)]
fn set_mode(path: &camino::Utf8Path, mode: u32) -> Result<()> {
    let meta = fs::metadata(path.as_std_path())
        .map_err(|e| miette!("Failed to read metadata for permissions on {path}: {e}"))?;
    let mut perms = meta.permissions();
    perms.set_mode(mode);
    fs::set_permissions(path.as_std_path(), perms)
        .map_err(|e| miette!("Failed to set permissions {mode:#o} on {path}: {e}"))
}
