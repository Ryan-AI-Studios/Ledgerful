use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use miette::{IntoDiagnostic, Result};
use rand::rngs::OsRng;
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};

pub fn get_keys_dir() -> Result<PathBuf> {
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .map(PathBuf::from)
        .map_err(|_| miette::miette!("Failed to locate home directory"))?;
    let keys_dir = home.join(".ledgerful").join("keys");
    if !keys_dir.exists() {
        fs::create_dir_all(&keys_dir).into_diagnostic()?;
    }
    Ok(keys_dir)
}

pub fn get_or_create_keys() -> Result<(SigningKey, VerifyingKey)> {
    let keys_dir = get_keys_dir()?;
    get_or_create_keys_in(&keys_dir)
}

pub fn get_or_create_keys_in(keys_dir: &Path) -> Result<(SigningKey, VerifyingKey)> {
    if !keys_dir.exists() {
        fs::create_dir_all(keys_dir).into_diagnostic()?;
    }

    let priv_path = keys_dir.join("private.key");
    let legacy_priv_path = keys_dir.join("private.pem");
    let pub_path = keys_dir.join("public.pem");

    // Active one-time migration: rename the misnamed legacy private key file
    // to the canonical name. We only rename when the target does not already
    // exist, so an existing `private.key` is never clobbered. If the rename
    // fails, we fall back to reading from the legacy path for this call so
    // the user's existing key is never lost.
    if legacy_priv_path.exists()
        && !priv_path.exists()
        && let Err(e) = fs::rename(&legacy_priv_path, &priv_path)
    {
        tracing::warn!(
            "Failed to rename legacy private key {:?} to {:?}: {e}. Reading from legacy path.",
            legacy_priv_path,
            priv_path
        );
    }

    // Resolve which private-key file to use. Prefer the canonical
    // `private.key`; fall back to the legacy `private.pem` only if the
    // rename failed (i.e. `private.key` still doesn't exist but the
    // legacy file does).
    let priv_file: Option<&Path> = if priv_path.exists() {
        Some(&priv_path)
    } else if legacy_priv_path.exists() {
        Some(&legacy_priv_path)
    } else {
        None
    };

    if let Some(priv_file) = priv_file {
        let signing_key = read_private_key(priv_file)?;

        // If the public key file is missing, derive it from the private
        // seed rather than minting a brand-new identity. This preserves
        // the user's existing signing identity even if `public.pem` was
        // lost or never written.
        let verifying_key = if pub_path.exists() {
            // Verify the stored public key matches the private seed.
            // If they disagree, trust the private seed (authoritative)
            // and rewrite the public key.
            let stored = read_public_key(&pub_path)?;
            if stored.to_bytes() == signing_key.verifying_key().to_bytes() {
                stored
            } else {
                tracing::warn!(
                    "Public key at {:?} does not match the private seed; regenerating.",
                    pub_path
                );
                let vk = signing_key.verifying_key();
                assert_within_state_root(&pub_path, keys_dir)?;
                fs::write(&pub_path, hex::encode(vk.to_bytes())).into_diagnostic()?;
                vk
            }
        } else {
            let vk = signing_key.verifying_key();
            assert_within_state_root(&pub_path, keys_dir)?;
            fs::write(&pub_path, hex::encode(vk.to_bytes())).into_diagnostic()?;
            vk
        };

        Ok((signing_key, verifying_key))
    } else {
        // No private key exists at all — fresh install. Generate a new
        // keypair and write both files.
        let mut csprng = OsRng;
        let mut bytes = [0u8; 32];
        use rand::RngCore;
        csprng.fill_bytes(&mut bytes);
        let signing_key = SigningKey::from_bytes(&bytes);
        let verifying_key = signing_key.verifying_key();

        let priv_hex = hex::encode(signing_key.to_bytes());
        let pub_hex = hex::encode(verifying_key.to_bytes());

        assert_within_state_root(&priv_path, keys_dir)?;
        fs::write(&priv_path, priv_hex).into_diagnostic()?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = std::fs::Permissions::from_mode(0o600);
            fs::set_permissions(&priv_path, perms).into_diagnostic()?;
        }
        assert_within_state_root(&pub_path, keys_dir)?;
        fs::write(&pub_path, pub_hex).into_diagnostic()?;

        Ok((signing_key, verifying_key))
    }
}

