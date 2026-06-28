use crate::sync::crypto;
use crate::sync::hlc::HLC;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::{Read, Write};

#[derive(Debug, Serialize, Deserialize)]
pub struct Bundle {
    pub manifest: Manifest,
    #[serde(with = "serde_base64_64")]
    pub signature: [u8; 64],
    #[serde(with = "serde_base64_32")]
    pub device_pub: [u8; 32],
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Manifest {
    pub version: u32,
    pub device_id: String,
    pub bundle_hlc: HLC,
    pub manifest_sha256: String,
    pub entry_count: usize,
    pub entries: Vec<Entry>,
    #[serde(default)]
    pub tombstones: Vec<Tombstone>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Tombstone {
    pub tx_id: String,
    pub tombstone_hlc: HLC,
    pub reason: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Entry {
    pub tx_id: String,
    pub category: String,
    pub entry_type: String,
    pub entity: String,
    pub entity_normalized: String,
    pub change_type: String,
    pub summary: String,
    pub reason: String,
    pub is_breaking: bool,
    pub committed_at: chrono::DateTime<chrono::Utc>,
    pub origin: String,
    pub trace_id: Option<String>,
    pub signature: Option<String>,
    pub public_key: Option<String>,
    pub risk: Option<String>,
    pub verification_status: Option<String>,
    pub verification_basis: Option<String>,
    pub outcome_notes: Option<String>,
    pub related_tickets: Option<String>,
    pub entry_hlc: HLC,
}

impl Manifest {
    pub fn filename(&self) -> String {
        let short_sha = if self.manifest_sha256.len() >= 8 {
            &self.manifest_sha256[..8]
        } else {
            &self.manifest_sha256
        };

        format!("{}-{}.zip.gpg", self.bundle_hlc, short_sha)
    }
}

impl Bundle {
    pub fn build(
        mut manifest: Manifest,
        sign_key: &SigningKey,
    ) -> Result<(Vec<u8>, [u8; 64]), String> {
        // 1. Calculate manifest_sha256 of entries + tombstones
        let payload = serde_json::json!({
            "entries": manifest.entries,
            "tombstones": manifest.tombstones,
        });
        let payload_json = serde_json::to_vec(&payload)
            .map_err(|e| format!("Failed to serialize payload: {}", e))?;
        let mut hasher = Sha256::new();
        hasher.update(&payload_json);
        manifest.manifest_sha256 = hex::encode(hasher.finalize());
        manifest.entry_count = manifest.entries.len();

        // 2. Serialize manifest to JSON
        let manifest_json = serde_json::to_vec(&manifest)
            .map_err(|e| format!("Failed to serialize manifest: {}", e))?;

        // 3. Sign manifest
        let signature = sign_key.sign(&manifest_json).to_bytes();

        // 4. Create ZIP bundle
        let mut buf = Vec::new();
        {
            let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
            let options = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);

            zip.start_file("manifest.json", options)
                .map_err(|e| format!("Failed to start manifest.json: {}", e))?;
            zip.write_all(&manifest_json)
                .map_err(|e| format!("Failed to write manifest.json: {}", e))?;

            zip.start_file("device.sig", options)
                .map_err(|e| format!("Failed to start device.sig: {}", e))?;
            zip.write_all(&signature)
                .map_err(|e| format!("Failed to write device.sig: {}", e))?;

            zip.finish()
                .map_err(|e| format!("Failed to finish ZIP: {}", e))?;
        }

        Ok((buf, signature))
    }

    pub fn parse(
        zip_bytes: &[u8],
        verify_keys: &HashMap<String, [u8; 32]>,
    ) -> Result<Self, String> {
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(zip_bytes))
            .map_err(|e| format!("Failed to open ZIP: {}", e))?;

        // 1. Read manifest.json
        let mut manifest_json = Vec::new();
        {
            let mut manifest_file = archive
                .by_name("manifest.json")
                .map_err(|e| format!("Missing manifest.json: {}", e))?;
            std::io::copy(&mut manifest_file, &mut manifest_json)
                .map_err(|e| format!("Failed to read manifest.json: {}", e))?;
        }
        let manifest: Manifest = serde_json::from_slice(&manifest_json)
            .map_err(|e| format!("Failed to parse manifest.json: {}", e))?;

        // 2. Read device.sig
        let mut signature = [0u8; 64];
        {
            let mut sig_file = archive
                .by_name("device.sig")
                .map_err(|e| format!("Missing device.sig: {}", e))?;
            sig_file
                .read_exact(&mut signature)
                .map_err(|e| format!("Failed to read device.sig: {}", e))?;
        }

        // 3. Verify signature
        let pub_key_bytes = verify_keys
            .get(&manifest.device_id)
            .ok_or_else(|| format!("Unknown device: {}", manifest.device_id))?;
        let verifying_key = VerifyingKey::from_bytes(pub_key_bytes)
            .map_err(|e| format!("Invalid public key: {}", e))?;
        let sig = Signature::from_bytes(&signature);
        verifying_key
            .verify(&manifest_json, &sig)
            .map_err(|e| format!("Signature verification failed: {}", e))?;

        // 4. Verify manifest SHA-256
        let payload = serde_json::json!({
            "entries": manifest.entries,
            "tombstones": manifest.tombstones,
        });
        let payload_json = serde_json::to_vec(&payload)
            .map_err(|e| format!("Failed to serialize payload: {}", e))?;
        let mut hasher = Sha256::new();
        hasher.update(&payload_json);
        let calculated_sha = hex::encode(hasher.finalize());
        if manifest.manifest_sha256 != calculated_sha {
            return Err(format!(
                "Manifest SHA-256 mismatch: expected {}, got {}",
                manifest.manifest_sha256, calculated_sha
            ));
        }

        Ok(Bundle {
            manifest,
            signature,
            device_pub: *pub_key_bytes,
        })
    }

    pub fn encrypt(zip_bytes: &[u8], secret: &[u8]) -> Result<Vec<u8>, String> {
        let mut salt = [0u8; 16];
        rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut salt);

        let bundle_key = crypto::derive_bundle_key(secret, &salt);

        let mut nonce = [0u8; 12];
        rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut nonce);

