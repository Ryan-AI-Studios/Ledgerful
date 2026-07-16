use ed25519_dalek::SigningKey;
use ledgerful::ledger::crypto::get_or_create_keys_in;
use std::fs;
use std::path::Path;

fn keys_dir(tmp: &Path) -> std::path::PathBuf {
    tmp.join(".ledgerful").join("keys")
}

fn seed_hex(seed: [u8; 32]) -> String {
    hex::encode(seed)
}

fn sentinel_hash(keys_dir: &Path) -> String {
    let mut entries: Vec<(std::path::PathBuf, Vec<u8>)> = Vec::new();
    if keys_dir.exists() {
        for entry in fs::read_dir(keys_dir).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if path.is_file() {
                entries.push((path.file_name().unwrap().into(), fs::read(&path).unwrap()));
            }
        }
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    let mut hasher = blake3::Hasher::new();
    for (name, contents) in entries {
        hasher.update(name.to_string_lossy().as_bytes());
        hasher.update(&contents);
    }
    hasher.finalize().to_hex().to_string()
}

fn get_keys_dir_for_real_home() -> std::path::PathBuf {
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .map(std::path::PathBuf::from)
        .unwrap_or_default();
    home.join(".ledgerful").join("keys")
}

#[test]
fn does_not_clobber_existing_private_key() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = keys_dir(tmp.path());
    fs::create_dir_all(&dir).unwrap();

    let seed_a = [1u8; 32];
    let seed_b = [2u8; 32];
    let expected_public_b = SigningKey::from_bytes(&seed_b).verifying_key().to_bytes();

    fs::write(dir.join("private.pem"), seed_hex(seed_a)).unwrap();
    fs::write(dir.join("private.key"), seed_hex(seed_b)).unwrap();
    fs::write(dir.join("public.pem"), hex::encode(expected_public_b)).unwrap();

    let (signing_key, verifying_key) =
        get_or_create_keys_in(&dir).expect("get_or_create_keys_in failed");

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
fn missing_public_pem_is_derived_from_private_seed_not_replaced() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = keys_dir(tmp.path());
    fs::create_dir_all(&dir).unwrap();

    let seed = [9u8; 32];
    let expected_public = SigningKey::from_bytes(&seed).verifying_key().to_bytes();

    fs::write(dir.join("private.pem"), seed_hex(seed)).unwrap();

    let (signing_key, verifying_key) =
        get_or_create_keys_in(&dir).expect("get_or_create_keys_in failed");

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
    let dir = keys_dir(tmp.path());
    fs::create_dir_all(&dir).unwrap();

    let seed = [11u8; 32];
    let expected_public = SigningKey::from_bytes(&seed).verifying_key().to_bytes();

    fs::write(dir.join("private.key"), seed_hex(seed)).unwrap();

    let (signing_key, verifying_key) =
        get_or_create_keys_in(&dir).expect("get_or_create_keys_in failed");

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
    let dir = keys_dir(tmp.path());
    fs::create_dir_all(&dir).unwrap();

    let seed = [13u8; 32];
    let expected_public = SigningKey::from_bytes(&seed).verifying_key().to_bytes();
    let wrong_public = SigningKey::from_bytes(&[14u8; 32])
        .verifying_key()
        .to_bytes();

    fs::write(dir.join("private.key"), seed_hex(seed)).unwrap();
    fs::write(dir.join("public.pem"), hex::encode(wrong_public)).unwrap();

    let (signing_key, verifying_key) =
        get_or_create_keys_in(&dir).expect("get_or_create_keys_in failed");

    assert_eq!(
        signing_key.to_bytes(),
        seed,
        "must preserve the existing private identity"
    );
    assert_eq!(
        verifying_key.to_bytes(),
        expected_public,
        "public key must be regenerated to match the private seed"
    );
    assert_eq!(
        fs::read_to_string(dir.join("public.pem")).unwrap().trim(),
        hex::encode(expected_public),
        "public.pem should contain the regenerated key"
    );
}

#[test]
fn sentinel_real_keys_dir_is_not_written_by_tests() {
    let real_keys_dir = get_keys_dir_for_real_home();
    let before_hash = sentinel_hash(&real_keys_dir);

    // Run a fresh key creation in a temp dir (the typical test path).
    let tmp = tempfile::tempdir().unwrap();
    let dir = keys_dir(tmp.path());
    fs::create_dir_all(&dir).unwrap();
    let _ = get_or_create_keys_in(&dir);

    let after_hash = sentinel_hash(&real_keys_dir);
    assert_eq!(
        before_hash, after_hash,
        "tests must not mutate the real ~/.ledgerful/keys directory"
    );
}
