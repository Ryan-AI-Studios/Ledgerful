use ed25519_dalek::SigningKey;
use ledgerful::sync::bundle::{Bundle, Entry, Manifest};
use ledgerful::sync::hlc::HLC;
use std::collections::HashMap;

#[test]
fn test_bundle_serialize_round_trip() {
    let mut csprng = rand::thread_rng();
    let signing_key = SigningKey::generate(&mut csprng);
    let public_key = signing_key.verifying_key();

    let device_id = "test-device".to_string();
    let hlc = HLC {
        physical_ms: 1700000000000,
        logical: 0,
        node_id: device_id.clone(),
    };

    let entries = vec![Entry {
        tx_id: "uuid-1".to_string(),
        category: "FEATURE".to_string(),
        entry_type: "IMPLEMENTATION".to_string(),
        entity: "src/lib.rs".to_string(),
        entity_normalized: "src/lib.rs".to_string(),
        change_type: "MODIFY".to_string(),
        summary: "feat 1".to_string(),
        reason: "req 1".to_string(),
        is_breaking: false,
        committed_at: chrono::Utc::now(),
        origin: "LOCAL".to_string(),
        trace_id: Some(device_id.clone()),
        signature: Some("sig1".to_string()),
        public_key: Some("pub1".to_string()),
        risk: Some("LOW".to_string()),
        verification_status: None,
        verification_basis: None,
        outcome_notes: None,
        related_tickets: None,
        entry_hlc: hlc.clone(),
    }];

    let manifest = Manifest {
        version: 1,
        device_id: device_id.clone(),
        bundle_hlc: hlc.clone(),
        manifest_sha256: "fake-sha".to_string(),
        entry_count: entries.len(),
        entries,
        tombstones: vec![],
    };

    // Build bundle
    let (zip_bytes, signature) = Bundle::build(manifest, &signing_key).unwrap();

    let mut verify_keys = HashMap::new();
    verify_keys.insert(device_id.clone(), public_key.to_bytes());

    // Parse bundle
    let bundle = Bundle::parse(&zip_bytes, &verify_keys).unwrap();

    assert_eq!(bundle.manifest.device_id, device_id);
    assert_eq!(bundle.manifest.entry_count, 1);
    assert_eq!(bundle.signature, signature);
    assert_eq!(bundle.device_pub, public_key.to_bytes());
}

#[test]
fn test_bundle_filename_format() {
    let device_id = "ws-box".to_string();
    let hlc = HLC {
        physical_ms: 1718420400123, // 2024-06-15T03:00:00.123Z (actually physical_ms is just a number)
        logical: 1,
        node_id: device_id.clone(),
    };

    let manifest = Manifest {
        version: 1,
        device_id: device_id.clone(),
        bundle_hlc: hlc,
        manifest_sha256: "a1b2c3d4e5f6g7h8i9j0".to_string(),
        entry_count: 0,
        entries: vec![],
        tombstones: vec![],
    };

    // We expect something like: 2024-06-15T03-00-00-123Z-0001-ws-box-a1b2c3d4.zip.gpg
    // Note: filenames can't have colons on Windows easily, so let's use hyphens or just ISO8601-lite
    // The plan said: 2026-06-15T03:00:00.123Z-0001-ws-box-a1b2c3d4.zip.gpg
    // Wait, 2026-06-15T03:00:00.123Z has colons.

    let filename = manifest.filename();
    assert!(filename.contains("ws-box"));
    assert!(filename.contains("a1b2c3d4"));
    assert!(filename.ends_with(".zip.gpg"));
}

#[test]
fn test_bundle_rejects_wrong_signing_key() {
    let mut csprng = rand::thread_rng();
    let signing_key_a = SigningKey::generate(&mut csprng);
    let signing_key_b = SigningKey::generate(&mut csprng);
    let public_key_b = signing_key_b.verifying_key();

    let device_id = "test-device".to_string();
    let manifest = Manifest {
        version: 1,
        device_id: device_id.clone(),
        bundle_hlc: HLC {
            physical_ms: 1,
            logical: 0,
            node_id: device_id.clone(),
        },
        manifest_sha256: "".to_string(),
        entry_count: 0,
        entries: vec![],
        tombstones: vec![],
    };

    // Signed with A
    let (zip_bytes, _) = Bundle::build(manifest, &signing_key_a).unwrap();

    // Verifying with B
    let mut verify_keys = HashMap::new();
    verify_keys.insert(device_id, public_key_b.to_bytes());

    let result = Bundle::parse(&zip_bytes, &verify_keys);
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .contains("Signature verification failed")
    );
}

