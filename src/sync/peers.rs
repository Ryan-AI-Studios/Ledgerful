//! Peer trust store and pairing-invite MAC helpers (track 0111).
//!
//! Wire format (v1):
//! ```text
//! LF-PAIR-1.<device_id>.<b64url_nopad(pub32)>.<b64url_nopad(mac_tag16)>
//! ```
//!
//! KDF / MAC:
//! - `key = blake3::derive_key("ledgerful team-sync pair v1", secret)`
//! - `msg = b"pair-invite-v1\0" || device_id || pub_key` (pub always 32 bytes last)
//! - `tag = keyed_hash(key, msg)[0..16]` verified with `ct_eq`
//!
//! Peer SoT is filesystem: `{sync_dir}/peers/{device_id}.pub`.

use crate::sync::crypto::ct_eq;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use ed25519_dalek::VerifyingKey;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use zeroize::Zeroizing;

/// Invite version prefix (literal).
pub const INVITE_PREFIX: &str = "LF-PAIR-1";

/// Domain-separated KDF context for pair invite keys.
pub const PAIR_KDF_CONTEXT: &str = "ledgerful team-sync pair v1";

/// MAC message version marker (includes trailing NUL).
pub const PAIR_MAC_MSG_PREFIX: &[u8] = b"pair-invite-v1\0";

/// Wire MAC tag length (first 16 bytes of full blake3 keyed hash).
pub const PAIR_TAG_LEN: usize = 16;

/// Parsed pairing invite (pre-MAC verification).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedInvite {
    pub device_id: String,
    pub pub_key: [u8; 32],
    pub tag: [u8; PAIR_TAG_LEN],
}

/// Result of persisting a peer public key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrustOutcome {
    /// New peer file written.
    NewlyTrusted,
    /// Same device_id + same pubkey already present.
    AlreadyTrusted,
    /// Same device_id replaced under `--force`.
    Replaced,
}

/// Derive a 32-byte pair-invite key from the team secret.
///
/// Wrapped in [`Zeroizing`] so the derived key is wiped on drop (same pattern as
/// [`crate::sync::crypto::derive_bundle_key`]).
pub fn derive_pair_key(secret: &[u8]) -> Zeroizing<[u8; 32]> {
    Zeroizing::new(blake3::derive_key(PAIR_KDF_CONTEXT, secret))
}

/// Full 32-byte keyed MAC over the pair-invite message.
///
/// Message: `b"pair-invite-v1\0" || device_id.as_bytes() || pub_key` (32-byte pub last).
pub fn mac_pair_invite(key: &[u8; 32], device_id: &str, pub_key: &[u8; 32]) -> blake3::Hash {
    let mut msg = Vec::with_capacity(PAIR_MAC_MSG_PREFIX.len() + device_id.len() + 32);
    msg.extend_from_slice(PAIR_MAC_MSG_PREFIX);
    msg.extend_from_slice(device_id.as_bytes());
    msg.extend_from_slice(pub_key);
    blake3::keyed_hash(key, &msg)
}

/// First 16 bytes of the pair invite MAC.
pub fn pair_tag(mac: &blake3::Hash) -> [u8; PAIR_TAG_LEN] {
    let mut tag = [0u8; PAIR_TAG_LEN];
    tag.copy_from_slice(&mac.as_bytes()[..PAIR_TAG_LEN]);
    tag
}

/// Format a v1 pairing invite string from raw fields (no secret).
pub fn format_invite(device_id: &str, pub_key: &[u8; 32], tag: &[u8; PAIR_TAG_LEN]) -> String {
    format!(
        "{INVITE_PREFIX}.{device_id}.{}.{}",
        URL_SAFE_NO_PAD.encode(pub_key),
        URL_SAFE_NO_PAD.encode(tag)
    )
}

/// Format a v1 pairing invite under `secret` (derive + MAC + encode).
pub fn format_invite_v1(device_id: &str, pub_key: &[u8; 32], secret: &[u8]) -> String {
    let key = derive_pair_key(secret);
    let mac = mac_pair_invite(&key, device_id, pub_key);
    let tag = pair_tag(&mac);
    format_invite(device_id, pub_key, &tag)
}

