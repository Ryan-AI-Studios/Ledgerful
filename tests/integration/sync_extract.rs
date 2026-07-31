//! Extract cursor integrity, empty path, batch drain, tombstone delta (0112).

#[cfg(feature = "sync")]
use ledgerful::config::model::Config;
#[cfg(feature = "sync")]
use ledgerful::ledger::*;
#[cfg(feature = "sync")]
use ledgerful::state::storage::StorageManager;
#[cfg(feature = "sync")]
use ledgerful::sync::error::SyncError;
#[cfg(feature = "sync")]
use ledgerful::sync::extract::{commit_extract_export, extract};
#[cfg(feature = "sync")]
use ledgerful::sync::hlc::HLC;
#[cfg(feature = "sync")]
use ledgerful::sync::state::SyncState;
#[cfg(feature = "sync")]
use rusqlite::Connection;
#[cfg(feature = "sync")]
use std::fs;
#[cfg(feature = "sync")]
use std::str::FromStr;
#[cfg(feature = "sync")]
use tempfile::tempdir;

#[cfg(feature = "sync")]
fn seed_signed_entries(repo_root: &std::path::Path, count: usize) {
    let state_dir = repo_root.join(".ledgerful").join("state");
    fs::create_dir_all(&state_dir).unwrap();
    let db_path = state_dir.join("ledger.db");
    let mut storage = StorageManager::init(&db_path).unwrap();

    let entity_path = repo_root.join("src/lib.rs");
    fs::create_dir_all(entity_path.parent().unwrap()).unwrap();
    fs::write(&entity_path, "").unwrap();

    let mut tx_mgr =
        TransactionManager::new(&mut storage, repo_root.to_path_buf(), Config::default());

    for i in 0..count {
        let entity = format!("src/file_{}.rs", i);
        let fpath = repo_root.join(&entity);
        fs::write(&fpath, "").unwrap();

        tx_mgr
            .atomic_change(
                TransactionRequest {
                    category: Category::Feature,
                    entity: entity.clone(),
                    ..Default::default()
                },
                CommitRequest {
                    change_type: ChangeType::Modify,
                    summary: format!("Summary {}", i),
                    reason: "Test".to_string(),
                    ..Default::default()
                },
                false,
            )
            .expect("Should create entry");
    }
}

#[test]
#[cfg(feature = "sync")]
fn test_extract_picks_up_new_committed_entries() {
    let dir = tempdir().unwrap();
    let repo_root = dir.path().to_path_buf();
    seed_signed_entries(&repo_root, 5);

    let mut csprng = rand::rngs::OsRng;
    let sign_key = ed25519_dalek::SigningKey::generate(&mut csprng);
    let device_id = "test-device";

    let result = extract(&repo_root.join(".ledgerful"), device_id, &sign_key, 100);
    match result {
        Ok(extracted) => {
            assert_eq!(extracted.bundle.manifest.entries.len(), 5);
            assert!(!extracted.zip_bytes.is_empty(), "must return signed zip");
        }
        Err(e) => panic!("Extract failed: {e:?}"),
    }
}

/// Extract must not null last_apply_hlc (REPLACE bug + SyncState::save clobber).
#[test]
#[cfg(feature = "sync")]
fn extract_preserves_last_apply_hlc() {
    let dir = tempdir().unwrap();
    let repo_root = dir.path().to_path_buf();
    seed_signed_entries(&repo_root, 2);

    let db_path = repo_root.join(".ledgerful/state/ledger.db");
    let conn = Connection::open(&db_path).unwrap();

    let apply_hlc = HLC {
        physical_ms: 1_700_000_000_000,
        logical: 7,
        node_id: "prior-apply".to_string(),
    };
    conn.execute(
        "INSERT INTO sync_state (id, last_extract_hlc, last_apply_hlc, device_id, last_run_at)
         VALUES (1, NULL, ?1, 'test-device', NULL)
         ON CONFLICT(id) DO UPDATE SET last_apply_hlc = excluded.last_apply_hlc, device_id = excluded.device_id",
        [apply_hlc.to_string()],
    )
    .unwrap();

    let mut csprng = rand::rngs::OsRng;
    let sign_key = ed25519_dalek::SigningKey::generate(&mut csprng);

    let state_path = repo_root.join(".ledgerful");
    let extracted =
        extract(&state_path, "test-device", &sign_key, 100).expect("extract should succeed");
    assert!(!extracted.zip_bytes.is_empty());

    // Prepare path must not touch apply cursor even before commit.
    let pre = SyncState::load(&conn).unwrap().expect("sync_state row");
    assert_eq!(
        pre.last_apply_hlc.as_ref().map(|h| h.to_string()),
        Some(apply_hlc.to_string()),
        "prepare extract must not clear last_apply_hlc"
    );

    commit_extract_export(&state_path, &extracted, "test-device").expect("commit export");

    let state = SyncState::load(&conn).unwrap().expect("sync_state row");
    assert_eq!(
        state.last_apply_hlc.as_ref().map(|h| h.to_string()),
        Some(apply_hlc.to_string()),
        "commit_extract_export must not clear last_apply_hlc"
    );
    assert!(
        state.last_extract_hlc.is_some(),
        "commit_extract_export must advance last_extract_hlc"
    );
}

