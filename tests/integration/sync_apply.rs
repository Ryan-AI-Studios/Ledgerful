#[cfg(feature = "sync")]
use ledgerful::state::storage::StorageManager;
#[cfg(feature = "sync")]
use ledgerful::sync::apply::apply;
#[cfg(feature = "sync")]
use ledgerful::sync::bundle::{Bundle, Entry, Manifest};
#[cfg(feature = "sync")]
use ledgerful::sync::hlc::HLC;
#[cfg(feature = "sync")]
use rusqlite::Connection;
#[cfg(feature = "sync")]
use std::collections::HashMap;
#[cfg(feature = "sync")]
use tempfile::tempdir;

#[test]
#[cfg(feature = "sync")]
fn test_apply_inserts_new_entries() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("ledger.db");

    // Initialize storage (runs migrations)
    let _storage = StorageManager::init(&db_path).unwrap();
    let mut conn = Connection::open(&db_path).unwrap();

    let device_id = "device-1";
    let bundle_hlc = HLC {
        physical_ms: 1000,
        logical: 0,
        node_id: device_id.to_string(),
    };

    let entry = Entry {
        tx_id: "tx-1".to_string(),
        category: "FEATURE".to_string(),
        entry_type: "COMMIT".to_string(),
        entity: "src/lib.rs".to_string(),
        entity_normalized: "src/lib.rs".to_string(),
        change_type: "MODIFY".to_string(),
        summary: "Test entry".to_string(),
        reason: "Testing sync".to_string(),
        is_breaking: false,
        committed_at: chrono::Utc::now(),
        origin: "LOCAL".to_string(),
        trace_id: Some("trace-1".to_string()),
        signature: Some("sig-1".to_string()),
        public_key: Some("pub-1".to_string()),
        risk: None,
        verification_status: None,
        verification_basis: None,
        outcome_notes: None,
        related_tickets: None,
        entry_hlc: HLC {
            physical_ms: 1001,
            logical: 0,
            node_id: device_id.to_string(),
        },
    };

    let manifest = Manifest {
        version: 1,
        device_id: device_id.to_string(),
        bundle_hlc: bundle_hlc.clone(),
        manifest_sha256: "fake-sha".to_string(),
        entry_count: 1,
        entries: vec![entry.clone()],
        tombstones: vec![],
    };

    let bundle = Bundle {
        manifest,
        signature: [0u8; 64],
        device_pub: [0u8; 32],
    };

    let mut device_keys = HashMap::new();
    device_keys.insert(device_id.to_string(), [0u8; 32]);

    let result = apply(&bundle, &mut conn, &device_keys).expect("Apply should succeed");

    assert_eq!(result.inserted, 1);
    assert_eq!(result.total_entries, 1);

    // Verify entry in DB
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM ledger_entries WHERE tx_id = 'tx-1'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 1);
}

#[test]
#[cfg(feature = "sync")]
fn test_apply_idempotent() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("ledger.db");
    StorageManager::init(&db_path).unwrap();
    let mut conn = Connection::open(&db_path).unwrap();

    let device_id = "device-1";
    let entry = Entry {
        tx_id: "tx-1".to_string(),
        category: "FEATURE".to_string(),
        entry_type: "COMMIT".to_string(),
        entity: "src/lib.rs".to_string(),
        entity_normalized: "src/lib.rs".to_string(),
        change_type: "MODIFY".to_string(),
        summary: "Test entry".to_string(),
        reason: "Testing sync".to_string(),
        is_breaking: false,
        committed_at: chrono::Utc::now(),
        origin: "LOCAL".to_string(),
        trace_id: Some("trace-1".to_string()),
        signature: Some("sig-1".to_string()),
        public_key: Some("pub-1".to_string()),
        risk: None,
        verification_status: None,
        verification_basis: None,
        outcome_notes: None,
        related_tickets: None,
        entry_hlc: HLC {
            physical_ms: 1001,
            logical: 0,
            node_id: device_id.to_string(),
        },
    };

    let manifest = Manifest {
        version: 1,
        device_id: device_id.to_string(),
        bundle_hlc: HLC {
            physical_ms: 1000,
            logical: 0,
            node_id: device_id.to_string(),
        },
        manifest_sha256: "fake-sha".to_string(),
        entry_count: 1,
        entries: vec![entry.clone()],
        tombstones: vec![],
    };

    let bundle = Bundle {
        manifest,
        signature: [0u8; 64],
        device_pub: [0u8; 32],
    };

    let device_keys = HashMap::new();

    let result1 = apply(&bundle, &mut conn, &device_keys).expect("First apply should succeed");
    assert_eq!(result1.inserted, 1);

    let result2 = apply(&bundle, &mut conn, &device_keys).expect("Second apply should succeed");
    assert_eq!(result2.inserted, 0);
    assert_eq!(result2.skipped, 1);
}

