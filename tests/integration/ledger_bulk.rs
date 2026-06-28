use ledgerful::config::model::Config;
use ledgerful::ledger::*;
use ledgerful::state::storage::StorageManager;
use tempfile::tempdir;

#[test]
fn test_reconcile_drift_bulk_concurrency() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("ledger.db");
    let mut storage = StorageManager::init(&db_path).unwrap();
    let repo_root = dir.path().to_path_buf();

    // 1. Manually insert some UNAUDITED transactions into the DB
    {
        let db = LedgerDb::new(storage.get_connection());
        let tx1 = Transaction {
            tx_id: "tx1".to_string(),
            operation_id: None,
            status: "UNAUDITED".to_string(),
            category: Category::Feature,
            entity: "file1.rs".to_string(),
            entity_normalized: "file1.rs".to_string(),
            planned_action: None,
            session_id: "session1".to_string(),
            source: "WATCHER".to_string(),
            started_at: "2023-01-01T00:00:00Z".to_string(),
            resolved_at: None,
            detected_at: None,
            drift_count: 1,
            first_seen_at: Some("2023-01-01T00:00:00Z".to_string()),
            last_seen_at: Some("2023-01-01T00:00:00Z".to_string()),
            issue_ref: None,
        };
        db.insert_transaction(&tx1).unwrap();
    }

    let mut tx_mgr = TransactionManager::new(&mut storage, repo_root.clone(), Config::default());

    // 2. Call reconcile_drift. This should succeed.
    tx_mgr
        .reconcile_drift(
            Some("tx1".to_string()),
            None,
            false,
            "First reconciliation".to_string(),
        )
        .unwrap();

    // 3. Verify it is RECONCILED
    {
        let db = LedgerDb::new(tx_mgr.get_connection());
        let tx = db.get_transaction("tx1").unwrap().unwrap();
        assert_eq!(tx.status, "RECONCILED");
    }

    // 4. Manually change it back to something else NOT UNAUDITED
    tx_mgr
        .get_connection()
        .execute(
            "UPDATE transactions SET status = 'PENDING' WHERE tx_id = 'tx1'",
            [],
        )
        .unwrap();

    // 5. Try to reconcile it again using --all.
    // This will call get_all_unaudited() which returns empty list, so it succeeds with Ok(()).
    // But we want it to fail if we THINK there's something to reconcile but it's already gone.
    // Wait, if get_all_unaudited() returns empty, then to_reconcile is empty, and it returns Ok(()).
    // This is actually "correct" in a way (nothing to do), but the goal is to prevent
    // updating something that CHANGED between the time we decided to update it and the actual update.

    // Let's simulate a real race:
    // 1. Get list of UNAUDITED txs.
    // 2. Another process updates one of them to PENDING.
    // 3. This process tries to bulk update them to RECONCILED.

    // We can't easily simulate that with TransactionManager's high level API without more work.
    // But I can test the underlying LedgerDb::update_transaction_status_bulk.
    let db = LedgerDb::new(tx_mgr.get_connection());
    let tx_ids = vec!["tx1".to_string()];
    // This should currently succeed but return count 0 if we added the status check.
    let count = db
        .update_transaction_status_bulk(&tx_ids, "RECONCILED", "UNAUDITED", Some("now"))
        .unwrap();
    assert_eq!(
        count, 0,
        "Should have updated 0 rows because status was PENDING, not UNAUDITED"
    );
}

#[test]
fn test_adopt_drift_bulk_concurrency() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("ledger.db");
    let mut storage = StorageManager::init(&db_path).unwrap();
    let repo_root = dir.path().to_path_buf();

    let db = LedgerDb::new(storage.get_connection());
    let tx2 = Transaction {
        tx_id: "tx2".to_string(),
        operation_id: None,
        status: "UNAUDITED".to_string(),
        category: Category::Feature,
        entity: "file2.rs".to_string(),
        entity_normalized: "file2.rs".to_string(),
        planned_action: None,
        session_id: "session1".to_string(),
        source: "WATCHER".to_string(),
        started_at: "2023-01-01T00:00:00Z".to_string(),
        resolved_at: None,
        detected_at: None,
        drift_count: 1,
        first_seen_at: Some("2023-01-01T00:00:00Z".to_string()),
        last_seen_at: Some("2023-01-01T00:00:00Z".to_string()),
        issue_ref: None,
    };
    db.insert_transaction(&tx2).unwrap();

    let mut tx_mgr = TransactionManager::new(&mut storage, repo_root.clone(), Config::default());

    // Adopt it
    tx_mgr
        .adopt_drift(Some("tx2".to_string()), None, false, None)
        .unwrap();

    {
        let db = LedgerDb::new(tx_mgr.get_connection());
        let tx = db.get_transaction("tx2").unwrap().unwrap();
        assert_eq!(tx.status, "PENDING");
    }

    // Try to adopt again (it's already PENDING, not UNAUDITED)
    let result = tx_mgr.adopt_drift(Some("tx2".to_string()), None, false, None);

    assert!(result.is_err(), "Should fail to adopt if not UNAUDITED");
}
