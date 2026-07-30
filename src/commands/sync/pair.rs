use crate::state::storage::StorageManager;
use ed25519_dalek::VerifyingKey;
use miette::{Result, miette};
use std::fs;

/// Generate a provisional pairing code, or refuse accept (fail-closed until 0111).
pub fn handle(code: Option<String>) -> Result<()> {
    let layout = crate::commands::helpers::get_layout()?;

    let sync_dir = layout.state_dir.join("sync");
    let pub_path = sync_dir.join("device.pub");

    if !pub_path.exists() {
        return Err(miette!(
            "device.pub not found at {}. Run `ledgerful sync init` first.",
            pub_path
        ));
    }

    let pub_key_bytes =
        fs::read(pub_path.as_std_path()).map_err(|e| miette!("Failed to read device.pub: {e}"))?;

    let _verifying_key = VerifyingKey::try_from(pub_key_bytes.as_slice())
        .map_err(|e| miette!("Invalid public key: {e}"))?;

    let storage = StorageManager::init_with_layout(&layout)?;
    let conn = storage.get_connection();

    let device_id: String = conn
        .query_row(
            "SELECT device_id FROM sync_state WHERE id = 1",
            [],
            |row| row.get(0),
        )
        .map_err(|e| {
            miette!(
                "Failed to read SoT device_id from sync_state: {e}. Run `ledgerful sync init` first."
            )
        })?;
    if device_id.trim().is_empty() || device_id == "unknown" {
        return Err(miette!(
            "SoT device_id is empty or invalid. Run `ledgerful sync init` (or --force) to mint a device identity."
        ));
    }

    match code {
        Some(_c) => {
            // Fail-closed: never print success / "Sync enabled" until 0111 peer store.
            Err(miette!(
                help = "Pair accept and peer key storage land in track 0111. \
On this device, generate a provisional code with `ledgerful sync pair` (no argument) after init. \
Do not set [sync].enabled = true until pairing is complete.",
                "Peer pairing accept is not implemented yet (Experimental team sync — track 0111)."
            ))
        }
        None => {
            let team_secret = std::env::var("LEDGERFUL_SYNC_SECRET").map_err(|_| {
                miette!(
                    "LEDGERFUL_SYNC_SECRET is not set. It is required to generate a provisional pairing code."
                )
            })?;
            if team_secret.trim().is_empty() {
                return Err(miette!("Team secret cannot be empty."));
            }

            // Provisional code: blake3(secret || pubkey) — construction review in 0111.
            let mut hmac_input = Vec::new();
            hmac_input.extend_from_slice(team_secret.as_bytes());
            hmac_input.extend_from_slice(&pub_key_bytes);

            let hash = blake3::hash(&hmac_input);
            let device_prefix = if device_id.len() >= 4 {
                &device_id[..4]
            } else {
                &device_id
            };
            let code_str = format!("{}-{}", device_prefix, &hash.to_hex()[..8]);

            println!("[Experimental] Provisional pairing code: {code_str}");
            println!("  Device ID (SoT): {device_id}");
            println!();
            println!(
                "Accept on a peer is not implemented yet (track 0111). \
This code is for experimental preview only — do not treat pairing as complete."
            );
            Ok(())
        }
    }
}