#[test]
fn test_bundle_rejects_tampered_manifest() {
    let mut csprng = rand::thread_rng();
    let signing_key = SigningKey::generate(&mut csprng);
    let public_key = signing_key.verifying_key();

    let device_id = "test-device".to_string();
    let manifest = Manifest {
        version: 1,
        device_id: device_id.clone(),
        bundle_hlc: HLC {
            physical_ms: 1,
            logical: 0,
            node_id: device_id.clone(),
        },
        manifest_sha256: "".to_string(),
        entry_count: 0,
        entries: vec![],
        tombstones: vec![],
    };

    let (zip_bytes, _) = Bundle::build(manifest, &signing_key).unwrap();

    // Tamper with ZIP (find manifest.json and change it)
    let mut tampered_zip = zip_bytes.clone();
    // Manifest is usually early in the ZIP. Let's just flip a bit and hope it hits the manifest content
    // and not the ZIP structure (which would also cause an error, but maybe not the one we expect).
    // Better: use zip crate to create a tampered one, but that's complex.
    // Let's just find "manifest.json" string and flip a bit after it.
    if let Some(pos) = tampered_zip.windows(13).position(|w| w == b"manifest.json") {
        tampered_zip[pos + 20] ^= 0x01;
    }

    let mut verify_keys = HashMap::new();
    verify_keys.insert(device_id, public_key.to_bytes());

    let result = Bundle::parse(&tampered_zip, &verify_keys);
    assert!(result.is_err());
}

#[test]
fn test_bundle_encrypt_decrypt_round_trip() {
    let key = [0u8; 32];
    let zip_bytes = b"Fake ZIP content".to_vec();

    let encrypted = Bundle::encrypt(&zip_bytes, &key).unwrap();
    let decrypted = Bundle::decrypt(&encrypted, &key).unwrap();

    assert_eq!(zip_bytes, decrypted);
}

#[test]
fn test_bundle_rejects_wrong_team_secret() {
    let key_a = [0u8; 32];
    let key_b = [1u8; 32];
    let zip_bytes = b"Fake ZIP content".to_vec();

    let encrypted = Bundle::encrypt(&zip_bytes, &key_a).unwrap();
    let result = Bundle::decrypt(&encrypted, &key_b);

    assert!(result.is_err());
}

#[test]
fn test_bundle_encrypt_rejects_short_ciphertext() {
    let key = [0u8; 32];

    // Minimum valid length is salt(16) + nonce(24) + tag(16) = 56 bytes.
    let short = vec![0u8; 55];
    let result = Bundle::decrypt(&short, &key);

    assert!(result.is_err());
}

#[test]
fn test_bundle_rejects_oversized_zip_input() {
    let device_id = "test-device".to_string();
    let mut verify_keys = HashMap::new();
    verify_keys.insert(device_id, [0u8; 32]);

    let oversized = vec![0u8; 256 * 1024 * 1024 + 1];
    let result = Bundle::parse(&oversized, &verify_keys);

    assert!(result.is_err());
    assert!(
        result.unwrap_err().contains("Bundle exceeds maximum size"),
        "expected bundle size cap error"
    );
}

#[test]
fn test_bundle_invalid_signature_is_rejected_before_full_deserialization() {
    use ed25519_dalek::SigningKey;

    let mut csprng = rand::thread_rng();
    let signing_key_a = SigningKey::generate(&mut csprng);
    let signing_key_b = SigningKey::generate(&mut csprng);

    // Build a manifest with a crafted "entries" array that would be expensive to deserialize
    // (many large strings) and sign it with key A.  Keep the serialized manifest under the
    // 64 MiB cap so the test exercises signature rejection, not the size cap.
    let device_id = "test-device".to_string();
    let expensive_entries: Vec<Entry> = (0..1_000)
        .map(|i| Entry {
            tx_id: format!("{}{}", "x".repeat(128), i),
            category: "FEATURE".to_string(),
            entry_type: "IMPLEMENTATION".to_string(),
            entity: "src/lib.rs".to_string(),
            entity_normalized: "src/lib.rs".to_string(),
            change_type: "MODIFY".to_string(),
            summary: "a".repeat(512),
            reason: "b".repeat(512),
            is_breaking: false,
            committed_at: chrono::Utc::now(),
            origin: "LOCAL".to_string(),
            trace_id: None,
            signature: None,
            public_key: None,
            risk: None,
            verification_status: None,
            verification_basis: None,
            outcome_notes: None,
            related_tickets: None,
            entry_hlc: HLC {
                physical_ms: 1,
                logical: 0,
                node_id: device_id.clone(),
            },
        })
        .collect();

    let manifest = Manifest {
        version: 1,
        device_id: device_id.clone(),
        bundle_hlc: HLC {
            physical_ms: 1,
            logical: 0,
            node_id: device_id.clone(),
        },
        manifest_sha256: "fake-sha".to_string(),
        entry_count: expensive_entries.len(),
        entries: expensive_entries,
        tombstones: vec![],
    };

    let (zip_bytes, _signature) = Bundle::build(manifest, &signing_key_a).unwrap();

    // Verify with key B (same device_id, wrong key) — signature must fail.
    let mut verify_keys = HashMap::new();
    verify_keys.insert(device_id, signing_key_b.verifying_key().to_bytes());

    let result = Bundle::parse(&zip_bytes, &verify_keys);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        err.contains("Signature verification failed"),
        "expected signature failure before full manifest deserialization, got: {}",
        err
    );
}
