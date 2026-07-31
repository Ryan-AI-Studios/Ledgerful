// `hlc`, `state`, `error` are always available — they have no `sync`-feature
// deps (only `serde`, `rusqlite`, `miette`, `thiserror`, `std`).
// The web handler's `SyncStatusResponse` DTO + `hlc_to_iso8601` helper
// reference these without the `sync` feature so the OpenAPI schema can
// document `/api/sync/status` in all builds. The crypto/transport/extract/
// apply/bundle modules stay gated.
pub mod error;
pub mod hlc;
pub mod state;

#[cfg(feature = "sync")]
pub mod apply;
#[cfg(feature = "sync")]
pub mod bundle;
#[cfg(feature = "sync")]
pub mod crypto;
#[cfg(feature = "sync")]
pub mod extract;
#[cfg(feature = "sync")]
pub mod peers;
#[cfg(feature = "sync")]
pub mod transport;

#[cfg(feature = "sync")]
use crate::config::model::Config;
#[cfg(feature = "sync")]
use crate::sync::bundle::Bundle;
#[cfg(feature = "sync")]
use crate::sync::error::SyncError;
#[cfg(feature = "sync")]
use crate::sync::transport::SyncTarget;
#[cfg(feature = "sync")]
use ed25519_dalek::SigningKey;
#[cfg(feature = "sync")]
use rusqlite::Connection;
#[cfg(feature = "sync")]
use std::path::Path;
#[cfg(feature = "sync")]
use std::time::{SystemTime, UNIX_EPOCH};