pub(crate) fn assert_within_state_root(path: &Path, root: &Path) -> Result<()> {
    let canonical_root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    // If the target already exists, canonicalize the full path to resolve
    // any symlink. A symlink pointing outside the root would canonicalize
    // to an external path and be rejected.
    let canonical_path = if path.exists() {
        path.canonicalize()
            .unwrap_or_else(|_| parent_plus_name(path))
    } else {
        parent_plus_name(path)
    };
    if !canonical_path.starts_with(&canonical_root) {
        return Err(miette::miette!(
            "Refusing to write key file {:?} outside state root {:?}",
            path,
            root
        ));
    }
    Ok(())
}

fn parent_plus_name(path: &Path) -> PathBuf {
    if let Some(parent) = path.parent() {
        let mut canon = parent
            .canonicalize()
            .unwrap_or_else(|_| parent.to_path_buf());
        if let Some(name) = path.file_name() {
            canon.push(name);
        }
        canon
    } else {
        path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
    }
}

fn read_private_key(path: &Path) -> Result<SigningKey> {
    let priv_bytes = fs::read(path).into_diagnostic()?;
    let priv_str = String::from_utf8(priv_bytes).into_diagnostic()?;
    let priv_decoded = hex::decode(priv_str.trim()).into_diagnostic()?;
    let priv_array: [u8; 32] = priv_decoded
        .try_into()
        .map_err(|_| miette::miette!("Invalid private key size"))?;
    Ok(SigningKey::from_bytes(&priv_array))
}

fn read_public_key(path: &Path) -> Result<VerifyingKey> {
    let pub_bytes = fs::read(path).into_diagnostic()?;
    let pub_str = String::from_utf8(pub_bytes).into_diagnostic()?;
    let pub_decoded = hex::decode(pub_str.trim()).into_diagnostic()?;
    let pub_array: [u8; 32] = pub_decoded
        .try_into()
        .map_err(|_| miette::miette!("Invalid public key size"))?;
    VerifyingKey::from_bytes(&pub_array).into_diagnostic()
}

pub fn verify_keypair_consistency(signing_key: &SigningKey, verifying_key: &VerifyingKey) -> bool {
    signing_key.verifying_key().to_bytes() == verifying_key.to_bytes()
}

pub fn sign_ledger_entry(
    tx_id: &str,
    category: &str,
    summary: &str,
    reason: &str,
    committed_at: &str,
) -> Result<(Option<String>, Option<String>)> {
    let keys_dir = get_keys_dir()?;
    sign_ledger_entry_in(&keys_dir, tx_id, category, summary, reason, committed_at)
}

pub fn sign_ledger_entry_in(
    keys_dir: &Path,
    tx_id: &str,
    category: &str,
    summary: &str,
    reason: &str,
    committed_at: &str,
) -> Result<(Option<String>, Option<String>)> {
    let (signing_key, verifying_key) = get_or_create_keys_in(keys_dir)?;

    if !verify_keypair_consistency(&signing_key, &verifying_key) {
        return Err(miette::miette!(
            "Keypair consistency check failed: public key does not match private seed. Refusing to sign."
        ));
    }

    let payload = format!(
        "tx_id:{}\ncategory:{}\nsummary:{}\nreason:{}\ncommitted_at:{}",
        tx_id, category, summary, reason, committed_at
    );

    let signature = signing_key.sign(payload.as_bytes());

    let sig_hex = hex::encode(signature.to_bytes());
    let pub_hex = hex::encode(verifying_key.to_bytes());

    Ok((Some(sig_hex), Some(pub_hex)))
}

/// Deterministic entry hash that commits to the entry's identity and its chain
/// linkage. The hash is outside the Ed25519 signing basis and is used for the
/// `prev_hash` column and the signed chain head.
///
/// Canonical input: `tx_id || signature_hex || prev_hash` where `prev_hash` is
/// the previous head's `latest_entry_hash`, or the empty string for the genesis
/// entry.
pub fn compute_entry_hash(tx_id: &str, signature_hex: &str, prev_hash: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(tx_id.as_bytes());
    hasher.update(signature_hex.as_bytes());
    hasher.update(prev_hash.as_bytes());
    hex::encode(hasher.finalize())
}

/// Canonical chain-head signing payload, shared between signing and
/// verification so the format cannot drift.
fn chain_head_payload(latest_entry_hash: &str, genesis: &str, length: i64) -> String {
    format!(
        "chain_head:{}\nlatest_entry_hash:{}\ngenesis:{}\nlength:{}",
        "chain_head", latest_entry_hash, genesis, length
    )
}

