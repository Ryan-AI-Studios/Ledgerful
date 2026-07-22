use crate::ledger::types::{Category, ChangeType, EntryType, LedgerEntry};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use miette::{IntoDiagnostic, Result};
use rand::rngs::OsRng;
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use unicode_normalization::UnicodeNormalization;

/// Legacy five-field Ed25519 payload (historical rows only).
pub const LEDGER_SIG_VERSION_V1: u32 = 1;
/// Full provenance Ed25519 payload (new commits).
pub const LEDGER_SIG_VERSION_V2: u32 = 2;
/// Production sign version for new commits.
pub const CURRENT_LEDGER_SIG_VERSION: u32 = LEDGER_SIG_VERSION_V2;

/// Classification of a signature relative to the trusted-key pin list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignatureTrustStatus {
    /// Crypto-valid and public key is in `intent.trusted_public_keys`.
    ValidTrusted,
    /// Crypto-valid but public key is not pinned (or pin list empty).
    ValidUnknownKey,
    /// Signature does not verify, version rejected, or consistency failure.
    Invalid,
    /// No signature present.
    Unsigned,
}

impl SignatureTrustStatus {
    /// Frozen CLI / dashboard vocabulary (0072 / 0074 / 0076).
    pub fn as_str(self) -> &'static str {
        match self {
            SignatureTrustStatus::ValidTrusted => "VALID (trusted)",
            SignatureTrustStatus::ValidUnknownKey => "VALID (unknown key)",
            SignatureTrustStatus::Invalid => "INVALID",
            SignatureTrustStatus::Unsigned => "UNSIGNED",
        }
    }

    pub fn is_crypto_valid(self) -> bool {
        matches!(
            self,
            SignatureTrustStatus::ValidTrusted | SignatureTrustStatus::ValidUnknownKey
        )
    }
}

/// All fields required to encode/sign/verify a v2 (or v1) ledger entry payload.
#[derive(Debug, Clone)]
pub struct LedgerSignInput {
    pub sig_version: u32,
    pub tx_id: String,
    pub category: String,
    pub summary: String,
    pub reason: String,
    pub committed_at: String,
    pub entity: String,
    pub change_type: String,
    pub entry_type: String,
    pub author: String,
    pub risk: String,
    pub is_breaking: bool,
    pub related_tickets: String,
    pub origin: String,
    /// Not signed; used for derive-at-verify consistency (v2 only).
    pub entity_normalized: String,
}

impl LedgerSignInput {
    pub fn from_entry(entry: &LedgerEntry) -> Self {
        Self {
            sig_version: entry.sig_version,
            tx_id: entry.tx_id.clone(),
            category: entry.category.to_string(),
            summary: entry.summary.clone(),
            reason: entry.reason.clone(),
            committed_at: entry.committed_at.clone(),
            entity: entry.entity.clone(),
            change_type: entry.change_type.to_string(),
            entry_type: entry.entry_type.to_string(),
            author: entry.author.clone(),
            risk: entry.risk.clone().unwrap_or_default(),
            is_breaking: entry.is_breaking,
            related_tickets: entry.related_tickets.clone().unwrap_or_default(),
            origin: entry.origin.clone(),
            entity_normalized: entry.entity_normalized.clone(),
        }
    }

    /// Build a v2 input for a new production commit.
    #[allow(clippy::too_many_arguments)]
    pub fn for_new_commit(
        tx_id: &str,
        category: Category,
        summary: &str,
        reason: &str,
        committed_at: &str,
        entity: &str,
        entity_normalized: &str,
        change_type: ChangeType,
        entry_type: EntryType,
        author: &str,
        risk: Option<&str>,
        is_breaking: bool,
        related_tickets: Option<&str>,
        origin: &str,
    ) -> Self {
        Self {
            sig_version: CURRENT_LEDGER_SIG_VERSION,
            tx_id: tx_id.to_string(),
            category: category.to_string(),
            summary: summary.to_string(),
            reason: reason.to_string(),
            committed_at: committed_at.to_string(),
            entity: entity.to_string(),
            change_type: change_type.to_string(),
            entry_type: entry_type.to_string(),
            author: author.to_string(),
            risk: risk.unwrap_or("").to_string(),
            is_breaking,
            related_tickets: related_tickets.unwrap_or("").to_string(),
            origin: origin.to_string(),
            entity_normalized: entity_normalized.to_string(),
        }
    }
}

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