#[test]
#[cfg(feature = "sync")]
fn test_apply_lww_resolution() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("ledger.db");
    StorageManager::init(&db_path).unwrap();
    let mut conn = Connection::open(&db_path).unwrap();

    let device_id = "device-1";
    let tx_id = "tx-shared";

    // 1. Initial entry
    let entry1 = Entry {
        tx_id: tx_id.to_string(),
        category: "FEATURE".to_string(),
        entry_type: "COMMIT".to_string(),
        entity: "src/lib.rs".to_string(),
        entity_normalized: "src/lib.rs".to_string(),
        change_type: "MODIFY".to_string(),
        summary: "First summary".to_string(),
        reason: "First reason".to_string(),
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
            physical_ms: 1001,
            logical: 0,
            node_id: device_id.to_string(),
        },
    };

    let bundle1 = Bundle {
        manifest: Manifest {
            version: 1,
            device_id: device_id.to_string(),
            bundle_hlc: HLC {
                physical_ms: 1000,
                logical: 0,
                node_id: device_id.to_string(),
            },
            manifest_sha256: "fake-sha-1".to_string(),
            entry_count: 1,
            entries: vec![entry1],
            tombstones: vec![],
        },
        signature: [0u8; 64],
        device_pub: [0u8; 32],
    };

    apply(&bundle1, &mut conn, &HashMap::new()).unwrap();

    // 2. Later entry (higher HLC)
    let entry2 = Entry {
        tx_id: tx_id.to_string(),
        category: "FEATURE".to_string(),
        entry_type: "COMMIT".to_string(),
        entity: "src/lib.rs".to_string(),
        entity_normalized: "src/lib.rs".to_string(),
        change_type: "MODIFY".to_string(),
        summary: "Updated summary".to_string(),
        reason: "Updated reason".to_string(),
        is_breaking: false,
        committed_at: chrono::Utc::now(),
        origin: "LOCAL".to_string(),
        trace_id: None,
        signature: None,
        public_key: None,
        risk: None,
        verification_status: Some("PASSED".to_string()),
        verification_basis: None,
        outcome_notes: Some("Updated notes".to_string()),
        related_tickets: None,
        entry_hlc: HLC {
            physical_ms: 1002,
            logical: 0,
            node_id: device_id.to_string(),
        },
    };

    let bundle2 = Bundle {
        manifest: Manifest {
            version: 1,
            device_id: device_id.to_string(),
            bundle_hlc: HLC {
                physical_ms: 1005,
                logical: 0,
                node_id: device_id.to_string(),
            },
            manifest_sha256: "fake-sha-2".to_string(),
            entry_count: 1,
            entries: vec![entry2],
            tombstones: vec![],
        },
        signature: [0u8; 64],
        device_pub: [0u8; 32],
    };

    let report = apply(&bundle2, &mut conn, &HashMap::new()).unwrap();
    assert_eq!(report.updated, 1);

    // Verify summary is NOT updated (immutable), but status is updated
    let (summary, status, notes): (String, Option<String>, Option<String>) = conn.query_row(
        "SELECT summary, verification_status, outcome_notes FROM ledger_entries WHERE tx_id = ?1",
        [tx_id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    ).unwrap();
    assert_eq!(summary, "First summary");
    assert_eq!(status, Some("PASSED".to_string()));
    assert_eq!(notes, Some("Updated notes".to_string()));

    // 3. Stale entry (lower HLC)
    let entry3 = Entry {
        tx_id: tx_id.to_string(),
        category: "FEATURE".to_string(),
        entry_type: "COMMIT".to_string(),
        entity: "src/lib.rs".to_string(),
        entity_normalized: "src/lib.rs".to_string(),
        change_type: "MODIFY".to_string(),
        summary: "Stale summary".to_string(),
        reason: "Stale reason".to_string(),
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
            physical_ms: 999,
            logical: 0,
            node_id: device_id.to_string(),
        },
    };

    let bundle3 = Bundle {
        manifest: Manifest {
            version: 1,
            device_id: device_id.to_string(),
            bundle_hlc: HLC {
                physical_ms: 1010,
                logical: 0,
                node_id: device_id.to_string(),
            },
            manifest_sha256: "fake-sha-3".to_string(),
            entry_count: 1,
            entries: vec![entry3],
            tombstones: vec![],
        },
        signature: [0u8; 64],
        device_pub: [0u8; 32],
    };

    let report3 = apply(&bundle3, &mut conn, &HashMap::new()).unwrap();
    assert_eq!(report3.skipped, 1);

    // Verify summary is NOT updated
    let summary2: String = conn
        .query_row(
            "SELECT summary FROM ledger_entries WHERE tx_id = ?1",
            [tx_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(summary2, "First summary");
}

#[test]
#[cfg(feature = "sync")]
fn test_apply_tombstone_marks_transaction_rolled_back() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("ledger.db");
    StorageManager::init(&db_path).unwrap();
    let mut conn = Connection::open(&db_path).unwrap();

    let device_id = "device-1";
    let tx_id = "tx-to-be-rolled-back";

    // 1. Insert entry
    let entry = Entry {
        tx_id: tx_id.to_string(),
        category: "FEATURE".to_string(),
        entry_type: "COMMIT".to_string(),
        entity: "src/lib.rs".to_string(),
        entity_normalized: "src/lib.rs".to_string(),
        change_type: "MODIFY".to_string(),
        summary: "Normal entry".to_string(),
        reason: "Reason".to_string(),
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
            physical_ms: 1001,
            logical: 0,
            node_id: device_id.to_string(),
        },
    };

    apply(
        &Bundle {
            manifest: Manifest {
                version: 1,
                device_id: device_id.to_string(),
                bundle_hlc: HLC {
                    physical_ms: 1000,
                    logical: 0,
                    node_id: device_id.to_string(),
                },
                manifest_sha256: "sha-1".to_string(),
                entry_count: 1,
                entries: vec![entry],
                tombstones: vec![],
            },
            signature: [0u8; 64],
            device_pub: [0u8; 32],
        },
        &mut conn,
        &HashMap::new(),
    )
    .unwrap();

    // 2. Apply tombstone
    let tombstone = ledgerful::sync::bundle::Tombstone {
        tx_id: tx_id.to_string(),
        tombstone_hlc: HLC {
            physical_ms: 1002,
            logical: 0,
            node_id: device_id.to_string(),
        },
        reason: "User rollback".to_string(),
    };

    apply(
        &Bundle {
            manifest: Manifest {
                version: 1,
                device_id: device_id.to_string(),
                bundle_hlc: HLC {
                    physical_ms: 1005,
                    logical: 0,
                    node_id: device_id.to_string(),
                },
                manifest_sha256: "sha-2".to_string(),
                entry_count: 0,
                entries: vec![],
                tombstones: vec![tombstone],
            },
            signature: [0u8; 64],
            device_pub: [0u8; 32],
        },
        &mut conn,
        &HashMap::new(),
    )
    .unwrap();

    // 3. Verify status
    let status: String = conn
        .query_row(
            "SELECT verification_status FROM ledger_entries WHERE tx_id = ?1",
            [tx_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(status, "ROLLED_BACK");
}
