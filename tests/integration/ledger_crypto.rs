use ed25519_dalek::SigningKey;
use ledgerful::ledger::crypto::{sign_ledger_entry_in, verify_signature};
use std::path::Path;

fn keys_dir(tmp: &Path) -> std::path::PathBuf {
    tmp.join(".ledgerful").join("keys")
}

#[test]
fn test_sign_and_verify_roundtrip() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = keys_dir(tmp.path());
    std::fs::create_dir_all(&dir).unwrap();
    let tx_id = "tx_123";
    let category = "FEATURE";
    let summary = "Add crypto tests";
    let reason = "Verify security";
    let committed_at = "2024-05-20T12:00:00Z";

    let (sig, pub_key) = sign_ledger_entry_in(&dir, tx_id, category, summary, reason, committed_at)
        .expect("Signing failed");

    let sig_str = sig.expect("No signature");
    let pub_str = pub_key.expect("No public key");

    assert!(verify_signature(
        tx_id,
        category,
        summary,
        reason,
        committed_at,
        &sig_str,
        &pub_str
    ));
}

#[test]
fn test_verify_fails_on_tampered_payload() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = keys_dir(tmp.path());
    std::fs::create_dir_all(&dir).unwrap();
    let tx_id = "tx_123";
    let category = "FEATURE";
    let summary = "Add crypto tests";
    let reason = "Verify security";
    let committed_at = "2024-05-20T12:00:00Z";

    let (sig, pub_key) = sign_ledger_entry_in(&dir, tx_id, category, summary, reason, committed_at)
        .expect("Signing failed");

    let sig_str = sig.expect("No signature");
    let pub_str = pub_key.expect("No public key");

    assert!(!verify_signature(
        tx_id,
        category,
        "Tampered",
        reason,
        committed_at,
        &sig_str,
        &pub_str
    ));
}

#[test]
fn test_sign_generates_keypair_on_fresh_keys_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = keys_dir(tmp.path());
    let result = sign_ledger_entry_in(
        &dir,
        "tx_id",
        "FEATURE",
        "summary",
        "reason",
        "2024-05-20T12:00:00Z",
    );
    assert!(
        result.is_ok(),
        "fresh temp keys dir should generate a keypair"
    );
}

#[test]
fn mismatched_public_heals_to_canonical_private_seed_and_signs_under_it() {
    use ledgerful::ledger::crypto::get_or_create_keys_in;
    use std::fs;

    let tmp = tempfile::tempdir().unwrap();
    let keys_dir = tmp.path().join(".ledgerful").join("keys");
    fs::create_dir_all(&keys_dir).unwrap();

    let canonical_seed = [42u8; 32];
    let canonical_signing = SigningKey::from_bytes(&canonical_seed);
    let canonical_pub = canonical_signing.verifying_key().to_bytes();

    let wrong_seed = [99u8; 32];
    let wrong_pub = SigningKey::from_bytes(&wrong_seed)
        .verifying_key()
        .to_bytes();

    fs::write(keys_dir.join("private.key"), hex::encode(canonical_seed)).unwrap();
    fs::write(keys_dir.join("public.pem"), hex::encode(wrong_pub)).unwrap();

    let (signing_key, verifying_key) =
        get_or_create_keys_in(&keys_dir).expect("healing should succeed");

    assert_eq!(
        signing_key.to_bytes(),
        canonical_seed,
        "private key must be the canonical seed, not swapped"
    );
    assert_eq!(
        verifying_key.to_bytes(),
        canonical_pub,
        "public key must be healed to match the canonical private seed"
    );

    let (sig, pub_hex) = sign_ledger_entry_in(
        &keys_dir,
        "tx1",
        "FEATURE",
        "test",
        "test",
        "2024-01-01T00:00:00Z",
    )
    .expect("signing after heal must succeed");

    assert_eq!(
        pub_hex.as_deref().unwrap(),
        hex::encode(canonical_pub),
        "signature must be under the canonical public key"
    );
    assert!(
        verify_signature(
            "tx1",
            "FEATURE",
            "test",
            "test",
            "2024-01-01T00:00:00Z",
            sig.as_deref().unwrap(),
            &hex::encode(canonical_pub)
        ),
        "signature must verify under the canonical key"
    );
}