/// Parse an invite string into fields (no MAC verification).
pub fn parse_invite(invite: &str) -> Result<ParsedInvite, String> {
    let invite = invite.trim();
    let parts: Vec<&str> = invite.split('.').collect();
    if parts.len() != 4 {
        return Err(invalid_invite_msg());
    }
    if parts[0] != INVITE_PREFIX {
        return Err(format!(
            "Unsupported pairing invite version '{}'. Expected '{INVITE_PREFIX}'. Upgrade Ledgerful or re-generate the invite.",
            parts[0]
        ));
    }
    let device_id = parts[1].to_string();
    if device_id.is_empty() {
        return Err(invalid_invite_msg());
    }

    let pub_raw = URL_SAFE_NO_PAD
        .decode(parts[2])
        .map_err(|_| invalid_invite_msg())?;
    if pub_raw.len() != 32 {
        return Err(invalid_invite_msg());
    }
    let mut pub_key = [0u8; 32];
    pub_key.copy_from_slice(&pub_raw);

    let tag_raw = URL_SAFE_NO_PAD
        .decode(parts[3])
        .map_err(|_| invalid_invite_msg())?;
    if tag_raw.len() != PAIR_TAG_LEN {
        return Err(invalid_invite_msg());
    }
    let mut tag = [0u8; PAIR_TAG_LEN];
    tag.copy_from_slice(&tag_raw);

    Ok(ParsedInvite {
        device_id,
        pub_key,
        tag,
    })
}

/// Verify invite MAC under `secret` and return `(device_id, pub_key)`.
///
/// Crypto failures use a unified message (no wrong-secret vs bad-MAC oracle).
pub fn verify_invite(secret: &[u8], invite: &str) -> Result<(String, [u8; 32]), String> {
    let parsed = parse_invite(invite)?;
    let key = derive_pair_key(secret);
    let mac = mac_pair_invite(&key, &parsed.device_id, &parsed.pub_key);
    let expected = pair_tag(&mac);
    if !ct_eq(&expected, &parsed.tag) {
        return Err(invalid_invite_msg());
    }
    Ok((parsed.device_id, parsed.pub_key))
}

fn invalid_invite_msg() -> String {
    "Invalid pairing invite or wrong team secret.".to_string()
}

/// Path-safe device_id for use as a peer filename component.
///
/// Rejects empty, `unknown`, `.`, `..`, `/`, `\`, NUL, and any `.` inside the id.
pub fn validate_device_id_for_path(device_id: &str) -> Result<(), String> {
    if device_id.is_empty() {
        return Err("device_id cannot be empty.".to_string());
    }
    if device_id == "unknown" {
        return Err("device_id 'unknown' is not allowed.".to_string());
    }
    if device_id == "." || device_id == ".." {
        return Err("device_id cannot be '.' or '..'.".to_string());
    }
    if device_id.contains('/')
        || device_id.contains('\\')
        || device_id.contains('\0')
        || device_id.contains('.')
    {
        return Err(
            "device_id contains path-unsafe characters ('.', '/', '\\\\', or NUL).".to_string(),
        );
    }
    Ok(())
}

/// `{sync_dir}/peers`
pub fn peers_dir(sync_dir: &Path) -> PathBuf {
    sync_dir.join("peers")
}

/// Load trusted peer public keys from `peers/*.pub` only (not self).
///
/// Malformed files (wrong length or invalid curve point) are **skipped** with a
/// warning — never `copy_from_slice` panic. IO errors on directory read propagate.
pub fn load_peer_keys(sync_dir: &Path) -> Result<HashMap<String, [u8; 32]>, String> {
    let mut map = HashMap::new();
    let dir = peers_dir(sync_dir);
    if !dir.exists() {
        return Ok(map);
    }
    let entries = std::fs::read_dir(&dir).map_err(|e| format!("Failed to read peers dir: {e}"))?;
    let mut names: Vec<(String, PathBuf)> = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| format!("Failed to read peers dir entry: {e}"))?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if let Some(peer_id) = name.strip_suffix(".pub") {
            // Temp names must not match `*.pub` — e.g. `.{id}.pub.tmp` is ignored here.
            // Skip path-unsafe stems (empty, `unknown`, `.`/`..`, slash/backslash, etc.).
            if let Err(msg) = validate_device_id_for_path(peer_id) {
                eprintln!(
                    "Warning: skipping peer file with path-unsafe id {}: {msg}",
                    path.display()
                );
                continue;
            }
            names.push((peer_id.to_string(), path));
        }
    }
    // Deterministic load order.
    names.sort_by(|a, b| a.0.cmp(&b.0));
    for (peer_id, path) in names {
        match load_one_peer_pub(&path) {
            Ok(key) => {
                map.insert(peer_id, key);
            }
            Err(msg) => {
                eprintln!(
                    "Warning: skipping untrusted peer file {}: {msg}",
                    path.display()
                );
            }
        }
    }
    Ok(map)
}

