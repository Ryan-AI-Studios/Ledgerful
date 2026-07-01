use ledgerful::commands::init::execute_init;
use ledgerful::commands::ledger::execute_ledger_status;
use ledgerful::config::model::Config;
use ledgerful::ledger::crypto::{sign_ledger_entry_in, verify_signature};
use ledgerful::ledger::*;
use ledgerful::state::storage::StorageManager;
use serial_test::serial;
use tempfile::{TempDir, tempdir};

use crate::common::{DirGuard, non_interactive, setup_git_repo};

fn setup_storage() -> (TempDir, StorageManager) {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("ledger.db");
    let storage = StorageManager::init(&db_path).unwrap();
    (dir, storage)
}

fn keys_dir(dir: &std::path::Path) -> std::path::PathBuf {
    dir.join(".ledgerful").join("keys")
}

#[test]
fn test_timestamp_preservation_and_signature_validity() {
    let (dir, mut storage) = setup_storage();
    let repo_root = dir.path().to_path_buf();

    let keys_dir = keys_dir(dir.path());
    std::fs::create_dir_all(&keys_dir).unwrap();

    // Create the file so canonicalize works
    let entity_path = repo_root.join("src/main.rs");
    std::fs::create_dir_all(entity_path.parent().unwrap()).unwrap();
    std::fs::write(&entity_path, "").unwrap();

    let mut tx_mgr = TransactionManager::new(&mut storage, repo_root.clone(), Config::default());

    let entity = "src/main.rs";
    let category = Category::Feature;
    let tx_id = tx_mgr
        .start_change(TransactionRequest {
            category,
            entity: entity.to_string(),
            ..Default::default()
        })
        .expect("Should start transaction");

    // Pre-calculate signature with a fixed timestamp
    let summary = "Fixed timestamp commit";
    let reason = "TDD signature fix";
    let committed_at = "2024-06-01T10:00:00Z";

    let (sig, pub_key) = sign_ledger_entry_in(
        &keys_dir,
        &tx_id,
        &category.to_string(),
        summary,
        reason,
        committed_at,
    )
    .expect("Signing failed");

    let sig_str = sig.expect("No signature");
    let pub_str = pub_key.expect("No public key");

    // Commit with the explicit timestamp
    tx_mgr
        .commit_change(
            tx_id.clone(),
            CommitRequest {
                change_type: ChangeType::Modify,
                summary: summary.to_string(),
                reason: reason.to_string(),
                committed_at: Some(committed_at.to_string()),
                signature: Some(sig_str.clone()),
                public_key: Some(pub_str.clone()),
                ..Default::default()
            },
            false,
        )
        .expect("Should commit transaction");

    // Verify committed entry has the correct timestamp in DB
    let entries = tx_mgr
        .get_ledger_entries_for_tx(&tx_id)
        .expect("Should find entries");
    assert_eq!(entries.len(), 1);
    assert_eq!(
        entries[0].committed_at, committed_at,
        "Timestamp was not preserved in database"
    );

    // Verify signature remains valid using the DB entry
    let entry = &entries[0];
    let is_valid = verify_signature(
        &entry.tx_id,
        &entry.category.to_string(),
        &entry.summary,
        &entry.reason,
        &entry.committed_at,
        entry.signature.as_ref().unwrap(),
        entry.public_key.as_ref().unwrap(),
    );

    assert!(
        is_valid,
        "Signature validation failed because timestamp drifted or was ignored"
    );
}

