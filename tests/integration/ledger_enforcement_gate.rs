use ledgerful::config::model::Config;
use ledgerful::ledger::transaction::TransactionManager;
use ledgerful::ledger::types::{
    Category, ChangeType, CommitRequest, TransactionRequest, VerificationStatus,
};
use ledgerful::state::storage::StorageManager;
use std::path::PathBuf;
use tempfile::tempdir;

#[test]
fn test_verification_gate_rejects_missing_basis() {
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
            category: Category::Feature,
            entity: "test".to_string(),
            ..Default::default()
        })
        .unwrap();

    let res = tx_mgr.commit_change(
        tx_id,
        CommitRequest {
            change_type: ChangeType::Modify,
            summary: "test".to_string(),
            reason: "test".to_string(),
            verification_status: Some(VerificationStatus::Verified),
            verification_basis: None,
            ..Default::default()
        },
        false,
    );

    assert!(
        res.is_err(),
        "Commit should be rejected if verification_basis is missing"
    );
    match res.unwrap_err() {
        ledgerful::ledger::error::LedgerError::VerificationRequired(_) => {}
        e => panic!("Unexpected error: {:?}", e),
    }
}
