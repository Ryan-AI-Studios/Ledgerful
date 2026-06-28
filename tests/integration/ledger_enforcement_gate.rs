use ledgerful::config::model::Config;
use ledgerful::ledger::transaction::TransactionManager;
use ledgerful::ledger::types::{
    Category, ChangeType, CommitRequest, TransactionRequest, VerificationBasis, VerificationStatus,
};
use ledgerful::state::storage::StorageManager;
use std::path::PathBuf;
use tempfile::tempdir;

#[test]
fn test_verification_gate_blocks_high_risk_categories() {
    let tmp = tempdir().unwrap();
    let root = PathBuf::from(tmp.path());
    let storage_path = root.join("ledger.db");
    let mut storage = StorageManager::init(&storage_path).unwrap();

    let mut config = Config::default();
    config.ledger.verify_to_commit = true;

    let mut tx_mgr = TransactionManager::new(&mut storage, root.clone(), config);

    let categories = vec![
        Category::Architecture,
        Category::Feature,
        Category::Bugfix,
        Category::Infra,
    ];

    for cat in categories {
        let tx_id = tx_mgr
            .start_change(TransactionRequest {
                category: cat,
                entity: "test".to_string(),
                ..Default::default()
            })
            .unwrap();

        let res = tx_mgr.commit_change(
            tx_id.clone(),
            CommitRequest {
                change_type: ChangeType::Modify,
                summary: "test".to_string(),
                reason: "test".to_string(),
                ..Default::default()
            },
            false,
        );

        assert!(
            res.is_err(),
            "Category {:?} should be blocked without verification",
            cat
        );
        match res.unwrap_err() {
            ledgerful::ledger::error::LedgerError::VerificationRequired(c) => {
                assert!(c.to_uppercase().contains(&cat.to_string().to_uppercase()));
            }
            e => panic!("Unexpected error: {:?}", e),
        }

        tx_mgr.rollback_change(tx_id, "test".to_string()).unwrap();
    }
}

#[test]
fn test_verification_gate_allows_with_status() {
    let tmp = tempdir().unwrap();
    let root = PathBuf::from(tmp.path());
    let storage_path = root.join("ledger.db");
    let mut storage = StorageManager::init(&storage_path).unwrap();

    let mut config = Config::default();
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
            verification_basis: Some(VerificationBasis::Tests),
            ..Default::default()
        },
        false,
    );

    assert!(
        res.is_ok(),
        "Commit should succeed with verification status and basis: {:?}",
        res.err()
    );
}

#[test]
fn test_verification_gate_rejects_missing_basis() {
    let tmp = tempdir().unwrap();
    let root = PathBuf::from(tmp.path());
    let storage_path = root.join("ledger.db");
    let mut storage = StorageManager::init(&storage_path).unwrap();

    let mut config = Config::default();
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

#[test]
fn test_verification_gate_force_override() {
    let tmp = tempdir().unwrap();
    let root = PathBuf::from(tmp.path());
    let storage_path = root.join("ledger.db");
    let mut storage = StorageManager::init(&storage_path).unwrap();

    let mut config = Config::default();
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
            ..Default::default()
        },
        true,
    );

    assert!(
        res.is_ok(),
        "Commit should succeed with force=true despite missing verification"
    );
}

#[test]
fn test_verification_gate_disabled_by_default() {
    let tmp = tempdir().unwrap();
    let root = PathBuf::from(tmp.path());
    let storage_path = root.join("ledger.db");
    let mut storage = StorageManager::init(&storage_path).unwrap();

    let config = Config::default(); // verify_to_commit = false by default

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
            ..Default::default()
        },
        false,
    );

    assert!(
        res.is_ok(),
        "Commit should succeed when verify_to_commit is false"
    );
}

#[test]
fn test_verification_gate_allows_low_risk_categories() {
    let tmp = tempdir().unwrap();
    let root = PathBuf::from(tmp.path());
    let storage_path = root.join("ledger.db");
    let mut storage = StorageManager::init(&storage_path).unwrap();

    let mut config = Config::default();
    config.ledger.verify_to_commit = true;

    let mut tx_mgr = TransactionManager::new(&mut storage, root.clone(), config);

    let categories = vec![
        Category::Refactor,
        Category::Tooling,
        Category::Docs,
        Category::Chore,
    ];

    for cat in categories {
        let tx_id = tx_mgr
            .start_change(TransactionRequest {
                category: cat,
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
                ..Default::default()
            },
            false,
        );

        assert!(res.is_ok(), "Category {:?} should NOT be blocked", cat);
    }
}