        let (ciphertext, tag) = crypto::seal(zip_bytes, &bundle_key, &nonce)?;

        let mut result = Vec::with_capacity(16 + 12 + ciphertext.len() + 16);
        result.extend_from_slice(&salt);
        result.extend_from_slice(&nonce);
        result.extend_from_slice(&ciphertext);
        result.extend_from_slice(&tag);

        Ok(result)
    }

    pub fn decrypt(ciphertext: &[u8], secret: &[u8]) -> Result<Vec<u8>, String> {
        if ciphertext.len() < 16 + 12 + 16 {
            return Err("Ciphertext too short".to_string());
        }

        let salt = &ciphertext[0..16];
        let nonce = &ciphertext[16..28];
        let tag_pos = ciphertext.len() - 16;
        let ct = &ciphertext[28..tag_pos];
        let tag = &ciphertext[tag_pos..];

        let bundle_key = crypto::derive_bundle_key(secret, salt);

        let mut tag_bytes = [0u8; 16];
        tag_bytes.copy_from_slice(tag);

        let mut nonce_bytes = [0u8; 12];
        nonce_bytes.copy_from_slice(nonce);

        let decrypted = crypto::open(ct, &tag_bytes, &bundle_key, &nonce_bytes)?;

        Ok(decrypted.to_vec())
    }
}

mod serde_base64_64 {
    use super::*;
    use serde::{Deserializer, Serializer};

    pub fn serialize<S>(bytes: &[u8; 64], serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&BASE64.encode(bytes))
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<[u8; 64], D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        let bytes = BASE64.decode(s).map_err(serde::de::Error::custom)?;
        let mut arr = [0u8; 64];
        if bytes.len() != 64 {
            return Err(serde::de::Error::custom("Invalid signature length"));
        }
        arr.copy_from_slice(&bytes);
        Ok(arr)
    }
}

mod serde_base64_32 {
    use super::*;
    use serde::{Deserializer, Serializer};

    pub fn serialize<S>(bytes: &[u8; 32], serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&BASE64.encode(bytes))
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<[u8; 32], D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        let bytes = BASE64.decode(s).map_err(serde::de::Error::custom)?;
        let mut arr = [0u8; 32];
        if bytes.len() != 32 {
            return Err(serde::de::Error::custom("Invalid public key length"));
        }
        arr.copy_from_slice(&bytes);
        Ok(arr)
    }
}
