use ledgerful::config::model::Config;
use ledgerful::ledger::transaction::TransactionManager;
use ledgerful::ledger::types::{Category, ChangeType, CommitRequest, TransactionRequest};
use ledgerful::state::storage::StorageManager;
use std::path::PathBuf;
use tempfile::tempdir;

#[test]
fn observe_mode_does_not_block_verification_gate() {
    let tmp = tempdir().unwrap();
    let root = PathBuf::from(tmp.path());
    let storage_path = root.join("ledger.db");
    let mut storage = StorageManager::init(&storage_path).unwrap();

    let mut config = Config::default();
    config.gate.mode = "observe".to_string();
    config.ledger.verify_to_commit = true;

    let mut tx_mgr = TransactionManager::new(&mut storage, root.clone(), config);

    let tx_id = tx_mgr
        .start_change(TransactionRequest {
            category: Category::Architecture,
            entity: "test".to_string(),
            ..Default::default()
        })
        .unwrap();

    let res = tx_mgr.commit_change(
        tx_id,
        CommitRequest {
            change_type: ChangeType::Modify,
            summary: "test observe".to_string(),
            reason: "test".to_string(),
            ..Default::default()
        },
        false,
    );

    assert!(
        res.is_ok(),
        "Observe mode should NOT block high-risk categories without verification: {:?}",
        res.err()
    );
}

#[test]
fn enforce_mode_blocks_verification_gate() {
    let tmp = tempdir().unwrap();
    let root = PathBuf::from(tmp.path());
    let storage_path = root.join("ledger.db");
    let mut storage = StorageManager::init(&storage_path).unwrap();

    let mut config = Config::default();
    config.gate.mode = "enforce".to_string();
    config.ledger.verify_to_commit = true;

    let mut tx_mgr = TransactionManager::new(&mut storage, root.clone(), config);

    let tx_id = tx_mgr
        .start_change(TransactionRequest {
            category: Category::Architecture,
            entity: "test".to_string(),
            ..Default::default()
        })
        .unwrap();

    let res = tx_mgr.commit_change(
        tx_id.clone(),
        CommitRequest {
            change_type: ChangeType::Modify,
            summary: "test enforce".to_string(),
            reason: "test".to_string(),
            ..Default::default()
        },
        false,
    );

    assert!(
        res.is_err(),
        "Enforce mode should block without verification"
    );
    tx_mgr.rollback_change(tx_id, "test".to_string()).unwrap();
}

