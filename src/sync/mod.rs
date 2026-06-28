pub mod apply;
pub mod bundle;
pub mod crypto;
pub mod error;
pub mod extract;
pub mod hlc;
pub mod state;
pub mod transport;

use crate::config::model::Config;
use crate::sync::bundle::Bundle;
use crate::sync::error::SyncError;
use crate::sync::transport::SyncTarget;
use ed25519_dalek::SigningKey;
use rusqlite::Connection;
use std::collections::HashMap;
use std::path::Path;

pub fn run(config: &Config, state_dir: &Path, team_secret: &[u8]) -> miette::Result<()> {
    use miette::IntoDiagnostic;

    if !config.sync.enabled {
        return Ok(());
    }

    let sync_dir = state_dir.join(".ledgerful").join("sync");
    if !sync_dir.exists() {
        return Err(miette::miette!(
            "Sync is not initialized. Run 'ledgerful sync init' first."
        ));
    }

    let key_path = sync_dir.join("device.key");
    if !key_path.exists() {
        return Err(miette::miette!(
            "Device key not found. Run 'ledgerful sync init' first."
        ));
    }

    let key_bytes = std::fs::read(&key_path).into_diagnostic()?;
    let sign_key = SigningKey::from_bytes(
        key_bytes
            .as_slice()
            .try_into()
            .map_err(|_| miette::miette!("Invalid device key length"))?,
    );

    let device_id = config
        .sync
        .device_id
        .as_ref()
        .ok_or_else(|| miette::miette!("device_id not configured"))?;

    let target = SyncTarget::parse(&config.sync.target).into_diagnostic()?;
    let transport = target.connect(device_id);

    // 1. Extract
    println!("Extracting local deltas...");
    match extract::extract(state_dir, device_id, &sign_key, config.sync.batch_size) {
        Ok(bundle) => {
            if bundle.manifest.entry_count > 0 || !bundle.manifest.tombstones.is_empty() {
                println!(
                    "Bundle created with {} entries and {} tombstones",
                    bundle.manifest.entry_count,
                    bundle.manifest.tombstones.len()
                );

                // 2. Encrypt and Upload
                let zip_bytes = Bundle::build(bundle.manifest.clone(), &sign_key)
                    .map_err(|e| miette::miette!("Failed to build ZIP: {}", e))?
                    .0;

                let encrypted = Bundle::encrypt(&zip_bytes, team_secret)
                    .map_err(|e| miette::miette!("Encryption failed: {}", e))?;

                let filename = bundle.manifest.filename();
                let temp_dir = tempfile::tempdir().into_diagnostic()?;
                let temp_path = temp_dir.path().join(&filename);
                std::fs::write(&temp_path, &encrypted).into_diagnostic()?;

                transport
                    .put_outgoing(&temp_path)
                    .map_err(|e| miette::miette!("Transport put failed: {}", e))?;
                println!("Uploaded bundle: {}", filename);
            } else {
                println!("No new entries to extract.");
            }
        }
        Err(SyncError::NoNewEntries) => {
            println!("No new entries to extract.");
        }
        Err(e) => return Err(e).into_diagnostic(),
    }

    // 3. Apply
    println!("Fetching remote bundles...");
    let incoming = transport
        .list_incoming()
        .map_err(|e| miette::miette!("Transport list failed: {}", e))?;

    // Load peer keys
    let mut peer_keys = HashMap::new();
    let peers_dir = sync_dir.join("peers");
    if peers_dir.exists() {
        for entry in std::fs::read_dir(peers_dir).into_diagnostic()? {
            let entry = entry.into_diagnostic()?;
            if entry.file_type().into_diagnostic()?.is_file() {
                let name = entry.file_name().to_string_lossy().to_string();
                if let Some(peer_id) = name.strip_suffix(".pub") {
                    let bytes = std::fs::read(entry.path()).into_diagnostic()?;
                    let mut key = [0u8; 32];
                    key.copy_from_slice(&bytes);
                    peer_keys.insert(peer_id.to_string(), key);
                }
            }
        }
    }
    // Add our own key to peer_keys to allow self-apply if needed (though usually we skip our own)
    peer_keys.insert(device_id.clone(), sign_key.verifying_key().to_bytes());

    let db_path = state_dir.join("state").join("ledger.db");
    let mut conn = Connection::open(&db_path).into_diagnostic()?;

    for bundle_path in incoming {
        let name = bundle_path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .to_string();

        // Skip our own bundles if they appear in incoming (depending on transport)
        if name.contains(device_id) {
            continue;
        }

        println!("Applying bundle: {}", name);
        let encrypted = transport
            .get_incoming(&name)
            .map_err(|e| miette::miette!("Transport get failed: {}", e))?;

        let zip_bytes = match Bundle::decrypt(&encrypted, team_secret) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("Failed to decrypt {}: {}. Quarantining.", name, e);
                transport
                    .move_to_quarantine(&name)
                    .map_err(|e| miette::miette!("Transport move failed: {}", e))?;
                continue;
            }
        };

        let bundle = match Bundle::parse(&zip_bytes, &peer_keys) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("Failed to parse {}: {}. Quarantining.", name, e);
                transport
                    .move_to_quarantine(&name)
                    .map_err(|e| miette::miette!("Transport move failed: {}", e))?;
                continue;
            }
        };

        match apply::apply(&bundle, &mut conn, &peer_keys) {
            Ok(report) => {
                println!(
                    "Applied {}: {} inserted, {} updated, {} skipped",
                    name, report.inserted, report.updated, report.skipped
                );
                transport
                    .move_to_processed(&name)
                    .map_err(|e| miette::miette!("Transport move failed: {}", e))?;
            }
            Err(e) => {
                eprintln!("Failed to apply {}: {}. Quarantining.", name, e);
                transport
                    .move_to_quarantine(&name)
                    .map_err(|e| miette::miette!("Transport move failed: {}", e))?;
            }
        }
    }

    // 4. Cleanup
    let retention_days = config.sync.archive_retention_days;
    let older_than =
        std::time::SystemTime::now() - std::time::Duration::from_secs(retention_days * 24 * 3600);
    match transport.trim_processed(older_than) {
        Ok(count) => {
            if count > 0 {
                println!("Trimmed {} old bundles from archive", count);
            }
        }
        Err(e) => eprintln!("Warning: Failed to trim archive: {}", e),
    }

    println!("Sync complete.");
    Ok(())
}