pub fn sign_chain_head(
    keys_dir: &Path,
    latest_entry_hash: &str,
    genesis: &str,
    length: i64,
) -> Result<(Option<String>, Option<String>)> {
    let (signing_key, verifying_key) = get_or_create_keys_in(keys_dir)?;

    if !verify_keypair_consistency(&signing_key, &verifying_key) {
        return Err(miette::miette!(
            "Keypair consistency check failed: public key does not match private seed. Refusing to sign chain head."
        ));
    }

    let payload = chain_head_payload(latest_entry_hash, genesis, length);

    let signature = signing_key.sign(payload.as_bytes());
    let sig_hex = hex::encode(signature.to_bytes());
    let pub_hex = hex::encode(verifying_key.to_bytes());

    Ok((Some(sig_hex), Some(pub_hex)))
}

pub fn verify_chain_head(
    latest_entry_hash: &str,
    genesis: &str,
    length: i64,
    signature_hex: &str,
    public_key_hex: &str,
) -> bool {
    verify_chain_head_with_result(
        latest_entry_hash,
        genesis,
        length,
        signature_hex,
        public_key_hex,
    )
    .is_ok()
}

fn verify_chain_head_with_result(
    latest_entry_hash: &str,
    genesis: &str,
    length: i64,
    signature_hex: &str,
    public_key_hex: &str,
) -> Result<(), ChainHeadVerifyError> {
    let payload = chain_head_payload(latest_entry_hash, genesis, length);

    let pub_decoded =
        hex::decode(public_key_hex).map_err(|_| ChainHeadVerifyError::InvalidPublicKey)?;
    let pub_array: [u8; 32] = pub_decoded
        .try_into()
        .map_err(|_| ChainHeadVerifyError::InvalidPublicKey)?;
    let verifying_key =
        VerifyingKey::from_bytes(&pub_array).map_err(|_| ChainHeadVerifyError::InvalidPublicKey)?;

    let sig_decoded =
        hex::decode(signature_hex).map_err(|_| ChainHeadVerifyError::InvalidSignature)?;
    let sig_array: [u8; 64] = sig_decoded
        .try_into()
        .map_err(|_| ChainHeadVerifyError::InvalidSignature)?;
    let signature = Signature::from_bytes(&sig_array);

    verifying_key
        .verify(payload.as_bytes(), &signature)
        .map_err(|_| ChainHeadVerifyError::VerificationFailed)?;
    Ok(())
}

pub fn verify_signature(
    tx_id: &str,
    category: &str,
    summary: &str,
    reason: &str,
    committed_at: &str,
    signature_hex: &str,
    public_key_hex: &str,
) -> bool {
    verify_signature_with_result(
        tx_id,
        category,
        summary,
        reason,
        committed_at,
        signature_hex,
        public_key_hex,
    )
    .is_ok()
}