/// Read the hex public key from `public.pem` under `keys_dir`, if present.
pub fn read_public_key_hex(keys_dir: &Path) -> Result<Option<String>> {
    let pub_path = keys_dir.join("public.pem");
    if !pub_path.exists() {
        return Ok(None);
    }
    let pub_bytes = fs::read(&pub_path).into_diagnostic()?;
    let pub_str = String::from_utf8(pub_bytes).into_diagnostic()?;
    let hex = pub_str.trim().to_ascii_lowercase();
    Ok(Some(hex))
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

/// NFC-normalize a string (used for entity + author in the signed payload).
pub fn nfc_normalize(s: &str) -> String {
    s.nfc().collect()
}

/// Sanitize free-text for the signed form: replace bare newlines with space,
/// refuse control chars other than TAB.
fn sanitize_free_text(s: &str) -> Result<String, SignatureVerifyError> {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '\n' | '\r' => out.push(' '),
            c if c == '\t' || !c.is_control() => out.push(c),
            _ => return Err(SignatureVerifyError::ForbiddenControlChar),
        }
    }
    Ok(out)
}

/// Encode the canonical v1 payload (byte-identical to historical production).
pub fn encode_v1_payload(
    tx_id: &str,
    category: &str,
    summary: &str,
    reason: &str,
    committed_at: &str,
) -> String {
    format!(
        "tx_id:{}\ncategory:{}\nsummary:{}\nreason:{}\ncommitted_at:{}",
        tx_id, category, summary, reason, committed_at
    )
}

/// Encode the frozen v2 canonical payload (exact field order, one line each).
pub fn encode_v2_payload(input: &LedgerSignInput) -> Result<String, SignatureVerifyError> {
    let entity = sanitize_free_text(&nfc_normalize(&input.entity))?;
    let author = sanitize_free_text(&nfc_normalize(&input.author))?;
    let summary = sanitize_free_text(&input.summary)?;
    let reason = sanitize_free_text(&input.reason)?;
    let risk = sanitize_free_text(&input.risk)?;
    let related_tickets = sanitize_free_text(&input.related_tickets)?;
    let origin = sanitize_free_text(&input.origin)?;

    Ok(format!(
        "sig_version:2\ntx_id:{}\ncategory:{}\nsummary:{}\nreason:{}\ncommitted_at:{}\nentity:{}\nchange_type:{}\nentry_type:{}\nauthor:{}\nrisk:{}\nis_breaking:{}\nrelated_tickets:{}\norigin:{}",
        input.tx_id,
        input.category,
        summary,
        reason,
        input.committed_at,
        entity,
        input.change_type,
        input.entry_type,
        author,
        risk,
        if input.is_breaking { "true" } else { "false" },
        related_tickets,
        origin,
    ))
}

/// SHA-256 hex of the canonical payload bytes (used for v2 entry_hash only).
pub fn content_digest_hex(payload_bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(payload_bytes);
    hex::encode(hasher.finalize())
}

/// Derive `entity_normalized` the same way transaction commit does, for verify.
///
/// Uses [`crate::util::path::normalize_relative_path`] against a fixed synthetic
/// repo root so verify does not need a live layout. Accepts both exact and
/// ASCII-lowercased forms (Windows case-insensitive repos lower the path at
/// commit time).
pub fn derive_entity_normalized(entity: &str) -> Option<String> {
    let nfc_entity = nfc_normalize(entity);
    // Synthetic absolute root so lexical cleanup works without a real tree.
    #[cfg(windows)]
    let root = Path::new(r"C:\__ledgerful_verify_root__");
    #[cfg(not(windows))]
    let root = Path::new("/__ledgerful_verify_root__");
    crate::util::path::normalize_relative_path(root, &nfc_entity).ok()
}