fn load_one_peer_pub(path: &Path) -> Result<[u8; 32], String> {
    let bytes = std::fs::read(path).map_err(|e| format!("read failed: {e}"))?;
    if bytes.len() != 32 {
        return Err(format!("expected 32 bytes, got {}", bytes.len()));
    }
    let mut key = [0u8; 32];
    key.copy_from_slice(&bytes);
    VerifyingKey::from_bytes(&key).map_err(|e| format!("invalid Ed25519 public key: {e}"))?;
    Ok(key)
}

/// Persist a peer public key under `peers/{device_id}.pub`.
///
/// Path-validates `device_id` **before** any write. Writes via a temp name that
/// does **not** match `*.pub` (`.{device_id}.pub.tmp`), then renames.
pub fn trust_peer(
    sync_dir: &Path,
    device_id: &str,
    pub_key: &[u8; 32],
    force: bool,
) -> Result<TrustOutcome, String> {
    validate_device_id_for_path(device_id)?;
    VerifyingKey::from_bytes(pub_key)
        .map_err(|e| format!("Invalid peer public key (curve check failed): {e}"))?;

    let dir = peers_dir(sync_dir);
    std::fs::create_dir_all(&dir).map_err(|e| format!("Failed to create peers dir: {e}"))?;

    let final_path = dir.join(format!("{device_id}.pub"));
    let mut outcome = TrustOutcome::NewlyTrusted;

    if final_path.exists() {
        let existing = std::fs::read(&final_path)
            .map_err(|e| format!("Failed to read existing peer key: {e}"))?;
        if existing.len() == 32 {
            let mut existing_key = [0u8; 32];
            existing_key.copy_from_slice(&existing);
            if existing_key == *pub_key {
                return Ok(TrustOutcome::AlreadyTrusted);
            }
        }
        if !force {
            return Err(format!(
                "Peer '{device_id}' is already trusted with a different public key. \
Use --force to replace the peer key (re-pair)."
            ));
        }
        outcome = TrustOutcome::Replaced;
    }

    // Temp must NOT match `*.pub` so a crash mid-write cannot pollute the trust glob.
    let temp_path = dir.join(format!(".{device_id}.pub.tmp"));
    std::fs::write(&temp_path, pub_key).map_err(|e| format!("Failed to write peer temp: {e}"))?;

    // Windows: rename over existing may fail — remove first when replacing.
    if final_path.exists() {
        std::fs::remove_file(&final_path)
            .map_err(|e| format!("Failed to remove previous peer key: {e}"))?;
    }
    std::fs::rename(&temp_path, &final_path)
        .map_err(|e| format!("Failed to finalize peer key: {e}"))?;

    Ok(outcome)
}

/// Delete `peers/{device_id}.pub`. Missing peer → error.
pub fn revoke_peer(sync_dir: &Path, device_id: &str) -> Result<(), String> {
    validate_device_id_for_path(device_id)?;
    let path = peers_dir(sync_dir).join(format!("{device_id}.pub"));
    if !path.exists() {
        return Err(format!(
            "Peer '{device_id}' is not trusted (no file at {}).",
            path.display()
        ));
    }
    std::fs::remove_file(&path).map_err(|e| format!("Failed to revoke peer '{device_id}': {e}"))?;
    Ok(())
}