fn verify_signature_with_result(
    tx_id: &str,
    category: &str,
    summary: &str,
    reason: &str,
    committed_at: &str,
    signature_hex: &str,
    public_key_hex: &str,
) -> Result<(), SignatureVerifyError> {
    let pub_bytes =
        hex::decode(public_key_hex).map_err(|_| SignatureVerifyError::InvalidPublicKeyEncoding)?;
    let pub_array: [u8; 32] = pub_bytes
        .try_into()
        .map_err(|_| SignatureVerifyError::InvalidPublicKeyLength)?;
    let verifying_key = VerifyingKey::from_bytes(&pub_array)
        .map_err(|_| SignatureVerifyError::InvalidPublicKeyMaterial)?;

    let sig_bytes =
        hex::decode(signature_hex).map_err(|_| SignatureVerifyError::InvalidSignatureEncoding)?;
    let sig_array: [u8; 64] = sig_bytes
        .try_into()
        .map_err(|_| SignatureVerifyError::InvalidSignatureLength)?;
    let signature = Signature::from_bytes(&sig_array);

    let payload = format!(
        "tx_id:{}\ncategory:{}\nsummary:{}\nreason:{}\ncommitted_at:{}",
        tx_id, category, summary, reason, committed_at
    );

    verifying_key
        .verify(payload.as_bytes(), &signature)
        .map_err(|_| SignatureVerifyError::SignatureMismatch)?;

    Ok(())
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
enum SignatureVerifyError {
    #[error("public key is not valid hex")]
    InvalidPublicKeyEncoding,
    #[error("public key has unexpected length")]
    InvalidPublicKeyLength,
    #[error("public key bytes are not a valid Ed25519 verifying key")]
    InvalidPublicKeyMaterial,
    #[error("signature is not valid hex")]
    InvalidSignatureEncoding,
    #[error("signature has unexpected length")]
    InvalidSignatureLength,
    #[error("signature does not verify against stored payload")]
    SignatureMismatch,
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
enum ChainHeadVerifyError {
    #[error("chain head public key is not valid")]
    InvalidPublicKey,
    #[error("chain head signature is not valid")]
    InvalidSignature,
    #[error("chain head signature does not verify")]
    VerificationFailed,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_private_key_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let seed = [1u8; 32];
        let path = tmp.path().join("private.key");
        fs::write(&path, hex::encode(seed)).unwrap();
        let key = read_private_key(&path).unwrap();
        assert_eq!(key.to_bytes(), seed);
    }

    fn temp_keys_dir() -> (tempfile::TempDir, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let keys_dir = temp.path().join(".ledgerful").join("keys");
        fs::create_dir_all(&keys_dir).unwrap();
        (temp, keys_dir)
    }

    #[test]
    fn assert_within_state_root_rejects_escape() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join(".ledgerful").join("keys");
        fs::create_dir_all(&root).unwrap();
        let escaped = tmp.path().join(".ledgerful").join("evil.key");
        let result = assert_within_state_root(&escaped, &root);
        assert!(result.is_err());
    }

    #[test]
    fn assert_within_state_root_rejects_symlink_escape() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join(".ledgerful").join("keys");
        fs::create_dir_all(&root).unwrap();

        // Create a symlink inside root pointing outside
        let outside = tmp.path().join("outside.txt");
        fs::write(&outside, "escaped").unwrap();
        let symlink_path = root.join("public.pem");

        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&outside, &symlink_path).unwrap();
            let result = assert_within_state_root(&symlink_path, &root);
            assert!(
                result.is_err(),
                "symlink pointing outside root must be rejected"
            );
        }
        // On Windows, symlinks require admin privileges — skip the symlink
        // assertion. The canonicalize-on-exists logic still protects on
        // Windows; we just can't test it without admin.
        #[cfg(not(unix))]
        {
            fs::write(&symlink_path, "ok").unwrap();
            let result = assert_within_state_root(&symlink_path, &root);
            assert!(result.is_ok(), "existing file inside root must pass");
        }
    }

    #[test]
    fn legacy_private_pem_is_migrated_to_private_key() {
        let (_temp, keys_dir) = temp_keys_dir();

        let seed = [2u8; 32];
        let signing_key = SigningKey::from_bytes(&seed);
        let verifying_key = signing_key.verifying_key();

        fs::write(keys_dir.join("private.pem"), hex::encode(seed)).unwrap();
        fs::write(
            keys_dir.join("public.pem"),
            hex::encode(verifying_key.to_bytes()),
        )
        .unwrap();

        let (loaded_signing, loaded_verifying) = get_or_create_keys_in(&keys_dir).unwrap();

        assert_eq!(loaded_signing.to_bytes(), seed);
        assert_eq!(loaded_verifying.to_bytes(), verifying_key.to_bytes());

        assert!(
            keys_dir.join("private.key").exists(),
            "private.key should exist after migration"
        );
        assert!(
            !keys_dir.join("private.pem").exists(),
            "private.pem should be removed after migration"
        );
        assert!(keys_dir.join("public.pem").exists());
    }

    #[test]
    fn fresh_install_creates_private_key_only() {
        let (_temp, keys_dir) = temp_keys_dir();

        assert!(!keys_dir.join("private.pem").exists());
        assert!(!keys_dir.join("private.key").exists());

        let (signing_key, verifying_key) = get_or_create_keys_in(&keys_dir).unwrap();

        assert!(keys_dir.join("private.key").exists());
        assert!(!keys_dir.join("private.pem").exists());
        assert!(keys_dir.join("public.pem").exists());

        assert_eq!(
            verifying_key.to_bytes(),
            signing_key.verifying_key().to_bytes()
        );
    }

    #[test]
    fn verify_keypair_consistency_true_for_matching_pair() {
        let signing_key = SigningKey::from_bytes(&[21u8; 32]);
        let verifying_key = signing_key.verifying_key();
        assert!(verify_keypair_consistency(&signing_key, &verifying_key));
    }

    #[test]
    fn verify_keypair_consistency_false_for_mismatched_pair() {
        let signing_key = SigningKey::from_bytes(&[21u8; 32]);
        let other_signing_key = SigningKey::from_bytes(&[22u8; 32]);
        let verifying_key = other_signing_key.verifying_key();
        assert!(!verify_keypair_consistency(&signing_key, &verifying_key));
    }
}