/// Consistency check for `entity_normalized` (not signed; derive-at-verify).
///
/// Recomputes with the production path normalizer (phase0). Case-only differences
/// are accepted when either side is the ASCII-lowercase of the derived form
/// (matches `TransactionManager::entity_normalized` on case-insensitive FS).
/// A mutated garbage value fails.
pub fn entity_normalized_consistent(entity: &str, entity_normalized: &str) -> bool {
    let Some(derived) = derive_entity_normalized(entity) else {
        // Entity cannot be path-normalized (escape/UNC). Fall back to strict
        // NFC slash equality only — never ignore_case alone (codex P1).
        let nfc_entity = nfc_normalize(entity);
        let entity_slash = nfc_entity.replace('\\', "/");
        let norm_slash = entity_normalized.replace('\\', "/");
        return entity_slash == norm_slash;
    };
    let norm_slash = entity_normalized.replace('\\', "/");
    derived == norm_slash || derived.eq_ignore_ascii_case(&norm_slash)
}

/// Sign a new production ledger entry as v2.
pub fn sign_ledger_entry_v2(input: &LedgerSignInput) -> Result<(Option<String>, Option<String>)> {
    let keys_dir = get_keys_dir()?;
    sign_ledger_entry_in_v2(&keys_dir, input)
}

/// Sign with an explicit keys directory using the v2 payload.
pub fn sign_ledger_entry_in_v2(
    keys_dir: &Path,
    input: &LedgerSignInput,
) -> Result<(Option<String>, Option<String>)> {
    let mut v2_input = input.clone();
    v2_input.sig_version = CURRENT_LEDGER_SIG_VERSION;
    let (signing_key, verifying_key) = get_or_create_keys_in(keys_dir)?;

    if !verify_keypair_consistency(&signing_key, &verifying_key) {
        return Err(miette::miette!(
            "Keypair consistency check failed: public key does not match private seed. Refusing to sign."
        ));
    }

    let payload = encode_v2_payload(&v2_input)
        .map_err(|e| miette::miette!("Failed to encode v2 signing payload: {e}"))?;
    let signature = signing_key.sign(payload.as_bytes());
    let sig_hex = hex::encode(signature.to_bytes());
    let pub_hex = hex::encode(verifying_key.to_bytes());
    Ok((Some(sig_hex), Some(pub_hex)))
}

/// Legacy five-field sign (v1). Kept for dual-verify test fixtures and
/// disaster-recovery only. **Production call sites must use v2.**
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

/// Legacy five-field sign (v1). See [`sign_ledger_entry`].
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

    let payload = encode_v1_payload(tx_id, category, summary, reason, committed_at);
    let signature = signing_key.sign(payload.as_bytes());
    let sig_hex = hex::encode(signature.to_bytes());
    let pub_hex = hex::encode(verifying_key.to_bytes());
    Ok((Some(sig_hex), Some(pub_hex)))
}

/// v1 entry_hash (unchanged): `SHA256(tx_id || signature_hex || prev_hash)`.
pub fn compute_entry_hash(tx_id: &str, signature_hex: &str, prev_hash: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(tx_id.as_bytes());
    hasher.update(signature_hex.as_bytes());
    hasher.update(prev_hash.as_bytes());
    hex::encode(hasher.finalize())
}

/// Version-aware entry hash. Dual-verify and chain walk select by stored
/// `sig_version`.
///
/// - v1: `SHA256(tx_id || signature_hex || prev_hash)`
/// - v2: `SHA256("v" || version || "\n" || content_digest_hex || "\n" || signature_hex || "\n" || prev_hash)`
pub fn compute_entry_hash_versioned(
    sig_version: u32,
    tx_id: &str,
    content_digest_hex: Option<&str>,
    signature_hex: &str,
    prev_hash: &str,
) -> String {
    if sig_version <= LEDGER_SIG_VERSION_V1 {
        return compute_entry_hash(tx_id, signature_hex, prev_hash);
    }
    let digest = content_digest_hex.unwrap_or("");
    let mut hasher = Sha256::new();
    hasher.update(b"v");
    hasher.update(sig_version.to_string().as_bytes());
    hasher.update(b"\n");
    hasher.update(digest.as_bytes());
    hasher.update(b"\n");
    hasher.update(signature_hex.as_bytes());
    hasher.update(b"\n");
    hasher.update(prev_hash.as_bytes());
    hex::encode(hasher.finalize())
}