#[test]
fn sign_ledger_entry_in_heals_mismatched_public_key_and_signs() {
    use ed25519_dalek::SigningKey;
    use ledgerful::ledger::crypto::get_or_create_keys_in;
    use std::fs;

    let tmp = tempfile::tempdir().unwrap();
    let keys_dir = tmp.path().join(".ledgerful").join("keys");
    fs::create_dir_all(&keys_dir).unwrap();

    let seed = [42u8; 32];
    let wrong_seed = [99u8; 32];
    let wrong_public = SigningKey::from_bytes(&wrong_seed)
        .verifying_key()
        .to_bytes();

    fs::write(keys_dir.join("private.key"), hex::encode(seed)).unwrap();
    fs::write(keys_dir.join("public.pem"), hex::encode(wrong_public)).unwrap();

    // Prime the temp dir through get_or_create_keys_in so the next call uses
    // the private key that we planted. The first call already heals the public
    // key; the second call signs with a guaranteed-consistent pair.
    let _ = get_or_create_keys_in(&keys_dir).expect("key load should heal the public key");

    let result = sign_ledger_entry_in(
        &keys_dir,
        "tx1",
        "FEATURE",
        "test",
        "test",
        "2024-01-01T00:00:00Z",
    );
    assert!(
        result.is_ok(),
        "should heal mismatched pub and sign successfully"
    );
    let (sig, pub_key) = result.unwrap();
    assert!(sig.is_some());

    let expected_public = SigningKey::from_bytes(&seed).verifying_key().to_bytes();
    assert_eq!(
        pub_key.unwrap(),
        hex::encode(expected_public),
        "public key should be healed to match private seed"
    );

    assert!(
        verify_signature(
            "tx1",
            "FEATURE",
            "test",
            "test",
            "2024-01-01T00:00:00Z",
            &sig.unwrap(),
            &hex::encode(expected_public)
        ),
        "healed signature should verify"
    );
}

#[test]
fn sign_ledger_entry_in_has_defense_in_depth_keypair_check() {
    // The verify_keypair_consistency check in sign_ledger_entry_in is
    // defense-in-depth: get_or_create_keys_in heals any mismatch before
    // returning, so the check never fires in the normal path. But if a
    // future change bypasses healing, the check would catch it.
    // This test proves the check function itself works.
    let signing_key = SigningKey::from_bytes(&[31u8; 32]);
    let wrong_verifying = SigningKey::from_bytes(&[32u8; 32]).verifying_key();
    assert!(
        !ledgerful::ledger::crypto::verify_keypair_consistency(&signing_key, &wrong_verifying),
        "defense-in-depth check must detect mismatched pairs"
    );

    let correct_verifying = signing_key.verifying_key();
    assert!(
        ledgerful::ledger::crypto::verify_keypair_consistency(&signing_key, &correct_verifying),
        "defense-in-depth check must accept matching pairs"
    );
}

#[test]
fn verify_signature_handles_invalid_sigs_without_panicking() {
    // verify_signature is a pure function — it doesn't load keys from disk.
    // It takes the signature and public key as hex strings from the ledger
    // entry. This proves read-only verification stays operational even when
    // the system has bad/mismatched key state, because verify doesn't
    // depend on the key files at all.

    // Invalid hex signature
    let is_valid = verify_signature(
        "tx1",
        "FEATURE",
        "test",
        "test",
        "2024-01-01T00:00:00Z",
        "deadbeef",
        &hex::encode([1u8; 32]),
    );
    assert!(
        !is_valid,
        "invalid signature hex must return false, not panic"
    );

    // Short signature
    let is_valid2 = verify_signature(
        "tx1",
        "FEATURE",
        "test",
        "test",
        "2024-01-01T00:00:00Z",
        "00",
        &hex::encode([1u8; 32]),
    );
    assert!(!is_valid2, "short signature must return false, not panic");

    // Invalid public key hex
    let is_valid3 = verify_signature(
        "tx1",
        "FEATURE",
        "test",
        "test",
        "2024-01-01T00:00:00Z",
        &hex::encode([0u8; 64]),
        "notvalidhex",
    );
    assert!(
        !is_valid3,
        "invalid public key hex must return false, not panic"
    );

    // Valid hex but wrong key — no panic, just false
    let wrong_sig = [0xaa; 64];
    let wrong_pub = [0xbb; 32];
    let is_valid4 = verify_signature(
        "tx1",
        "FEATURE",
        "test",
        "test",
        "2024-01-01T00:00:00Z",
        &hex::encode(wrong_sig),
        &hex::encode(wrong_pub),
    );
    assert!(
        !is_valid4,
        "wrong key/sig pair must return false, not panic"
    );
}
