use argon2::{Argon2, Params};
use chacha20poly1305::{
    ChaCha20Poly1305, Nonce,
    aead::{Aead, KeyInit},
};
use ed25519_dalek::{Signature, SigningKey, VerifyingKey};
use subtle::ConstantTimeEq;
use zeroize::Zeroizing;

/// Derives a 32-byte bundle key from a secret and salt using Argon2id.
/// The result is wrapped in Zeroizing to ensure it's wiped on drop.
pub fn derive_bundle_key(secret: &[u8], salt: &[u8]) -> Zeroizing<[u8; 32]> {
    let mut key = [0u8; 32];
    let params = Params::new(64 * 1024, 3, 1, Some(32)).expect("Invalid Argon2 params");
    let argon2 = Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params);

    argon2
        .hash_password_into(secret, salt, &mut key)
        .expect("Argon2 hashing failed");

    Zeroizing::new(key)
}

/// Seals plaintext using ChaCha20-Poly1305 with the given key and nonce.
pub fn seal(
    plaintext: &[u8],
    key: &[u8; 32],
    nonce: &[u8; 12],
) -> Result<(Vec<u8>, [u8; 16]), String> {
    let cipher = ChaCha20Poly1305::new(key.into());
    let nonce = Nonce::from_slice(nonce);

    let ciphertext_with_tag = cipher
        .encrypt(nonce, plaintext)
        .map_err(|e| format!("Encryption failed: {}", e))?;

    if ciphertext_with_tag.len() < 16 {
        return Err("Ciphertext too short".to_string());
    }

    let tag_pos = ciphertext_with_tag.len() - 16;
    let (ct, tag) = ciphertext_with_tag.split_at(tag_pos);

    let mut tag_bytes = [0u8; 16];
    tag_bytes.copy_from_slice(tag);

    Ok((ct.to_vec(), tag_bytes))
}

/// Opens ciphertext using ChaCha20-Poly1305 with the given key, nonce, and tag.
pub fn open(
    ciphertext: &[u8],
    tag: &[u8; 16],
    key: &[u8; 32],
    nonce: &[u8; 12],
) -> Result<Zeroizing<Vec<u8>>, String> {
    let cipher = ChaCha20Poly1305::new(key.into());
    let nonce = Nonce::from_slice(nonce);

    let mut full_ciphertext = ciphertext.to_vec();
    full_ciphertext.extend_from_slice(tag);

    let decrypted = cipher
        .decrypt(nonce, full_ciphertext.as_ref())
        .map_err(|e| format!("Decryption failed: {}", e))?;

    Ok(Zeroizing::new(decrypted))
}

/// Signs a message using Ed25519.
pub fn sign(signing_key: &SigningKey, message: &[u8]) -> [u8; 64] {
    use ed25519_dalek::Signer;
    signing_key.sign(message).to_bytes()
}

/// Verifies an Ed25519 signature.
pub fn verify(verifying_key: &VerifyingKey, message: &[u8], signature_bytes: &[u8; 64]) -> bool {
    use ed25519_dalek::Verifier;
    let signature = Signature::from_bytes(signature_bytes);
    verifying_key.verify(message, &signature).is_ok()
}

/// Constant-time equality check for byte slices.
pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    a.ct_eq(b).into()
}
