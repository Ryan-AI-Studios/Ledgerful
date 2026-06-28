use ledgerful::config::model::Config;
use ledgerful::ledger::drift::DriftManager;
use ledgerful::ledger::*;
use ledgerful::state::storage::StorageManager;
use std::fs;
use tempfile::{TempDir, tempdir};

fn setup_storage() -> (TempDir, StorageManager) {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("ledger.db");
    let storage = StorageManager::init(&db_path).unwrap();
    (dir, storage)
}

#[test]
fn test_drift_detection_creates_unaudited() {
    let (dir, mut storage) = setup_storage();
    let repo_root = dir.path().to_path_buf();

    // Create the file
    let entity = "drift_test.rs";
    let entity_path = repo_root.join(entity);
    fs::write(&entity_path, "").unwrap();

    {
        let mut drift_mgr = DriftManager::new(
            storage.get_connection_mut(),
            repo_root.clone(),
            Config::default(),
        );
        drift_mgr
            .process_event(entity)
            .expect("Should process drift");
    }

    let tx_mgr = TransactionManager::new(&mut storage, repo_root, Config::default());
    let unaudited = tx_mgr.get_all_unaudited().expect("Should get unaudited");

    assert_eq!(unaudited.len(), 1);
    assert_eq!(unaudited[0].entity, entity);
    assert_eq!(unaudited[0].status, "UNAUDITED");
    assert_eq!(unaudited[0].drift_count, 1);
}

#[test]
fn test_drift_detection_increments_count() {
    let (dir, mut storage) = setup_storage();
    let repo_root = dir.path().to_path_buf();

    let entity = "drift_count.rs";
    let entity_path = repo_root.join(entity);
    fs::write(&entity_path, "").unwrap();

    {
        let mut drift_mgr = DriftManager::new(
            storage.get_connection_mut(),
            repo_root.clone(),
            Config::default(),
        );
        drift_mgr.process_event(entity).unwrap();
        drift_mgr.process_event(entity).unwrap();
    }

    let tx_mgr = TransactionManager::new(&mut storage, repo_root, Config::default());
    let unaudited = tx_mgr.get_all_unaudited().unwrap();

    assert_eq!(unaudited.len(), 1);
    assert_eq!(unaudited[0].drift_count, 2);
}

#[test]
fn test_drift_detection_ignores_pending() {
    let (dir, mut storage) = setup_storage();
    let repo_root = dir.path().to_path_buf();

    let entity = "tracked.rs";
    let entity_path = repo_root.join(entity);
    fs::write(&entity_path, "").unwrap();

    {
        let mut tx_mgr =
            TransactionManager::new(&mut storage, repo_root.clone(), Config::default());
        tx_mgr
            .start_change(TransactionRequest {
                entity: entity.to_string(),
                category: Category::Feature,
                ..Default::default()
            })
            .expect("Should start tracking");
    }

    {
        let mut drift_mgr = DriftManager::new(
            storage.get_connection_mut(),
            repo_root.clone(),
            Config::default(),
        );
        drift_mgr
            .process_event(entity)
            .expect("Should process tracked file without drift");
    }

    let tx_mgr = TransactionManager::new(&mut storage, repo_root, Config::default());
    let unaudited = tx_mgr.get_all_unaudited().expect("Should get unaudited");
    assert_eq!(
        unaudited.len(),
        0,
        "Should not create unaudited record for tracked file"
    );
}

#[test]
fn test_reconcile_drift() {
    let (dir, mut storage) = setup_storage();
    let repo_root = dir.path().to_path_buf();

    let entity = "reconcile_test.rs";
    fs::write(repo_root.join(entity), "").unwrap();

    {
        let mut drift_mgr = DriftManager::new(
            storage.get_connection_mut(),
            repo_root.clone(),
            Config::default(),
        );
        drift_mgr.process_event(entity).unwrap();
    }

    {
        let mut tx_mgr =
            TransactionManager::new(&mut storage, repo_root.clone(), Config::default());
        let unaudited = tx_mgr.get_all_unaudited().unwrap();
        let tx_id = unaudited[0].tx_id.clone();

        tx_mgr
            .reconcile_drift(Some(tx_id), None, false, "intentional change".to_string())
            .unwrap();
    }

    let tx_mgr = TransactionManager::new(&mut storage, repo_root, Config::default());
    let unaudited = tx_mgr.get_all_unaudited().unwrap();
    assert_eq!(unaudited.len(), 0);

    let entries = tx_mgr.get_ledger_entries(entity).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].entry_type, EntryType::Reconciliation);
    assert_eq!(entries[0].reason, "intentional change");
}

