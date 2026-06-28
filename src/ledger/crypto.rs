use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use miette::{IntoDiagnostic, Result};
use rand::rngs::OsRng;
use std::fs;
use std::path::PathBuf;

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
    let priv_path = keys_dir.join("private.pem");
    let pub_path = keys_dir.join("public.pem");

    if priv_path.exists() && pub_path.exists() {
        let priv_bytes = fs::read(&priv_path).into_diagnostic()?;
        let pub_bytes = fs::read(&pub_path).into_diagnostic()?;

        let priv_str = String::from_utf8(priv_bytes).into_diagnostic()?;
        let pub_str = String::from_utf8(pub_bytes).into_diagnostic()?;

        let priv_decoded = hex::decode(priv_str.trim()).into_diagnostic()?;
        let pub_decoded = hex::decode(pub_str.trim()).into_diagnostic()?;

        let priv_array: [u8; 32] = priv_decoded
            .try_into()
            .map_err(|_| miette::miette!("Invalid private key size"))?;
        let pub_array: [u8; 32] = pub_decoded
            .try_into()
            .map_err(|_| miette::miette!("Invalid public key size"))?;

        let signing_key = SigningKey::from_bytes(&priv_array);
        let verifying_key = VerifyingKey::from_bytes(&pub_array).into_diagnostic()?;

        Ok((signing_key, verifying_key))
    } else {
        let mut csprng = OsRng;
        let mut bytes = [0u8; 32];
        use rand::RngCore;
        csprng.fill_bytes(&mut bytes);
        let signing_key = SigningKey::from_bytes(&bytes);
        let verifying_key = signing_key.verifying_key();

        let priv_hex = hex::encode(signing_key.to_bytes());
        let pub_hex = hex::encode(verifying_key.to_bytes());

        fs::write(&priv_path, priv_hex).into_diagnostic()?;
        fs::write(&pub_path, pub_hex).into_diagnostic()?;

        Ok((signing_key, verifying_key))
    }
}

pub fn sign_ledger_entry(
    tx_id: &str,
    category: &str,
    summary: &str,
    reason: &str,
    committed_at: &str,
) -> Result<(Option<String>, Option<String>)> {
    let (signing_key, verifying_key) = get_or_create_keys()?;

    let payload = format!(
        "tx_id:{}\ncategory:{}\nsummary:{}\nreason:{}\ncommitted_at:{}",
        tx_id, category, summary, reason, committed_at
    );

    let signature = signing_key.sign(payload.as_bytes());

    let sig_hex = hex::encode(signature.to_bytes());
    let pub_hex = hex::encode(verifying_key.to_bytes());

    Ok((Some(sig_hex), Some(pub_hex)))
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
    let pub_bytes = match hex::decode(public_key_hex) {
        Ok(b) => b,
        Err(_) => return false,
    };
    let pub_array: [u8; 32] = match pub_bytes.try_into() {
        Ok(arr) => arr,
        Err(_) => return false,
    };
    let verifying_key = match VerifyingKey::from_bytes(&pub_array) {
        Ok(k) => k,
        Err(_) => return false,
    };

    let sig_bytes = match hex::decode(signature_hex) {
        Ok(b) => b,
        Err(_) => return false,
    };
    let sig_array: [u8; 64] = match sig_bytes.try_into() {
        Ok(arr) => arr,
        Err(_) => return false,
    };
    let signature = Signature::from_bytes(&sig_array);

    let payload = format!(
        "tx_id:{}\ncategory:{}\nsummary:{}\nreason:{}\ncommitted_at:{}",
        tx_id, category, summary, reason, committed_at
    );

    verifying_key.verify(payload.as_bytes(), &signature).is_ok()
}