/// Run one sync cycle (extract → put → list → apply → trim).
///
/// `state_dir` is the layout state home (typically `{work_root}/.ledgerful`):
/// - keys: `{state_dir}/sync/`
/// - ledger DB: `{state_dir}/state/ledger.db`
///
/// Local `device_id` is read from SoT `sync_state.device_id` (not config-only).
/// Callers should refuse when `!enabled` with a user-visible message; this
/// silent return remains defense-in-depth only.
#[cfg(feature = "sync")]
pub fn run(config: &Config, state_dir: &Path, team_secret: &[u8]) -> miette::Result<()> {
    use miette::IntoDiagnostic;

    if !config.sync.enabled {
        return Ok(());
    }

    let sync_dir = state_dir.join("sync");
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

    let db_path = state_dir.join("state").join("ledger.db");
    let mut conn = Connection::open(&db_path).into_diagnostic()?;

    // SoT: sync_state.device_id (init writes this). Config mirror is optional.
    let device_id = match crate::sync::state::SyncState::load(&conn)? {
        Some(s) if !s.device_id.is_empty() && s.device_id != "unknown" => s.device_id,
        _ => {
            return Err(miette::miette!(
                "device_id SoT missing in sync_state. Run 'ledgerful sync init' first."
            ));
        }
    };

    let target = SyncTarget::parse(&config.sync.target).into_diagnostic()?;
    let transport = target.connect(&device_id);

    let mut exported = 0usize;
    let mut imported = 0usize;
    let mut updated = 0usize;
    let mut skipped = 0usize;
    let mut quarantined = 0usize;
    let mut trimmed = 0usize;

    // 1. Extract → encrypt → put → commit cursor (single signed zip; no rebuild).
    // Export DB side-effects commit only after put succeeds so a failed upload
    // does not permanently drop local deltas on retry.
    println!("Extracting local deltas...");
    match extract::extract(state_dir, &device_id, &sign_key, config.sync.batch_size) {
        Ok(extracted) => {
            let entry_count = extracted.bundle.manifest.entry_count;
            let tombstone_count = extracted.bundle.manifest.tombstones.len();
            println!(
                "Bundle created with {} entries and {} tombstones",
                entry_count, tombstone_count
            );

            let encrypted = Bundle::encrypt(&extracted.zip_bytes, team_secret)
                .map_err(|e| miette::miette!("Encryption failed: {}", e))?;

            let filename = extracted.bundle.manifest.filename();
            transport
                .put_outgoing_bytes(&filename, &encrypted)
                .map_err(|e| miette::miette!("Transport put failed: {}", e))?;

            extract::commit_extract_export(state_dir, &extracted, &device_id)
                .map_err(|e| miette::miette!("Failed to commit extract export state: {}", e))?;

            println!("Uploaded bundle: {}", filename);
            exported = 1;
        }
        Err(SyncError::NoNewEntries) => {
            println!("No new entries to extract.");
        }
        Err(e) => return Err(e).into_diagnostic(),
    }

    // 2. Apply remote peer bundles
    println!("Fetching remote bundles...");
    let incoming = transport
        .list_incoming()
        .map_err(|e| miette::miette!("Transport list failed: {}", e))?;

    // Load peer keys (peers only; fallible — no copy_from_slice panic on malformed *.pub).
    let mut peer_keys = peers::load_peer_keys(&sync_dir)
        .map_err(|e| miette::miette!("Failed to load peer keys: {e}"))?;
    // Self-insert stays at the call site (do not fold local key into load_peer_keys).
    peer_keys.insert(device_id.clone(), sign_key.verifying_key().to_bytes());

    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let max_ahead_ms = config.sync.max_clock_drift_seconds.saturating_mul(1000);

    for id in incoming {
        // Peer-scoped list already skips local outbox — no substring self-skip.
        let label = format!("{}/{}", id.peer_id, id.name);
        println!("Applying bundle: {}", label);

        let encrypted = match transport.get_incoming(&id) {
            Ok(b) => b,
            Err(e) => {
                // Cannot quarantine without a readable path (missing/symlink/oversize).
                eprintln!(
                    "Failed to get {}: {}. Skipping (not counted as quarantined).",
                    label, e
                );
                continue;
            }
        };

        let zip_bytes = match Bundle::decrypt(&encrypted, team_secret) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("Failed to decrypt {}: {}. Quarantining.", label, e);
                match transport.move_to_quarantine(&id) {
                    Ok(()) => quarantined += 1,
                    Err(move_err) => {
                        eprintln!(
                            "Warning: failed to move {} to quarantine after decrypt error: {}. Bundle may be retried (not counted quarantined).",
                            label, move_err
                        );
                    }
                }
                continue;
            }
        };

        let bundle = match Bundle::parse(&zip_bytes, &peer_keys) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("Failed to parse {}: {}. Quarantining.", label, e);
                match transport.move_to_quarantine(&id) {
                    Ok(()) => quarantined += 1,
                    Err(move_err) => {
                        eprintln!(
                            "Warning: failed to move {} to quarantine after parse error: {}. Bundle may be retried (not counted quarantined).",
                            label, move_err
                        );
                    }
                }
                continue;
            }
        };

        // Ahead-only clock skew: reject remote bundle_hlc too far in the future.
        if bundle.manifest.bundle_hlc.physical_ms > now_ms.saturating_add(max_ahead_ms) {
            eprintln!(
                "Bundle {} has future HLC (clock drift: remote {} ms vs local {} ms, max ahead {} s). Quarantining.",
                label,
                bundle.manifest.bundle_hlc.physical_ms,
                now_ms,
                config.sync.max_clock_drift_seconds
            );
            match transport.move_to_quarantine(&id) {
                Ok(()) => quarantined += 1,
                Err(move_err) => {
                    eprintln!(
                        "Warning: failed to move {} to quarantine after clock-drift reject: {}. Bundle may be retried (not counted quarantined).",
                        label, move_err
                    );
                }
            }
            continue;
        }

        match apply::apply(&bundle, &mut conn, &peer_keys) {
            Ok(report) => {
                println!(
                    "Applied {}: {} inserted, {} updated, {} skipped",
                    label, report.inserted, report.updated, report.skipped
                );
                imported += report.inserted;
                updated += report.updated;
                skipped += report.skipped;
                if let Err(move_err) = transport.move_to_processed(&id) {
                    eprintln!(
                        "Warning: applied {} but failed to move to processed: {}. Bundle may be re-applied (idempotent).",
                        label, move_err
                    );
                }
            }
            Err(e) => {
                eprintln!("Failed to apply {}: {}. Quarantining.", label, e);
                match transport.move_to_quarantine(&id) {
                    Ok(()) => quarantined += 1,
                    Err(move_err) => {
                        eprintln!(
                            "Warning: failed to move {} to quarantine after apply error: {}. Bundle may be retried (not counted quarantined).",
                            label, move_err
                        );
                    }
                }
            }
        }
    }

    // 3. Cleanup
    let retention_days = config.sync.archive_retention_days;
    let older_than =
        std::time::SystemTime::now() - std::time::Duration::from_secs(retention_days * 24 * 3600);
    match transport.trim_processed(older_than) {
        Ok(count) => {
            trimmed = count;
            if count > 0 {
                println!("Trimmed {} old bundles from archive", count);
            }
        }
        Err(e) => eprintln!("Warning: Failed to trim archive: {}", e),
    }

    println!(
        "Sync complete. exported={} imported={} updated={} skipped={} quarantined={} trimmed={}",
        exported, imported, updated, skipped, quarantined, trimmed
    );
    Ok(())
}
