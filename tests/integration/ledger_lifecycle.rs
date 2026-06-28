use ledgerful::config::model::Config;
use ledgerful::ledger::*;
use ledgerful::state::storage::StorageManager;
use tempfile::{TempDir, tempdir};

fn setup_storage() -> (TempDir, StorageManager) {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("ledger.db");
    let storage = StorageManager::init(&db_path).unwrap();
    (dir, storage)
}

#[test]
fn test_ledger_start_commit_roundtrip() {
    let (dir, mut storage) = setup_storage();
    let repo_root = dir.path().to_path_buf();

    // Create the file so canonicalize works
    let entity_path = repo_root.join("src/main.rs");
    std::fs::create_dir_all(entity_path.parent().unwrap()).unwrap();
    std::fs::write(&entity_path, "").unwrap();

    let mut tx_mgr = TransactionManager::new(&mut storage, repo_root, Config::default());

    let entity = "src/main.rs";
    let tx_id = tx_mgr
        .start_change(TransactionRequest {
            category: Category::Feature,
            entity: entity.to_string(),
            planned_action: Some("Add new CLI command".to_string()),
            ..Default::default()
        })
        .expect("Should start transaction");

    assert!(!tx_id.is_empty());

    // Verify pending
    let pending = tx_mgr
        .get_pending(entity)
        .expect("Should find pending")
        .expect("Should be Some");
    assert_eq!(pending.tx_id, tx_id);
    assert_eq!(pending.status, "PENDING");

    // Commit
    tx_mgr
        .commit_change(
            tx_id.clone(),
            CommitRequest {
                change_type: ChangeType::Modify,
                summary: "Implemented ledger start command".to_string(),
                reason: "Part of track L1-2".to_string(),
                ..Default::default()
            },
            false,
        )
        .expect("Should commit transaction");

    // Verify committed
    let tx = tx_mgr
        .get_transaction(&tx_id)
        .expect("Should find tx")
        .expect("Should be Some");
    assert_eq!(tx.status, "COMMITTED");

    // Verify ledger entry exists
    let entries = tx_mgr
        .get_ledger_entries_for_tx(&tx_id)
        .expect("Should find entries");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].summary, "Implemented ledger start command");
}

#[test]
fn test_ledger_start_rollback() {
    let (dir, mut storage) = setup_storage();
    let repo_root = dir.path().to_path_buf();

    let entity_path = repo_root.join("src/lib.rs");
    std::fs::create_dir_all(entity_path.parent().unwrap()).unwrap();
    std::fs::write(&entity_path, "").unwrap();

    let mut tx_mgr = TransactionManager::new(&mut storage, repo_root, Config::default());

    let entity = "src/lib.rs";
    let tx_id = tx_mgr
        .start_change(TransactionRequest {
            category: Category::Bugfix,
            entity: entity.to_string(),
            ..Default::default()
        })
        .unwrap();

    tx_mgr
        .rollback_change(tx_id.clone(), "test rollback".to_string())
        .expect("Should rollback");

    let tx = tx_mgr
        .get_transaction(&tx_id)
        .expect("Should find tx")
        .expect("Should be Some");
    assert_eq!(tx.status, "ROLLED_BACK");

    // Should be able to start a new one for same entity
    let new_tx_id = tx_mgr
        .start_change(TransactionRequest {
            category: Category::Bugfix,
            entity: entity.to_string(),
            ..Default::default()
        })
        .expect("Should start new transaction after rollback");

    assert_ne!(tx_id, new_tx_id);
}

#[test]
fn test_ledger_conflict() {
    let (dir, mut storage) = setup_storage();
    let repo_root = dir.path().to_path_buf();

    let entity_path = repo_root.join("src/util.rs");
    std::fs::create_dir_all(entity_path.parent().unwrap()).unwrap();
    std::fs::write(&entity_path, "").unwrap();

    let mut tx_mgr = TransactionManager::new(&mut storage, repo_root, Config::default());

    let entity = "src/util.rs";
    tx_mgr
        .start_change(TransactionRequest {
            category: Category::Refactor,
            entity: entity.to_string(),
            ..Default::default()
        })
        .unwrap();

    let result = tx_mgr.start_change(TransactionRequest {
        category: Category::Refactor,
        entity: entity.to_string(),
        ..Default::default()
    });

    match result {
        Err(LedgerError::Conflict(_)) => {}
        _ => panic!("Expected conflict error, got {:?}", result),
    }
}

#[test]
fn test_ledger_atomic() {
    let (dir, mut storage) = setup_storage();
    let repo_root = dir.path().to_path_buf();

    let entity_path = repo_root.join("src/main.rs");
    std::fs::create_dir_all(entity_path.parent().unwrap()).unwrap();
    std::fs::write(&entity_path, "").unwrap();

    let mut tx_mgr = TransactionManager::new(&mut storage, repo_root, Config::default());

    let entity = "src/main.rs";
    tx_mgr
        .atomic_change(
            TransactionRequest {
                category: Category::Docs,
                entity: entity.to_string(),
                ..Default::default()
            },
            CommitRequest {
                change_type: ChangeType::Modify,
                summary: "Update README".to_string(),
                reason: "Documentation".to_string(),
                ..Default::default()
            },
            false,
        )
        .expect("Should perform atomic change");

    let pending = tx_mgr.get_pending(entity).unwrap();
    assert!(pending.is_none());

    let entries = tx_mgr.get_ledger_entries(entity).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].summary, "Update README");
}

#[test]
fn test_fuzzy_matching() {
    let (dir, mut storage) = setup_storage();
    let repo_root = dir.path().to_path_buf();

    let entity_path = repo_root.join("test.txt");
    std::fs::write(&entity_path, "").unwrap();

    let mut tx_mgr = TransactionManager::new(&mut storage, repo_root, Config::default());

    let real_id = tx_mgr
        .start_change(TransactionRequest {
            category: Category::Feature,
            entity: "test.txt".to_string(),
            ..Default::default()
        })
        .unwrap();

    let prefix = &real_id[..8];
    let matched = tx_mgr.resolve_tx_id(prefix).expect("Should resolve prefix");
    assert_eq!(matched, real_id);
}
