use crate::common::crypto_home_guard;
use ed25519_dalek::SigningKey;
use ledgerful::ledger::crypto::get_or_create_keys;
use std::fs;

fn keys_dir(tmp: &std::path::Path) -> std::path::PathBuf {
    tmp.join(".ledgerful").join("keys")
}

fn seed_hex(seed: [u8; 32]) -> String {
    hex::encode(seed)
}

#[test]
fn migrates_existing_private_pem_to_private_key() {
    let tmp = tempfile::tempdir().unwrap();
    let (_home, _userprofile) = crypto_home_guard(tmp.path());
    let dir = keys_dir(tmp.path());
    fs::create_dir_all(&dir).unwrap();

    let seed = [7u8; 32];
    let expected_public = SigningKey::from_bytes(&seed).verifying_key().to_bytes();
    fs::write(dir.join("private.pem"), seed_hex(seed)).unwrap();
    fs::write(dir.join("public.pem"), hex::encode(expected_public)).unwrap();

    let (_, verifying_key) = get_or_create_keys().expect("get_or_create_keys failed");

    assert!(
        !dir.join("private.pem").exists(),
        "legacy private.pem should be removed"
    );
    assert!(
        dir.join("private.key").exists(),
        "private.key should exist after migration"
    );
    assert_eq!(
        verifying_key.to_bytes(),
        expected_public,
        "loaded key must match pre-existing seed"
    );
}

#[test]
fn does_not_clobber_existing_private_key() {
    let tmp = tempfile::tempdir().unwrap();
    let (_home, _userprofile) = crypto_home_guard(tmp.path());
    let dir = keys_dir(tmp.path());
    fs::create_dir_all(&dir).unwrap();

    let seed_a = [1u8; 32];
    let seed_b = [2u8; 32];
    let expected_public_b = SigningKey::from_bytes(&seed_b).verifying_key().to_bytes();

    fs::write(dir.join("private.pem"), seed_hex(seed_a)).unwrap();
    fs::write(dir.join("private.key"), seed_hex(seed_b)).unwrap();
    fs::write(dir.join("public.pem"), hex::encode(expected_public_b)).unwrap();

    let (signing_key, verifying_key) = get_or_create_keys().expect("get_or_create_keys failed");

    assert!(
        dir.join("private.pem").exists(),
        "legacy private.pem must be left alone when .key exists"
    );
    assert!(dir.join("private.key").exists(), "private.key must remain");
    assert_eq!(
        verifying_key.to_bytes(),
        expected_public_b,
        "the .key file must win"
    );
    assert_eq!(
        signing_key.to_bytes(),
        seed_b,
        "the returned signing key must match private.key"
    );
    assert_eq!(
        fs::read_to_string(dir.join("private.key")).unwrap().trim(),
        seed_hex(seed_b)
    );
}

#[test]
fn fresh_install_writes_private_key_directly() {
    let tmp = tempfile::tempdir().unwrap();
    let (_home, _userprofile) = crypto_home_guard(tmp.path());

    let (_, _) = get_or_create_keys().expect("get_or_create_keys failed");

    let dir = keys_dir(tmp.path());
    assert!(
        !dir.join("private.pem").exists(),
        "legacy private.pem should not appear on fresh install"
    );
    assert!(
        dir.join("private.key").exists(),
        "private.key should be written directly"
    );
}

#[test]
fn missing_public_pem_is_derived_from_private_seed_not_replaced() {
    let tmp = tempfile::tempdir().unwrap();
    let (_home, _userprofile) = crypto_home_guard(tmp.path());
    let dir = keys_dir(tmp.path());
    fs::create_dir_all(&dir).unwrap();

    let seed = [9u8; 32];
    let expected_public = SigningKey::from_bytes(&seed).verifying_key().to_bytes();

    fs::write(dir.join("private.pem"), seed_hex(seed)).unwrap();

    let (signing_key, verifying_key) = get_or_create_keys().expect("get_or_create_keys failed");

    assert_eq!(
        signing_key.to_bytes(),
        seed,
        "must preserve the existing private identity"
    );
    assert_eq!(
        verifying_key.to_bytes(),
        expected_public,
        "public key must be derived from the existing private seed"
    );
    assert!(
        dir.join("public.pem").exists(),
        "public.pem should be regenerated"
    );
    assert_eq!(
        fs::read_to_string(dir.join("public.pem")).unwrap().trim(),
        hex::encode(expected_public)
    );
    assert!(
        !dir.join("private.pem").exists(),
        "private.pem should have been renamed to private.key"
    );
}

#[test]
fn missing_public_pem_with_existing_private_key_derives_not_replaces() {
    let tmp = tempfile::tempdir().unwrap();
    let (_home, _userprofile) = crypto_home_guard(tmp.path());
    let dir = keys_dir(tmp.path());
    fs::create_dir_all(&dir).unwrap();

    let seed = [11u8; 32];
    let expected_public = SigningKey::from_bytes(&seed).verifying_key().to_bytes();

    fs::write(dir.join("private.key"), seed_hex(seed)).unwrap();

    let (signing_key, verifying_key) = get_or_create_keys().expect("get_or_create_keys failed");

    assert_eq!(
        signing_key.to_bytes(),
        seed,
        "must preserve the existing private identity"
    );
    assert_eq!(
        verifying_key.to_bytes(),
        expected_public,
        "public key must be derived from the existing private seed"
    );
    assert!(
        dir.join("public.pem").exists(),
        "public.pem should be regenerated"
    );
}

#[test]
fn mismatched_public_pem_is_regenerated_from_private_seed() {
    let tmp = tempfile::tempdir().unwrap();
    let (_home, _userprofile) = crypto_home_guard(tmp.path());
    let dir = keys_dir(tmp.path());
    fs::create_dir_all(&dir).unwrap();

    let seed = [13u8; 32];
    let expected_public = SigningKey::from_bytes(&seed).verifying_key().to_bytes();
    let wrong_seed = [14u8; 32];
    let wrong_public = SigningKey::from_bytes(&wrong_seed)
        .verifying_key()
        .to_bytes();

    fs::write(dir.join("private.key"), seed_hex(seed)).unwrap();
    fs::write(dir.join("public.pem"), hex::encode(wrong_public)).unwrap();

    let (signing_key, verifying_key) = get_or_create_keys().expect("get_or_create_keys failed");

    assert_eq!(
        signing_key.to_bytes(),
        seed,
        "must preserve the existing private identity"
    );
    assert_eq!(
        verifying_key.to_bytes(),
        expected_public,
        "public key must be derived from the private seed, not the stale file"
    );
    assert_eq!(
        fs::read_to_string(dir.join("public.pem")).unwrap().trim(),
        hex::encode(expected_public),
        "public.pem should have been rewritten"
    );
}