#[test]
fn observe_mode_fidelity_same_records_as_enforce_no_blocks() {
    let tmp1 = tempdir().unwrap();
    let tmp2 = tempdir().unwrap();
    let root1 = PathBuf::from(tmp1.path());
    let root2 = PathBuf::from(tmp2.path());

    let mut storage1 = StorageManager::init(&root1.join("ledger.db")).unwrap();
    let mut storage2 = StorageManager::init(&root2.join("ledger.db")).unwrap();

    let mut config1 = Config::default();
    config1.gate.mode = "observe".to_string();
    let mut config2 = Config::default();
    config2.gate.mode = "enforce".to_string();

    let mut tx1 = TransactionManager::new(&mut storage1, root1, config1);
    let mut tx2 = TransactionManager::new(&mut storage2, root2, config2);

    let summary = "Add feature X".to_string();
    let reason = "Because Y".to_string();

    let id1 = tx1
        .start_change(TransactionRequest {
            category: Category::Feature,
            entity: "src/main.rs".to_string(),
            ..Default::default()
        })
        .unwrap();
    tx1.commit_change(
        id1.clone(),
        CommitRequest {
            change_type: ChangeType::Modify,
            summary: summary.clone(),
            reason: reason.clone(),
            ..Default::default()
        },
        false,
    )
    .unwrap();

    let id2 = tx2
        .start_change(TransactionRequest {
            category: Category::Feature,
            entity: "src/main.rs".to_string(),
            ..Default::default()
        })
        .unwrap();
    tx2.commit_change(
        id2.clone(),
        CommitRequest {
            change_type: ChangeType::Modify,
            summary: summary.clone(),
            reason: reason.clone(),
            ..Default::default()
        },
        false,
    )
    .unwrap();

    let conn1 = storage1.get_connection();
    let conn2 = storage2.get_connection();

    let (s1, r1, c1): (String, String, String) = conn1
        .query_row(
            "SELECT summary, reason, category FROM ledger_entries ORDER BY rowid DESC LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    let (s2, r2, c2): (String, String, String) = conn2
        .query_row(
            "SELECT summary, reason, category FROM ledger_entries ORDER BY rowid DESC LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();

    assert_eq!(s1, s2, "summary must match");
    assert_eq!(r1, r2, "reason must match");
    assert_eq!(c1, c2, "category must match");
}

#[test]
fn observed_marker_set_on_observe_mode_commit() {
    let tmp = tempdir().unwrap();
    let root = PathBuf::from(tmp.path());
    let storage_path = root.join("ledger.db");
    let mut storage = StorageManager::init(&storage_path).unwrap();

    let mut config = Config::default();
    config.gate.mode = "observe".to_string();
    config.ledger.verify_to_commit = true;

    let mut tx_mgr = TransactionManager::new(&mut storage, root.clone(), config);

    let tx_id = tx_mgr
        .start_change(TransactionRequest {
            category: Category::Architecture,
            entity: "test".to_string(),
            ..Default::default()
        })
        .unwrap();

    tx_mgr
        .commit_change(
            tx_id,
            CommitRequest {
                change_type: ChangeType::Modify,
                summary: "test observed".to_string(),
                reason: "test".to_string(),
                ..Default::default()
            },
            false,
        )
        .unwrap();

    let conn = storage.get_connection();
    let observed: Option<i64> = conn
        .query_row(
            "SELECT observed FROM ledger_entries ORDER BY rowid DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .ok();
    assert_eq!(
        observed,
        Some(1),
        "observed marker should be 1 for observe-mode commit with block condition"
    );
}

#[test]
fn observed_marker_not_set_on_enforce_mode_commit() {
    let tmp = tempdir().unwrap();
    let root = PathBuf::from(tmp.path());
    let storage_path = root.join("ledger.db");
    let mut storage = StorageManager::init(&storage_path).unwrap();

    let mut config = Config::default();
    config.gate.mode = "enforce".to_string();

    let mut tx_mgr = TransactionManager::new(&mut storage, root.clone(), config);

    let tx_id = tx_mgr
        .start_change(TransactionRequest {
            category: Category::Feature,
            entity: "test".to_string(),
            ..Default::default()
        })
        .unwrap();

    tx_mgr
        .commit_change(
            tx_id,
            CommitRequest {
                change_type: ChangeType::Modify,
                summary: "test enforce".to_string(),
                reason: "test".to_string(),
                ..Default::default()
            },
            false,
        )
        .unwrap();

    let conn = storage.get_connection();
    let observed: Option<i64> = conn
        .query_row(
            "SELECT observed FROM ledger_entries ORDER BY rowid DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .ok();
    assert_ne!(
        observed,
        Some(1),
        "observed marker should not be 1 for enforce-mode commit"
    );
}

#[test]
fn gate_mode_transition_writes_ledger_entry() {
    use camino::Utf8PathBuf;
    use ledgerful::commands::gate::write_mode_transition_entry;
    use ledgerful::state::layout::Layout;

    let tmp = tempdir().unwrap();
    let root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
    let layout = Layout::new(&root);
    layout.ensure_state_dir().unwrap();

    let db_path = layout.state_subdir().join("ledger.db");
    StorageManager::init(db_path.as_std_path()).unwrap();

    write_mode_transition_entry(&layout, "observe", "enforce").unwrap();

    let storage = StorageManager::open_read_only_sqlite_only(&root).unwrap();
    let conn = storage.get_connection();
    let (summary, entry_type): (String, String) = conn
        .query_row(
            "SELECT summary, entry_type FROM ledger_entries ORDER BY rowid DESC LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert!(
        summary.contains("observe") && summary.contains("enforce"),
        "summary should contain transition: got '{}'",
        summary
    );
    assert_eq!(
        entry_type, "MAINTENANCE",
        "entry_type should be MAINTENANCE: got '{}'",
        entry_type
    );
}