/// Empty extract returns NoNewEntries without advancing watermark.
#[test]
#[cfg(feature = "sync")]
fn empty_extract_does_not_advance_watermark() {
    let dir = tempdir().unwrap();
    let repo_root = dir.path().to_path_buf();
    let state_dir = repo_root.join(".ledgerful").join("state");
    fs::create_dir_all(&state_dir).unwrap();
    let db_path = state_dir.join("ledger.db");
    let _storage = StorageManager::init(&db_path).unwrap();
    let conn = Connection::open(&db_path).unwrap();

    let prior = HLC {
        physical_ms: 1_600_000_000_000,
        logical: 3,
        node_id: "dev".to_string(),
    };
    conn.execute(
        "INSERT INTO sync_state (id, last_extract_hlc, last_apply_hlc, device_id)
         VALUES (1, ?1, '1700000000000-0001-prior', 'dev')",
        [prior.to_string()],
    )
    .unwrap();

    let mut csprng = rand::rngs::OsRng;
    let sign_key = ed25519_dalek::SigningKey::generate(&mut csprng);

    let err =
        extract(&repo_root.join(".ledgerful"), "dev", &sign_key, 100).expect_err("empty extract");
    assert!(
        matches!(err, SyncError::NoNewEntries),
        "expected NoNewEntries, got {err:?}"
    );

    let extract_hlc: String = conn
        .query_row(
            "SELECT last_extract_hlc FROM sync_state WHERE id = 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(extract_hlc, prior.to_string());

    let apply: String = conn
        .query_row(
            "SELECT last_apply_hlc FROM sync_state WHERE id = 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(apply, "1700000000000-0001-prior");
}

/// batch_size LIMIT drains across two extracts.
#[test]
#[cfg(feature = "sync")]
fn batch_size_drains_across_two_extracts() {
    let dir = tempdir().unwrap();
    let repo_root = dir.path().to_path_buf();
    seed_signed_entries(&repo_root, 5);

    let mut csprng = rand::rngs::OsRng;
    let sign_key = ed25519_dalek::SigningKey::generate(&mut csprng);
    let state = repo_root.join(".ledgerful");

    let first = extract(&state, "dev", &sign_key, 3).expect("first extract");
    assert_eq!(first.bundle.manifest.entries.len(), 3);
    commit_extract_export(&state, &first, "dev").expect("commit first");

    let second = extract(&state, "dev", &sign_key, 3).expect("second extract");
    assert_eq!(second.bundle.manifest.entries.len(), 2);
    commit_extract_export(&state, &second, "dev").expect("commit second");

    let third = extract(&state, "dev", &sign_key, 3);
    assert!(matches!(third, Err(SyncError::NoNewEntries)));
}

/// Tombstones older than watermark are not re-exported every run.
#[test]
#[cfg(feature = "sync")]
fn tombstone_delta_not_reexported() {
    let dir = tempdir().unwrap();
    let repo_root = dir.path().to_path_buf();
    let state_dir = repo_root.join(".ledgerful").join("state");
    fs::create_dir_all(&state_dir).unwrap();
    let db_path = state_dir.join("ledger.db");
    let _storage = StorageManager::init(&db_path).unwrap();
    let conn = Connection::open(&db_path).unwrap();

    // Seed device with NULL extract watermark so first extract picks all tombstones.
    conn.execute(
        "INSERT INTO sync_state (id, last_extract_hlc, device_id)
         VALUES (1, NULL, 'dev')
         ON CONFLICT(id) DO UPDATE SET last_extract_hlc = NULL, device_id = 'dev'",
        [],
    )
    .unwrap();

    // Insert tombstones with HLCs
    conn.execute(
        "INSERT INTO tx_tombstones (tx_id, tombstone_hlc, reason) VALUES (?1, ?2, ?3)",
        ("tx-old", "1000000000000-0001-dev", "old"),
    )
    .unwrap();
    conn.execute(
        "INSERT INTO tx_tombstones (tx_id, tombstone_hlc, reason) VALUES (?1, ?2, ?3)",
        ("tx-new", "2000000000000-0001-dev", "new"),
    )
    .unwrap();

    let mut csprng = rand::rngs::OsRng;
    let sign_key = ed25519_dalek::SigningKey::generate(&mut csprng);
    let state = repo_root.join(".ledgerful");

    let first = extract(&state, "dev", &sign_key, 100).expect("first");
    assert_eq!(first.bundle.manifest.tombstones.len(), 2);
    commit_extract_export(&state, &first, "dev").expect("commit first");

    // Second extract: no new entries/tombstones → NoNewEntries (watermark advanced past both)
    let second = extract(&state, "dev", &sign_key, 100);
    assert!(
        matches!(second, Err(SyncError::NoNewEntries)),
        "tombstones must not re-export; got {second:?}"
    );

    // Insert a newer tombstone only
    let after_extract = SyncState::load(&conn)
        .unwrap()
        .unwrap()
        .last_extract_hlc
        .unwrap();
    let newer = HLC {
        physical_ms: after_extract.physical_ms + 10_000,
        logical: 0,
        node_id: "dev".to_string(),
    };
    conn.execute(
        "INSERT INTO tx_tombstones (tx_id, tombstone_hlc, reason) VALUES (?1, ?2, ?3)",
        ("tx-later", newer.to_string(), "later"),
    )
    .unwrap();

    let third = extract(&state, "dev", &sign_key, 100).expect("third");
    assert_eq!(third.bundle.manifest.tombstones.len(), 1);
    assert_eq!(third.bundle.manifest.tombstones[0].tx_id, "tx-later");
    commit_extract_export(&state, &third, "dev").expect("commit third");
}

/// Extract must not call full SyncState::save (which can write last_apply_hlc=NULL).
/// Proven by: seed apply cursor, load with Option that would clobber if save used, extract.
#[test]
#[cfg(feature = "sync")]
fn extract_does_not_use_full_save_clobber() {
    let dir = tempdir().unwrap();
    let repo_root = dir.path().to_path_buf();
    seed_signed_entries(&repo_root, 1);
    let db_path = repo_root.join(".ledgerful/state/ledger.db");
    let conn = Connection::open(&db_path).unwrap();

    // Seed apply cursor
    conn.execute(
        "INSERT INTO sync_state (id, last_apply_hlc, device_id)
         VALUES (1, '1699999999999-0002-keep-me', 'dev')
         ON CONFLICT(id) DO UPDATE SET last_apply_hlc = '1699999999999-0002-keep-me', device_id = 'dev'",
        [],
    )
    .unwrap();

    // If commit used SyncState::save with last_apply_hlc=None, this would become NULL.
    let mut csprng = rand::rngs::OsRng;
    let sign_key = ed25519_dalek::SigningKey::generate(&mut csprng);
    let state = repo_root.join(".ledgerful");
    let extracted = extract(&state, "dev", &sign_key, 100).unwrap();
    commit_extract_export(&state, &extracted, "dev").unwrap();

    let apply: Option<String> = conn
        .query_row(
            "SELECT last_apply_hlc FROM sync_state WHERE id = 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        apply.as_deref(),
        Some("1699999999999-0002-keep-me"),
        "partial extract cursor must not clobber apply"
    );
    // sanity parse
    let _ = HLC::from_str(apply.as_ref().unwrap()).unwrap();
}

/// Failed put must not permanently drop deltas: prepare-only extract leaves
/// entry_hlc/watermark untouched so retry re-selects the same rows.
#[test]
#[cfg(feature = "sync")]
fn failed_put_does_not_drop_pending_export_rows() {
    let dir = tempdir().unwrap();
    let repo_root = dir.path().to_path_buf();
    seed_signed_entries(&repo_root, 3);
    let state = repo_root.join(".ledgerful");

    let mut csprng = rand::rngs::OsRng;
    let sign_key = ed25519_dalek::SigningKey::generate(&mut csprng);

    let first = extract(&state, "dev", &sign_key, 100).expect("prepare extract");
    assert_eq!(first.bundle.manifest.entries.len(), 3);
    // Simulate put failure: do NOT call commit_extract_export.

    let retry = extract(&state, "dev", &sign_key, 100).expect("retry after failed put");
    assert_eq!(
        retry.bundle.manifest.entries.len(),
        3,
        "without commit, pending entries must remain selectable"
    );

    let db_path = state.join("state/ledger.db");
    let conn = Connection::open(&db_path).unwrap();
    let stamped: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM ledger_entries WHERE entry_hlc IS NOT NULL",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(stamped, 0, "prepare-only extract must not stamp entry_hlc");
}