/// List trusted peer device_ids (sorted).
pub fn list_peers(sync_dir: &Path) -> Result<Vec<String>, String> {
    let keys = load_peer_keys(sync_dir)?;
    let mut ids: Vec<String> = keys.into_keys().collect();
    ids.sort();
    Ok(ids)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;
    use rand::rngs::OsRng;
    use tempfile::tempdir;

    fn sample_keypair() -> ([u8; 32], [u8; 32]) {
        let signing = SigningKey::generate(&mut OsRng);
        (signing.to_bytes(), signing.verifying_key().to_bytes())
    }

    #[test]
    fn invite_round_trip_same_secret() {
        let secret = b"team-secret-high-entropy-test-material";
        let (_sk, pk) = sample_keypair();
        let device_id = "device-aabbccdd";
        let invite = format_invite_v1(device_id, &pk, secret);
        assert!(invite.starts_with("LF-PAIR-1."));
        assert!(!invite.contains('='), "must be URL_SAFE_NO_PAD");
        let (got_id, got_pk) = verify_invite(secret, &invite).expect("verify");
        assert_eq!(got_id, device_id);
        assert_eq!(got_pk, pk);
    }

    #[test]
    fn wrong_secret_fails() {
        let (_sk, pk) = sample_keypair();
        let invite = format_invite_v1("device-aabbccdd", &pk, b"secret-a");
        let err = verify_invite(b"secret-b", &invite).expect_err("wrong secret");
        assert!(err.to_lowercase().contains("invalid") || err.to_lowercase().contains("secret"));
    }

    #[test]
    fn tampered_pub_or_tag_fails() {
        let secret = b"team-secret";
        let (_sk, pk) = sample_keypair();
        let invite = format_invite_v1("device-aabbccdd", &pk, secret);
        // Flip a character in the pub field (part index 2).
        let parts: Vec<&str> = invite.split('.').collect();
        let mut bad_pub = parts[2].to_string();
        let flip = if bad_pub.starts_with('A') { 'B' } else { 'A' };
        bad_pub.replace_range(0..1, &flip.to_string());
        let tampered = format!("{}.{}.{}.{}", parts[0], parts[1], bad_pub, parts[3]);
        assert!(verify_invite(secret, &tampered).is_err());

        // Tag tamper
        let mut bad_tag = parts[3].to_string();
        let flip2 = if bad_tag.starts_with('A') { 'B' } else { 'A' };
        bad_tag.replace_range(0..1, &flip2.to_string());
        let tampered_tag = format!("{}.{}.{}.{}", parts[0], parts[1], parts[2], bad_tag);
        assert!(verify_invite(secret, &tampered_tag).is_err());
    }

    #[test]
    fn path_unsafe_device_ids_rejected() {
        for id in [
            "", "unknown", ".", "..", "../x", "a/b", "a\\b", "a.b", "dev\0ice",
        ] {
            assert!(
                validate_device_id_for_path(id).is_err(),
                "should reject {id:?}"
            );
        }
        assert!(validate_device_id_for_path("device-aabbccdd").is_ok());
    }

    #[test]
    fn trust_rejects_path_unsafe_before_write() {
        let tmp = tempdir().unwrap();
        let sync_dir = tmp.path().join("sync");
        std::fs::create_dir_all(&sync_dir).unwrap();
        let (_sk, pk) = sample_keypair();
        let err = trust_peer(&sync_dir, "../evil", &pk, false).unwrap_err();
        assert!(
            err.to_lowercase().contains("path")
                || err.to_lowercase().contains("unsafe")
                || err.to_lowercase().contains('.')
        );
        assert!(
            !peers_dir(&sync_dir).exists()
                || std::fs::read_dir(peers_dir(&sync_dir)).unwrap().count() == 0
        );
    }

    /// 32-byte encoding whose Edwards y-coordinate is not on the curve.
    ///
    /// All-zero is **accepted** by ed25519-dalek 2.x (ZIP-215); y=2 is not a
    /// square residue and fails `CompressedEdwardsY::decompress`.
    fn invalid_curve_pub_fixture() -> [u8; 32] {
        let mut bad = [0u8; 32];
        bad[0] = 2;
        bad
    }

    #[test]
    fn bad_curve_point_rejected_on_trust() {
        let tmp = tempdir().unwrap();
        let sync_dir = tmp.path().join("sync");
        std::fs::create_dir_all(&sync_dir).unwrap();
        let bad = invalid_curve_pub_fixture();
        assert!(
            VerifyingKey::from_bytes(&bad).is_err(),
            "fixture must be invalid under ed25519-dalek 2.x"
        );
        let result = trust_peer(&sync_dir, "device-badcurve", &bad, false);
        assert!(
            result.is_err(),
            "trust_peer must reject invalid curve point, got: {result:?}"
        );
        assert!(!peers_dir(&sync_dir).join("device-badcurve.pub").exists());
    }

    #[test]
    fn load_peer_keys_skips_unsafe_stems_and_does_not_insert_self() {
        let tmp = tempdir().unwrap();
        let sync_dir = tmp.path().join("sync");
        let dir = peers_dir(&sync_dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (_sk, pk) = sample_keypair();

        // Valid trusted peer under peers/.
        assert_eq!(
            trust_peer(&sync_dir, "device-good", &pk, false).unwrap(),
            TrustOutcome::NewlyTrusted
        );

        // device.pub outside peers/ must never be loaded by load_peer_keys.
        std::fs::write(sync_dir.join("device.pub"), pk).unwrap();

        // Path-unsafe stems under peers/: skipped (not inserted).
        std::fs::write(dir.join("unknown.pub"), pk).unwrap();
        // Empty stem after strip_suffix(".pub") → rejected by validate_device_id_for_path.
        std::fs::write(dir.join(".pub"), pk).unwrap();

        let map = load_peer_keys(&sync_dir).unwrap();
        assert!(
            map.contains_key("device-good"),
            "valid peer must load: {map:?}"
        );
        assert!(
            !map.contains_key("unknown"),
            "unknown.pub stem must be skipped: {map:?}"
        );
        assert!(!map.contains_key(""), "empty stem must be skipped: {map:?}");
        // load_peer_keys never auto-inserts self (self-insert is call-site only).
        assert_eq!(
            map.len(),
            1,
            "only path-safe peers/*.pub entries; self not auto-inserted: {map:?}"
        );
    }

    #[test]
    fn mac_construction_differs_from_old_hash_secret_concat_pub() {
        let secret = b"team-secret";
        let (_sk, pk) = sample_keypair();
        let device_id = "device-aabbccdd";

        // Old provisional: blake3::hash(secret || pub) truncated hex — not keyed.
        let mut old_input = Vec::new();
        old_input.extend_from_slice(secret);
        old_input.extend_from_slice(&pk);
        let old_hash = blake3::hash(&old_input);
        let old_tag: [u8; 16] = old_hash.as_bytes()[..16].try_into().unwrap();

        let key = derive_pair_key(secret);
        let mac = mac_pair_invite(&key, device_id, &pk);
        let new_tag = pair_tag(&mac);

        assert_ne!(
            old_tag, new_tag,
            "keyed pair MAC must not match old hash(secret||pub) truncation"
        );
    }

    #[test]
    fn trust_and_list_and_revoke_round_trip() {
        let tmp = tempdir().unwrap();
        let sync_dir = tmp.path().join("sync");
        std::fs::create_dir_all(&sync_dir).unwrap();
        let (_sk, pk) = sample_keypair();
        let id = "device-aabbccdd";

        assert_eq!(
            trust_peer(&sync_dir, id, &pk, false).unwrap(),
            TrustOutcome::NewlyTrusted
        );
        assert_eq!(
            trust_peer(&sync_dir, id, &pk, false).unwrap(),
            TrustOutcome::AlreadyTrusted
        );

        let peers = list_peers(&sync_dir).unwrap();
        assert_eq!(peers, vec![id.to_string()]);

        let map = load_peer_keys(&sync_dir).unwrap();
        assert_eq!(map.get(id), Some(&pk));

        // Different key without force fails.
        let (_sk2, pk2) = sample_keypair();
        assert!(trust_peer(&sync_dir, id, &pk2, false).is_err());
        assert_eq!(
            trust_peer(&sync_dir, id, &pk2, true).unwrap(),
            TrustOutcome::Replaced
        );
        let map = load_peer_keys(&sync_dir).unwrap();
        assert_eq!(map.get(id), Some(&pk2));

        revoke_peer(&sync_dir, id).unwrap();
        assert!(list_peers(&sync_dir).unwrap().is_empty());
        assert!(!load_peer_keys(&sync_dir).unwrap().contains_key(id));
    }

    #[test]
    fn temp_pub_tmp_not_trusted_by_load() {
        let tmp = tempdir().unwrap();
        let sync_dir = tmp.path().join("sync");
        let dir = peers_dir(&sync_dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (_sk, pk) = sample_keypair();
        // Incomplete temp must not match *.pub
        let temp = dir.join(".device-aabbccdd.pub.tmp");
        std::fs::write(&temp, pk).unwrap();
        let map = load_peer_keys(&sync_dir).unwrap();
        assert!(map.is_empty(), "temp must not be trusted: {map:?}");
    }

    #[test]
    fn malformed_peer_file_no_panic() {
        let tmp = tempdir().unwrap();
        let sync_dir = tmp.path().join("sync");
        let dir = peers_dir(&sync_dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("device-short.pub"), [1u8; 16]).unwrap();
        let map = load_peer_keys(&sync_dir).unwrap();
        assert!(!map.contains_key("device-short"));
    }

    #[test]
    fn wrong_version_prefix_soft_fail() {
        let err = parse_invite("LF-PAIR-2.device-x.aaaa.bbbb").unwrap_err();
        assert!(
            err.contains("Unsupported") || err.contains("version"),
            "got: {err}"
        );
    }
}