#[test]
fn test_adopt_drift() {
    let (dir, mut storage) = setup_storage();
    let repo_root = dir.path().to_path_buf();

    let entity = "adopt_test.rs";
    fs::write(repo_root.join(entity), "").unwrap();

    {
        let mut drift_mgr = DriftManager::new(
            storage.get_connection_mut(),
            repo_root.clone(),
            Config::default(),
        );
        drift_mgr.process_event(entity).unwrap();
    }

    {
        let mut tx_mgr =
            TransactionManager::new(&mut storage, repo_root.clone(), Config::default());
        let unaudited = tx_mgr.get_all_unaudited().unwrap();
        let tx_id = unaudited[0].tx_id.clone();

        tx_mgr.adopt_drift(Some(tx_id), None, false, None).unwrap();
    }

    let tx_mgr = TransactionManager::new(&mut storage, repo_root, Config::default());
    let unaudited = tx_mgr.get_all_unaudited().unwrap();
    assert_eq!(unaudited.len(), 0);

    let pending = tx_mgr.get_all_pending().unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].entity, entity);
    assert_eq!(pending[0].status, "PENDING");
}

#[test]
fn test_bulk_reconcile_by_pattern() {
    let (dir, mut storage) = setup_storage();
    let repo_root = dir.path().to_path_buf();

    fs::create_dir_all(repo_root.join("src")).unwrap();
    fs::create_dir_all(repo_root.join("docs")).unwrap();
    fs::write(repo_root.join("src/a.rs"), "").unwrap();
    fs::write(repo_root.join("src/b.rs"), "").unwrap();
    fs::write(repo_root.join("docs/readme.md"), "").unwrap();

    {
        let mut drift_mgr = DriftManager::new(
            storage.get_connection_mut(),
            repo_root.clone(),
            Config::default(),
        );
        drift_mgr.process_event("src/a.rs").unwrap();
        drift_mgr.process_event("src/b.rs").unwrap();
        drift_mgr.process_event("docs/readme.md").unwrap();
    }

    {
        let mut tx_mgr =
            TransactionManager::new(&mut storage, repo_root.clone(), Config::default());
        tx_mgr
            .reconcile_drift(
                None,
                Some("src/*.rs".to_string()),
                false,
                "bulk reconcile".to_string(),
            )
            .unwrap();
    }

    let tx_mgr = TransactionManager::new(&mut storage, repo_root, Config::default());
    let unaudited = tx_mgr.get_all_unaudited().unwrap();
    assert_eq!(unaudited.len(), 1);
    assert_eq!(unaudited[0].entity, "docs/readme.md");
}

#[test]
fn test_auto_reconcile_on_commit() {
    let (dir, mut storage) = setup_storage();
    let repo_root = dir.path().to_path_buf();

    let entity = "auto_reconcile.rs";
    fs::write(repo_root.join(entity), "").unwrap();

    {
        let mut drift_mgr = DriftManager::new(
            storage.get_connection_mut(),
            repo_root.clone(),
            Config::default(),
        );
        drift_mgr.process_event(entity).unwrap();
    }

    {
        let mut tx_mgr =
            TransactionManager::new(&mut storage, repo_root.clone(), Config::default());
        let tx_id = tx_mgr
            .start_change(TransactionRequest {
                entity: entity.to_string(),
                ..Default::default()
            })
            .unwrap();

        // This is what execute_ledger_commit does when auto_reconcile is true
        tx_mgr
            .auto_reconcile_entity(entity, "auto".to_string())
            .unwrap();

        tx_mgr
            .commit_change(
                tx_id,
                CommitRequest {
                    summary: "commit with auto-reconcile".to_string(),
                    ..Default::default()
                },
                false,
            )
            .unwrap();
    }

    let tx_mgr = TransactionManager::new(&mut storage, repo_root, Config::default());
    let unaudited = tx_mgr.get_all_unaudited().unwrap();
    assert_eq!(unaudited.len(), 0);

    let entries = tx_mgr.get_ledger_entries(entity).unwrap();
    // One for reconciliation, one for implementation
    assert_eq!(entries.len(), 2);
    assert!(
        entries
            .iter()
            .any(|e| e.entry_type == EntryType::Reconciliation)
    );
    assert!(
        entries
            .iter()
            .any(|e| e.entry_type == EntryType::Implementation)
    );
}