/// Compute entry hash from a full entry (encodes payload for content_digest).
///
/// v2 encode failures (forbidden control chars, etc.) return `Err` — never an
/// empty digest that would silently collide under chain linkage.
pub fn compute_entry_hash_for_entry(entry: &LedgerEntry) -> Result<String, SignatureVerifyError> {
    let sig_hex = entry.signature.as_deref().unwrap_or("");
    let prev = entry.prev_hash.as_deref().unwrap_or("");
    if entry.sig_version <= LEDGER_SIG_VERSION_V1 {
        return Ok(compute_entry_hash(&entry.tx_id, sig_hex, prev));
    }
    let input = LedgerSignInput::from_entry(entry);
    let payload = encode_v2_payload(&input)?;
    let digest = content_digest_hex(payload.as_bytes());
    Ok(compute_entry_hash_versioned(
        entry.sig_version,
        &entry.tx_id,
        Some(&digest),
        sig_hex,
        prev,
    ))
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

/// Legacy five-field verify (v1 only). Prefer [`verify_entry_signature`].
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
    let payload = encode_v1_payload(tx_id, category, summary, reason, committed_at);
    verify_payload_bytes(payload.as_bytes(), signature_hex, public_key_hex)
}

fn verify_payload_bytes(
    payload: &[u8],
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

    verifying_key
        .verify(payload, &signature)
        .map_err(|_| SignatureVerifyError::SignatureMismatch)?;
    Ok(())
}

/// Dual-verify by **stored** `sig_version` only (never try both).
pub fn verify_entry_signature(
    input: &LedgerSignInput,
    signature_hex: &str,
    public_key_hex: &str,
) -> bool {
    verify_entry_signature_with_result(input, signature_hex, public_key_hex).is_ok()
}

/// Verify a [`LedgerEntry`]'s stored signature against its stored `sig_version`.
pub fn verify_ledger_entry_signature(entry: &LedgerEntry) -> bool {
    let (Some(sig), Some(pub_key)) = (&entry.signature, &entry.public_key) else {
        return false;
    };
    let input = LedgerSignInput::from_entry(entry);
    verify_entry_signature(&input, sig, pub_key)
}

pub fn verify_entry_signature_with_result(
    input: &LedgerSignInput,
    signature_hex: &str,
    public_key_hex: &str,
) -> Result<(), SignatureVerifyError> {
    // v2: entity_normalized consistency (derived, not signed).
    if input.sig_version >= LEDGER_SIG_VERSION_V2
        && !entity_normalized_consistent(&input.entity, &input.entity_normalized)
    {
        return Err(SignatureVerifyError::EntityNormalizedMismatch);
    }

    let payload = if input.sig_version <= LEDGER_SIG_VERSION_V1 {
        encode_v1_payload(
            &input.tx_id,
            &input.category,
            &input.summary,
            &input.reason,
            &input.committed_at,
        )
    } else {
        encode_v2_payload(input)?
    };
    verify_payload_bytes(payload.as_bytes(), signature_hex, public_key_hex)
}

/// Classify a signature with optional trusted-key pin list and min_sig_version.
pub fn classify_entry_signature(
    entry: &LedgerEntry,
    trusted_public_keys: &[String],
    min_sig_version: u32,
) -> SignatureTrustStatus {
    match (&entry.signature, &entry.public_key) {
        (Some(sig), Some(pub_key)) => {
            if entry.sig_version < min_sig_version {
                return SignatureTrustStatus::Invalid;
            }
            let input = LedgerSignInput::from_entry(entry);
            if !verify_entry_signature(&input, sig, pub_key) {
                return SignatureTrustStatus::Invalid;
            }
            let key_norm = pub_key.trim().to_ascii_lowercase();
            if trusted_public_keys.is_empty() {
                SignatureTrustStatus::ValidUnknownKey
            } else if trusted_public_keys
                .iter()
                .any(|k| k.trim().to_ascii_lowercase() == key_norm)
            {
                SignatureTrustStatus::ValidTrusted
            } else {
                SignatureTrustStatus::ValidUnknownKey
            }
        }
        _ => SignatureTrustStatus::Unsigned,
    }
}