#[test]
#[serial(cwd)]
fn ledger_status_verify_signatures_rejects_corrupted_signature() {
    let _env_non_interactive = non_interactive();
    let dir = tempdir().unwrap();
    setup_git_repo(dir.path());
    let root = camino::Utf8Path::from_path(dir.path())
        .unwrap()
        .to_path_buf();
    let _guard = DirGuard::from_utf8(&root);

    let keys_dir = keys_dir(dir.path());
    std::fs::create_dir_all(&keys_dir).unwrap();

    execute_init(false).unwrap();

    let entity_path = root.join("src/main.rs");
    std::fs::create_dir_all(entity_path.parent().unwrap()).unwrap();
    std::fs::write(&entity_path, "").unwrap();

    let db_path = root.join(".ledgerful").join("state").join("ledger.db");
    let mut storage = StorageManager::init(db_path.as_std_path()).unwrap();
    let mut tx_mgr = TransactionManager::new(&mut storage, root.clone().into(), Config::default());

    let category = Category::Feature;
    let tx_id = tx_mgr
        .start_change(TransactionRequest {
            category,
            entity: "src/main.rs".to_string(),
            ..Default::default()
        })
        .unwrap();
    let summary = "Signed commit";
    let reason = "Exercise ledger status signature verification";
    let committed_at = "2026-06-03T00:00:00Z";
    let (sig, public_key) = sign_ledger_entry_in(
        &keys_dir,
        &tx_id,
        &category.to_string(),
        summary,
        reason,
        committed_at,
    )
    .unwrap();

    tx_mgr
        .commit_change(
            tx_id.clone(),
            CommitRequest {
                change_type: ChangeType::Modify,
                summary: summary.to_string(),
                reason: reason.to_string(),
                committed_at: Some(committed_at.to_string()),
                signature: sig,
                public_key,
                ..Default::default()
            },
            false,
        )
        .unwrap();
    drop(tx_mgr);

    storage
        .get_connection_mut()
        .execute(
            "UPDATE ledger_entries SET signature = 'corrupted' WHERE tx_id = ?1",
            [&tx_id],
        )
        .unwrap();

    let err = execute_ledger_status(None, true, true, true, false, false).unwrap_err();

    assert!(format!("{err}").contains("Ledger signature verification failed"));
}

/// `ledger status --all` must show the full committed history instead of
/// truncating to the most recent 10 entries, both for an entity-scoped query
/// and for the repo-wide view.
#[test]
#[serial(cwd)]
fn test_ledger_status_all_flag_succeeds_with_more_than_ten_entries() {
    let _env_non_interactive = non_interactive();
    let dir = tempdir().unwrap();
    setup_git_repo(dir.path());
    let root = camino::Utf8Path::from_path(dir.path())
        .unwrap()
        .to_path_buf();
    let _guard = DirGuard::from_utf8(&root);

    let keys_dir = keys_dir(dir.path());
    std::fs::create_dir_all(&keys_dir).unwrap();

    execute_init(false).unwrap();

    let entity_path = root.join("src/main.rs");
    std::fs::create_dir_all(entity_path.parent().unwrap()).unwrap();
    std::fs::write(&entity_path, "").unwrap();

    let db_path = root.join(".ledgerful").join("state").join("ledger.db");
    let mut storage = StorageManager::init(db_path.as_std_path()).unwrap();
    let mut tx_mgr = TransactionManager::new(&mut storage, root.clone().into(), Config::default());

    for i in 0..15 {
        let tx_id = tx_mgr
            .start_change(TransactionRequest {
                category: Category::Chore,
                entity: "src/main.rs".to_string(),
                ..Default::default()
            })
            .unwrap();
        tx_mgr
            .commit_change(
                tx_id,
                CommitRequest {
                    change_type: ChangeType::Modify,
                    summary: format!("entry {i}"),
                    reason: "bulk history for --all test".to_string(),
                    ..Default::default()
                },
                false,
            )
            .unwrap();
    }
    drop(tx_mgr);

    // Entity-scoped: --all extends the per-entity history beyond the default
    // top-10 truncation.
    assert!(
        execute_ledger_status(
            Some("src/main.rs".to_string()),
            false,
            false,
            false,
            false,
            true
        )
        .is_ok()
    );

    // Repo-wide (no --entity): --all adds the "RECENT HISTORY" section.
    assert!(execute_ledger_status(None, false, false, false, false, true).is_ok());
}
