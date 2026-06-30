use ledgerful::sync::crypto;

#[test]
fn test_team_secret_to_bundle_key_is_deterministic() {
    let secret = b"twelve words team secret here please";
    let salt = b"bundle-salt-1234";

    let key1 = crypto::derive_bundle_key(secret, salt).unwrap();
    let key2 = crypto::derive_bundle_key(secret, salt).unwrap();

    assert_eq!(*key1, *key2);
    assert_ne!(*key1, [0u8; 32]);
}

#[test]
fn test_bundle_key_varies_with_salt() {
    let secret = b"twelve words team secret here please";
    let salt1 = b"salt-long-1";
    let salt2 = b"salt-long-2";

    let key1 = crypto::derive_bundle_key(secret, salt1).unwrap();
    let key2 = crypto::derive_bundle_key(secret, salt2).unwrap();

    assert_ne!(*key1, *key2);
}

#[test]
fn test_aead_round_trip() {
    let key = [0u8; 32]; // Not secure but fine for round trip test
    let nonce = [0u8; 24];
    let plaintext = b"Hello, Ledgerful Sync!";
    let aad = b"bundle-aad-1";

    let (ciphertext, tag) = crypto::seal(plaintext, &key, &nonce, aad).expect("Encryption failed");
    let decrypted = crypto::open(&ciphertext, &tag, &key, &nonce, aad).expect("Decryption failed");

    assert_eq!(plaintext, decrypted.as_slice());
}

#[test]
fn test_aead_open_rejects_tampered_ciphertext() {
    let key = [0u8; 32];
    let nonce = [0u8; 24];
    let plaintext = b"Sensitive data";
    let aad = b"bundle-aad-2";

    let (mut ciphertext, tag) =
        crypto::seal(plaintext, &key, &nonce, aad).expect("Encryption failed");

    // Tamper with ciphertext
    ciphertext[0] ^= 0xFF;

    let result = crypto::open(&ciphertext, &tag, &key, &nonce, aad);
    assert!(
        result.is_err(),
        "Should have failed to decrypt tampered data"
    );
}

#[test]
fn test_aead_open_rejects_wrong_aad() {
    let key = [0u8; 32];
    let nonce = [0u8; 24];
    let plaintext = b"Context-bound data";

    let (ciphertext, tag) =
        crypto::seal(plaintext, &key, &nonce, b"correct-aad").expect("Encryption failed");
    let result = crypto::open(&ciphertext, &tag, &key, &nonce, b"wrong-aad");

    assert!(
        result.is_err(),
        "Should have failed to decrypt with mismatched AAD"
    );
}

#[test]
fn test_ed25519_sign_and_verify_round_trip() {
    use ed25519_dalek::SigningKey;
    use rand::rngs::OsRng;

    let mut csprng = OsRng;
    let signing_key = SigningKey::generate(&mut csprng);
    let public_key = signing_key.verifying_key();

    let message = b"Transaction: ABC-123";
    let signature = crypto::sign(&signing_key, message);

    let is_valid = crypto::verify(&public_key, message, &signature);
    assert!(is_valid, "Signature should be valid");
}

#[test]
fn test_ed25519_verify_rejects_tampered_signature() {
    use ed25519_dalek::SigningKey;
    use rand::rngs::OsRng;

    let mut csprng = OsRng;
    let signing_key = SigningKey::generate(&mut csprng);
    let public_key = signing_key.verifying_key();

    let message = b"Transaction: ABC-123";
    let mut signature = crypto::sign(&signing_key, message);

    // Tamper with signature
    signature[0] ^= 0xFF;

    let is_valid = crypto::verify(&public_key, message, &signature);
    assert!(!is_valid, "Tampered signature should be invalid");
}