/// Validate a trusted public key hex string (64 hex chars). Returns lowercase.
pub fn normalize_trusted_public_key(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("trusted public key is empty".to_string());
    }
    if trimmed.contains("BEGIN") || trimmed.contains('-') {
        return Err("trusted public key must be hex, not PEM".to_string());
    }
    let lower = trimmed.to_ascii_lowercase();
    if lower.len() != 64 {
        return Err(format!(
            "trusted public key must be 64 hex chars, got {}",
            lower.len()
        ));
    }
    if !lower.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err("trusted public key contains non-hex characters".to_string());
    }
    Ok(lower)
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum SignatureVerifyError {
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
    #[error("free-text field contains forbidden control characters")]
    ForbiddenControlChar,
    #[error("entity_normalized is inconsistent with entity")]
    EntityNormalizedMismatch,
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
    use crate::ledger::types::{Category, ChangeType, EntryType};

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

    fn sample_v2_input() -> LedgerSignInput {
        LedgerSignInput {
            sig_version: 2,
            tx_id: "tx-abc".to_string(),
            category: "FEATURE".to_string(),
            summary: "add feature".to_string(),
            reason: "needed".to_string(),
            committed_at: "2026-07-22T00:00:00Z".to_string(),
            entity: "src/main.rs".to_string(),
            change_type: "MODIFY".to_string(),
            entry_type: "IMPLEMENTATION".to_string(),
            author: "Ada Lovelace".to_string(),
            risk: "LOW".to_string(),
            is_breaking: false,
            related_tickets: "TICKET-1".to_string(),
            origin: "LOCAL".to_string(),
            entity_normalized: "src/main.rs".to_string(),
        }
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
    fn existing_private_key_is_not_clobbered_by_legacy_file() {
        let (_temp, keys_dir) = temp_keys_dir();

        let new_seed = [3u8; 32];
        let new_signing = SigningKey::from_bytes(&new_seed);
        let new_verifying = new_signing.verifying_key();

        let legacy_seed = [4u8; 32];

        fs::write(keys_dir.join("private.key"), hex::encode(new_seed)).unwrap();
        fs::write(
            keys_dir.join("public.pem"),
            hex::encode(new_verifying.to_bytes()),
        )
        .unwrap();
        fs::write(keys_dir.join("private.pem"), hex::encode(legacy_seed)).unwrap();

        let (loaded_signing, loaded_verifying) = get_or_create_keys_in(&keys_dir).unwrap();

        assert_eq!(
            loaded_signing.to_bytes(),
            new_seed,
            "private.key must not be clobbered"
        );
        assert_eq!(loaded_verifying.to_bytes(), new_verifying.to_bytes());

        if keys_dir.join("private.pem").exists() {
            assert!(keys_dir.join("private.key").exists());
        }
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

    #[test]
    fn v2_encode_is_deterministic_and_ordered() {
        let input = sample_v2_input();
        let a = encode_v2_payload(&input).unwrap();
        let b = encode_v2_payload(&input).unwrap();
        assert_eq!(a, b);
        let lines: Vec<&str> = a.lines().collect();
        assert_eq!(lines[0], "sig_version:2");
        assert_eq!(lines[1], "tx_id:tx-abc");
        assert_eq!(lines[6], "entity:src/main.rs");
        assert_eq!(lines[11], "is_breaking:false");
        assert_eq!(lines[13], "origin:LOCAL");
        assert_eq!(lines.len(), 14);
        assert!(!a.ends_with('\n'));
    }

    #[test]
    fn v2_sign_verify_canary() {
        let (_temp, keys_dir) = temp_keys_dir();
        let input = sample_v2_input();
        let (sig, pub_key) = sign_ledger_entry_in_v2(&keys_dir, &input).unwrap();
        let sig = sig.unwrap();
        let pub_key = pub_key.unwrap();
        assert!(verify_entry_signature(&input, &sig, &pub_key));
    }

    #[test]
    #[allow(clippy::type_complexity)]
    fn v2_mutation_of_each_integrity_field_fails_verify() {
        let (_temp, keys_dir) = temp_keys_dir();
        let base = sample_v2_input();
        let (sig, pub_key) = sign_ledger_entry_in_v2(&keys_dir, &base).unwrap();
        let sig = sig.unwrap();
        let pub_key = pub_key.unwrap();

        let mutators: Vec<(&str, Box<dyn Fn(&mut LedgerSignInput)>)> = vec![
            ("tx_id", Box::new(|i| i.tx_id = "mutated".into())),
            ("category", Box::new(|i| i.category = "BUGFIX".into())),
            ("summary", Box::new(|i| i.summary = "mutated".into())),
            ("reason", Box::new(|i| i.reason = "mutated".into())),
            (
                "committed_at",
                Box::new(|i| i.committed_at = "2099-01-01T00:00:00Z".into()),
            ),
            ("entity", Box::new(|i| i.entity = "src/other.rs".into())),
            ("change_type", Box::new(|i| i.change_type = "CREATE".into())),
            (
                "entry_type",
                Box::new(|i| i.entry_type = "ARCHITECTURE".into()),
            ),
            ("author", Box::new(|i| i.author = "Eve".into())),
            ("risk", Box::new(|i| i.risk = "CRITICAL".into())),
            ("is_breaking", Box::new(|i| i.is_breaking = true)),
            (
                "related_tickets",
                Box::new(|i| i.related_tickets = "OTHER".into()),
            ),
            ("origin", Box::new(|i| i.origin = "SIBLING".into())),
        ];

        for (name, mutate) in mutators {
            let mut mutated = base.clone();
            mutate(&mut mutated);
            // Keep entity_normalized consistent (production path normalizer)
            // so we isolate the signed-field failure, not the consistency check.
            mutated.entity_normalized =
                derive_entity_normalized(&mutated.entity).unwrap_or_else(|| mutated.entity.clone());
            assert!(
                !verify_entry_signature(&mutated, &sig, &pub_key),
                "mutating {name} must fail verify"
            );
        }
    }

    #[test]
    fn entity_normalized_mismatch_fails_v2() {
        let (_temp, keys_dir) = temp_keys_dir();
        let mut input = sample_v2_input();
        let (sig, pub_key) = sign_ledger_entry_in_v2(&keys_dir, &input).unwrap();
        let sig = sig.unwrap();
        let pub_key = pub_key.unwrap();
        input.entity_normalized = "TAMPERED/path.rs".to_string();
        assert!(!verify_entry_signature(&input, &sig, &pub_key));
    }

    #[test]
    fn entity_normalized_uses_production_path_normalizer() {
        // Phase0: derive-at-verify with the same normalizer as commit.
        assert!(entity_normalized_consistent(
            "./src/../src/main.rs",
            "src/main.rs"
        ));
        assert!(entity_normalized_consistent("src\\main.rs", "src/main.rs"));
        // Case-only differences accepted (case-insensitive FS lowercases at commit).
        assert!(entity_normalized_consistent("Src/Main.rs", "src/main.rs"));
        // Garbage normalized value must fail even if it shares a prefix.
        assert!(!entity_normalized_consistent(
            "src/main.rs",
            "TAMPERED/path.rs"
        ));
        // Case-only mismatch on a different path must fail.
        assert!(!entity_normalized_consistent("src/main.rs", "src/other.rs"));
    }

    #[test]
    fn v1_historical_still_verifies() {
        let (_temp, keys_dir) = temp_keys_dir();
        let (sig, pub_key) = sign_ledger_entry_in(
            &keys_dir,
            "tx-v1",
            "FEATURE",
            "summary",
            "reason",
            "2026-01-01T00:00:00Z",
        )
        .unwrap();
        let sig = sig.unwrap();
        let pub_key = pub_key.unwrap();
        assert!(verify_signature(
            "tx-v1",
            "FEATURE",
            "summary",
            "reason",
            "2026-01-01T00:00:00Z",
            &sig,
            &pub_key
        ));
        let input = LedgerSignInput {
            sig_version: 1,
            tx_id: "tx-v1".into(),
            category: "FEATURE".into(),
            summary: "summary".into(),
            reason: "reason".into(),
            committed_at: "2026-01-01T00:00:00Z".into(),
            entity: "x".into(),
            change_type: "MODIFY".into(),
            entry_type: "IMPLEMENTATION".into(),
            author: "a".into(),
            risk: String::new(),
            is_breaking: false,
            related_tickets: String::new(),
            origin: "LOCAL".into(),
            entity_normalized: "x".into(),
        };
        assert!(verify_entry_signature(&input, &sig, &pub_key));
        // Dual-verify must not accept v1 sig under v2 version.
        let mut v2 = input.clone();
        v2.sig_version = 2;
        assert!(!verify_entry_signature(&v2, &sig, &pub_key));
    }

    #[test]
    fn entry_hash_v1_and_v2_differ() {
        let v1 = compute_entry_hash("tx", "sig", "prev");
        let v2 = compute_entry_hash_versioned(2, "tx", Some("digest"), "sig", "prev");
        assert_ne!(v1, v2);
        assert_eq!(
            compute_entry_hash_versioned(1, "tx", None, "sig", "prev"),
            v1
        );
    }

    #[test]
    fn current_sig_version_is_v2() {
        assert_eq!(CURRENT_LEDGER_SIG_VERSION, 2);
    }

    #[test]
    fn enum_display_is_screaming_snake() {
        assert_eq!(Category::Feature.to_string(), "FEATURE");
        assert_eq!(ChangeType::Modify.to_string(), "MODIFY");
        assert_eq!(EntryType::Implementation.to_string(), "IMPLEMENTATION");
        assert_eq!(EntryType::Architecture.to_string(), "ARCHITECTURE");
        assert_eq!(ChangeType::Create.to_string(), "CREATE");
    }

    #[test]
    fn trusted_key_classification() {
        let (_temp, keys_dir) = temp_keys_dir();
        let input = sample_v2_input();
        let (sig, pub_key) = sign_ledger_entry_in_v2(&keys_dir, &input).unwrap();
        let entry = LedgerEntry {
            id: 1,
            tx_id: input.tx_id.clone(),
            category: Category::Feature,
            entry_type: EntryType::Implementation,
            entity: input.entity.clone(),
            entity_normalized: input.entity_normalized.clone(),
            change_type: ChangeType::Modify,
            summary: input.summary.clone(),
            reason: input.reason.clone(),
            is_breaking: false,
            committed_at: input.committed_at.clone(),
            verification_status: None,
            verification_basis: None,
            outcome_notes: None,
            origin: input.origin.clone(),
            trace_id: None,
            signature: sig,
            public_key: pub_key.clone(),
            risk: Some(input.risk.clone()),
            related_tickets: Some(input.related_tickets.clone()),
            author: input.author.clone(),
            observed: None,
            prev_hash: None,
            sig_version: 2,
        };
        let pk = pub_key.unwrap();
        assert_eq!(
            classify_entry_signature(&entry, &[], 1),
            SignatureTrustStatus::ValidUnknownKey
        );
        assert_eq!(
            classify_entry_signature(&entry, std::slice::from_ref(&pk), 1),
            SignatureTrustStatus::ValidTrusted
        );
        assert_eq!(
            classify_entry_signature(&entry, &["00".repeat(32)], 1),
            SignatureTrustStatus::ValidUnknownKey
        );
        assert_eq!(
            classify_entry_signature(&entry, &[], 2),
            SignatureTrustStatus::ValidUnknownKey
        );
        // min_sig_version=2 would still accept v2; force reject via version
        let mut v1_entry = entry.clone();
        v1_entry.sig_version = 1;
        assert_eq!(
            classify_entry_signature(&v1_entry, &[], 2),
            SignatureTrustStatus::Invalid
        );
    }

    #[test]
    fn normalize_trusted_public_key_rejects_bad() {
        assert!(normalize_trusted_public_key("").is_err());
        assert!(normalize_trusted_public_key("deadbeef").is_err());
        assert!(normalize_trusted_public_key("-----BEGIN PUBLIC KEY-----").is_err());
        let ok = "a".repeat(64);
        assert_eq!(normalize_trusted_public_key(&ok).unwrap(), ok);
        assert_eq!(
            normalize_trusted_public_key(&"A".repeat(64)).unwrap(),
            "a".repeat(64)
        );
    }

    #[test]
    fn free_text_newlines_sanitized() {
        let mut input = sample_v2_input();
        input.summary = "line1\nline2".into();
        let payload = encode_v2_payload(&input).unwrap();
        assert!(payload.contains("summary:line1 line2"));
        assert_eq!(payload.lines().count(), 14);
    }

    /// Residual columns (not in the v2 basis) may mutate without invalidating
    /// the entry signature — documents the honest residual surface.
    #[test]
    fn residual_columns_mutation_still_valid() {
        let (_temp, keys_dir) = temp_keys_dir();
        let input = sample_v2_input();
        let (sig, pub_key) = sign_ledger_entry_in_v2(&keys_dir, &input).unwrap();
        let sig = sig.unwrap();
        let pub_key = pub_key.unwrap();

        let mut entry = LedgerEntry {
            id: 1,
            tx_id: input.tx_id.clone(),
            category: Category::Feature,
            entry_type: EntryType::Implementation,
            entity: input.entity.clone(),
            entity_normalized: input.entity_normalized.clone(),
            change_type: ChangeType::Modify,
            summary: input.summary.clone(),
            reason: input.reason.clone(),
            is_breaking: false,
            committed_at: input.committed_at.clone(),
            verification_status: None,
            verification_basis: None,
            outcome_notes: None,
            origin: input.origin.clone(),
            trace_id: None,
            signature: Some(sig.clone()),
            public_key: Some(pub_key.clone()),
            risk: Some(input.risk.clone()),
            related_tickets: Some(input.related_tickets.clone()),
            author: input.author.clone(),
            observed: None,
            prev_hash: None,
            sig_version: 2,
        };
        assert!(verify_ledger_entry_signature(&entry));

        // Mutate residual columns only — signature must remain VALID.
        entry.verification_status = Some(crate::ledger::types::VerificationStatus::Verified);
        entry.verification_basis = Some(crate::ledger::types::VerificationBasis::ManualInspection);
        entry.outcome_notes = Some("mutated notes".into());
        entry.observed = Some(true);
        entry.trace_id = Some("trace-mutated".into());
        entry.id = 999;
        entry.prev_hash = Some("not-in-basis".into());
        assert!(
            verify_ledger_entry_signature(&entry),
            "mutating residual columns must leave the entry signature VALID"
        );
    }

    /// RT-C3: client-supplied signatures must verify against the v2 basis.
    #[test]
    fn supplied_v2_signature_good_accepted_bad_rejected() {
        let (_temp, keys_dir) = temp_keys_dir();
        let input = sample_v2_input();
        let (sig, pub_key) = sign_ledger_entry_in_v2(&keys_dir, &input).unwrap();
        let sig = sig.unwrap();
        let pub_key = pub_key.unwrap();

        assert!(
            verify_entry_signature(&input, &sig, &pub_key),
            "good supplied v2 signature must verify"
        );

        let bad_sig = "00".repeat(64);
        assert!(
            !verify_entry_signature(&input, &bad_sig, &pub_key),
            "bad supplied signature must be rejected"
        );

        // Mutated payload under good sig → reject.
        let mut mutated = input.clone();
        mutated.entity = "src/evil.rs".into();
        mutated.entity_normalized = "src/evil.rs".into();
        assert!(
            !verify_entry_signature(&mutated, &sig, &pub_key),
            "good sig over wrong provenance must be rejected"
        );
    }

    #[test]
    fn compute_entry_hash_for_entry_fails_loudly_on_encode_error() {
        let mut input = sample_v2_input();
        // BEL is a forbidden control char → encode_v2_payload fails.
        input.summary = "bad\u{0007}summary".into();
        let entry = LedgerEntry {
            id: 1,
            tx_id: input.tx_id.clone(),
            category: Category::Feature,
            entry_type: EntryType::Implementation,
            entity: input.entity.clone(),
            entity_normalized: input.entity_normalized.clone(),
            change_type: ChangeType::Modify,
            summary: input.summary.clone(),
            reason: input.reason.clone(),
            is_breaking: false,
            committed_at: input.committed_at.clone(),
            verification_status: None,
            verification_basis: None,
            outcome_notes: None,
            origin: input.origin.clone(),
            trace_id: None,
            signature: Some("ab".repeat(32)),
            public_key: Some("cd".repeat(32)),
            risk: None,
            related_tickets: None,
            author: input.author.clone(),
            observed: None,
            prev_hash: None,
            sig_version: 2,
        };
        let err = compute_entry_hash_for_entry(&entry).unwrap_err();
        assert_eq!(err, SignatureVerifyError::ForbiddenControlChar);
    }
}
