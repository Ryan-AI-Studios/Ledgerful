use crate::common::{DirGuard, git_add_and_commit, setup_git_repo};
use ledgerful::commands::init::execute_init;
use ledgerful::commands::ledger_audit::execute_ledger_audit;
use std::fs;
use tempfile::tempdir;

#[test]
fn audit_returns_entity_list() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();

    setup_git_repo(root);
    fs::write(root.join("dummy.txt"), "content").unwrap();
    git_add_and_commit(root, "initial");

    let _guard = DirGuard::new(root);
    execute_init(false).unwrap();

    // Audit with limit 5, no entity filter, not json, no unaudited
    let result = execute_ledger_audit(None, false, 5, 0, false);
    assert!(result.is_ok());
}

#[test]
fn audit_entity_related_returns_related_entities() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();

    setup_git_repo(root);

    // Create related files
    fs::create_dir_all(root.join("src/cli")).unwrap();
    fs::write(root.join("src/cli/dispatch.rs"), "fn dispatch() {}").unwrap();
    fs::write(root.join("src/cli/helpers.rs"), "fn helper() {}").unwrap();
    git_add_and_commit(root, "initial");

    let _guard = DirGuard::new(root);
    execute_init(false).unwrap();

    let db_path = root.join(".ledgerful/state/ledger.db");
    let mut storage = ledgerful::state::storage::StorageManager::init(&db_path).unwrap();
    let mut manager = ledgerful::ledger::TransactionManager::new(
        &mut storage,
        root.to_path_buf(),
        ledgerful::config::model::Config::default(),
    );

    // Transaction for dispatch.rs
    let tx1 = manager
        .start_change(ledgerful::ledger::TransactionRequest {
            category: ledgerful::ledger::Category::Feature,
            entity: "src/cli/dispatch.rs".to_string(),
            ..Default::default()
        })
        .unwrap();
    manager
        .commit_change(
            tx1,
            ledgerful::ledger::CommitRequest {
                summary: "Dispatch change".to_string(),
                reason: "reason".to_string(),
                ..Default::default()
            },
            false,
        )
        .unwrap();

    // Transaction for helpers.rs
    let tx2 = manager
        .start_change(ledgerful::ledger::TransactionRequest {
            category: ledgerful::ledger::Category::Bugfix,
            entity: "src/cli/helpers.rs".to_string(),
            ..Default::default()
        })
        .unwrap();
    manager
        .commit_change(
            tx2,
            ledgerful::ledger::CommitRequest {
                summary: "Helpers change".to_string(),
                reason: "reason".to_string(),
                ..Default::default()
            },
            false,
        )
        .unwrap();

    drop(manager);
    drop(storage);

    // Audit dispatch.rs
    let result = execute_ledger_audit(Some("src/cli/dispatch.rs".to_string()), false, 5, 0, false);
    assert!(result.is_ok());
}
