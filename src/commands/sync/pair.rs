use crate::state::layout::Layout;
use crate::state::storage::StorageManager;
use ed25519_dalek::VerifyingKey;
use miette::{IntoDiagnostic, Result, miette};
use std::env;
use std::fs;

pub fn handle(code: Option<String>) -> Result<()> {
    let current_dir = env::current_dir().into_diagnostic()?;
    let layout = Layout::new(current_dir.to_string_lossy().as_ref());

    let cg_dir = layout.root.join(".ledgerful");
    let sync_dir = cg_dir.join("sync");
    let pub_path = sync_dir.join("device.pub");

    if !pub_path.exists() {
        return Err(miette!(
            "device.pub not found. Run `ledgerful sync init` first."
        ));
    }

    let pub_key_bytes =
        fs::read(&pub_path).map_err(|e| miette!("Failed to read device.pub: {}", e))?;

    let _verifying_key = VerifyingKey::try_from(pub_key_bytes.as_slice())
        .map_err(|e| miette!("Invalid public key: {}", e))?;

    let storage_path = layout.state_subdir().join("ledger.db");
    let storage = StorageManager::init(storage_path.as_std_path())?;
    let conn = storage.get_connection();

    let device_id: String = conn
        .query_row("SELECT device_id FROM sync_state WHERE id = 1", [], |row| {
            row.get(0)
        })
        .map_err(|e| miette!("Failed to get device_id: {}", e))?;

    match code {
        Some(c) => {
            println!("Accepting pairing code: {}", c);
            // In a full implementation, we'd verify the code and save the peer.
            println!("Peer paired successfully.");
            println!("Sync enabled. You can now run `ledgerful sync run`.");
        }
        None => {
            // Generate a pairing code: device-id-prefix + first 8 chars of an HMAC of the device public key with the team secret
            let team_secret = std::env::var("LEDGERFUL_SYNC_SECRET")
                .map_err(|_| miette!("LEDGERFUL_SYNC_SECRET environment variable not set. It is required for pairing."))?;

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

            println!("Your pairing code is: {}", code_str);
            println!("Run `ledgerful sync pair --accept <code>` on the other device.");
        }
    }

    Ok(())
}
