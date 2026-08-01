//! Team sync pairing: generate/accept LF-PAIR-1 invites, list/revoke peers.
//!
//! Never sets `[sync].enabled = true`. Secrets use `Zeroizing` on generate and accept.

use crate::state::storage::StorageManager;
use crate::sync::peers::{
    TrustOutcome, format_invite_v1, list_peers, revoke_peer, trust_peer,
    validate_device_id_for_path, verify_invite,
};
use ed25519_dalek::VerifyingKey;
use miette::{Result, miette};
use std::fs;
use zeroize::Zeroizing;

/// Pair command entry: mutually exclusive modes — invite accept, `--list`, `--revoke`, or generate.
pub fn handle(code: Option<String>, list: bool, revoke: Option<String>, force: bool) -> Result<()> {
    // Manual mutual exclusion (clap conflicts_with covers most; catch residual combos).
    let mode_count = [code.is_some(), list, revoke.is_some()]
        .iter()
        .filter(|x| **x)
        .count();
    if mode_count > 1 {
        return Err(miette!(
            "Conflicting pair flags: invite, --list, and --revoke are mutually exclusive. \
Use one of: `ledgerful sync pair`, `ledgerful sync pair <invite>`, \
`ledgerful sync pair --list`, or `ledgerful sync pair --revoke <device_id>`."
        ));
    }
    if force && code.is_none() {
        return Err(miette!(
            "--force only applies when accepting a pairing invite (re-key an existing peer)."
        ));
    }

    let layout = crate::commands::helpers::get_layout()?;
    let sync_dir = layout.state_dir.join("sync");

    if list {
        return handle_list(sync_dir.as_std_path());
    }
    if let Some(device_id) = revoke {
        return handle_revoke(sync_dir.as_std_path(), &device_id);
    }

    // Generate and accept need keys + SoT.
    let pub_path = sync_dir.join("device.pub");
    if !pub_path.exists() {
        return Err(miette!(
            "device.pub not found at {}. Run `ledgerful sync init` first.",
            pub_path
        ));
    }

    let pub_key_bytes =
        fs::read(pub_path.as_std_path()).map_err(|e| miette!("Failed to read device.pub: {e}"))?;
    let local_pub: [u8; 32] = pub_key_bytes
        .as_slice()
        .try_into()
        .map_err(|_| miette!("Invalid local device.pub length (expected 32 bytes)"))?;
    VerifyingKey::from_bytes(&local_pub).map_err(|e| miette!("Invalid local public key: {e}"))?;

    let storage = StorageManager::init_with_layout(&layout)?;
    let conn = storage.get_connection();
    let local_device_id: String = conn
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
    if local_device_id.trim().is_empty() || local_device_id == "unknown" {
        return Err(miette!(
            "SoT device_id is empty or invalid. Run `ledgerful sync init` (or --force) to mint a device identity."
        ));
    }

    match code {
        Some(invite) => handle_accept(
            sync_dir.as_std_path(),
            &layout,
            &local_device_id,
            &invite,
            force,
        ),
        None => handle_generate(&local_device_id, &local_pub),
    }
}

fn handle_list(sync_dir: &std::path::Path) -> Result<()> {
    let peers = list_peers(sync_dir).map_err(|e| miette!("{e}"))?;
    println!("[Available] Trusted peers ({})", peers.len());
    if peers.is_empty() {
        println!("  (none) — generate an invite with `ledgerful sync pair` on the peer device,");
        println!("  then accept here with `ledgerful sync pair <invite>` (same team secret).");
    } else {
        for id in &peers {
            println!("  {id}");
        }
    }
    Ok(())
}

fn handle_revoke(sync_dir: &std::path::Path, device_id: &str) -> Result<()> {
    validate_device_id_for_path(device_id).map_err(|e| miette!("{e}"))?;
    revoke_peer(sync_dir, device_id).map_err(|e| miette!("{e}"))?;
    println!("[Available] Revoked trust for peer '{device_id}'.");
    Ok(())
}

fn load_team_secret() -> Result<Zeroizing<String>> {
    let secret = Zeroizing::new(std::env::var("LEDGERFUL_SYNC_SECRET").map_err(|_| {
        miette!(
            "LEDGERFUL_SYNC_SECRET is not set. It is required to generate or accept a pairing invite."
        )
    })?);
    if secret.trim().is_empty() {
        return Err(miette!("Team secret cannot be empty."));
    }
    Ok(secret)
}

fn handle_generate(local_device_id: &str, local_pub: &[u8; 32]) -> Result<()> {
    let secret = load_team_secret()?;
    let invite = format_invite_v1(local_device_id, local_pub, secret.as_bytes());

    println!("[Available] Pairing invite (single line — copy to peer):");
    println!("{invite}");
    println!();
    println!("  Device ID (SoT): {local_device_id}");
    println!();
    println!("Peer steps:");
    println!(
        "  1. Same team secret via password manager / LEDGERFUL_SYNC_SECRET (prefer not pasting secret into chat with the invite)."
    );
    println!("  2. Run: ledgerful sync pair '<invite>'");
    println!("  3. Mutual trust: peer generates its invite; accept it here with the same secret.");
    println!();
    println!("Never sets [sync].enabled = true. See docs/team-sync.md.");
    Ok(())
}

fn handle_accept(
    sync_dir: &std::path::Path,
    layout: &crate::state::layout::Layout,
    local_device_id: &str,
    invite: &str,
    force: bool,
) -> Result<()> {
    let secret = load_team_secret()?;

    // Verify MAC (unified invalid/wrong-secret message from peers module).
    let (peer_id, peer_pub) =
        verify_invite(secret.as_bytes(), invite).map_err(|e| miette!("{e}"))?;

    // Path-safe before any FS write.
    validate_device_id_for_path(&peer_id).map_err(|e| miette!("{e}"))?;

    // Reject self-pair.
    if peer_id == local_device_id {
        return Err(miette!(
            "Cannot pair with this device's own invite (self-pair). Use the peer device's invite."
        ));
    }

    // Curve check is also inside trust_peer; keep explicit for clear error ordering.
    VerifyingKey::from_bytes(&peer_pub)
        .map_err(|e| miette!("Invalid peer public key in invite (curve check failed): {e}"))?;

    let outcome = trust_peer(sync_dir, &peer_id, &peer_pub, force).map_err(|e| miette!("{e}"))?;

    // Never writes enabled=true; report current config honestly.
    let config = crate::config::load::load_config(layout)?;

    let peers = list_peers(sync_dir).map_err(|e| miette!("{e}"))?;
    let peer_path = crate::sync::peers::peers_dir(sync_dir).join(format!("{peer_id}.pub"));

    match outcome {
        TrustOutcome::NewlyTrusted => {
            println!("[Available] Trusted peer '{peer_id}'.");
        }
        TrustOutcome::AlreadyTrusted => {
            println!("[Available] Peer '{peer_id}' is already trusted (same public key).");
        }
        TrustOutcome::Replaced => {
            println!("[Available] Replaced public key for peer '{peer_id}' (--force).");
        }
    }
    println!("  Peer key: {}", peer_path.display());
    println!("  Trusted peers: {}", peers.len());
    println!(
        "  [sync].enabled remains {} (pair never enables sync).",
        config.sync.enabled
    );
    println!("  For two-way sync, also accept this device's invite on the peer.");
    Ok(())
}
