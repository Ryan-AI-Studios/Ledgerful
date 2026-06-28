use ledgerful::config::model::Config;
use ledgerful::ledger::*;
use ledgerful::state::storage::StorageManager;
use std::fs;
use tempfile::tempdir;

#[test]
fn test_adr_lifecycle_status_and_owner() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("ledger.db");
    let repo_root = dir.path().to_path_buf();

    fs::create_dir_all(repo_root.join("docs")).unwrap();
    fs::write(repo_root.join("docs/arch.md"), "").unwrap();

    let mut storage = StorageManager::init(&db_path).unwrap();
    let mut manager = TransactionManager::new(&mut storage, repo_root.clone(), Config::default());

    // 1. Create an ADR
    let tx_id = manager
        .start_change(TransactionRequest {
            category: Category::Architecture,
            entity: "docs/arch.md".to_string(),
            ..Default::default()
        })
        .unwrap();

    manager
        .commit_change(
            tx_id.clone(),
            CommitRequest {
                summary: "Use CozoDB for Knowledge Graph".to_string(),
                reason: "Need Datalog for reachability".to_string(),
                ..Default::default()
            },
            false,
        )
        .unwrap();

    // 2. Update status and owner (Proposed API)
    manager
        .update_adr_metadata(
            &tx_id,
            AdrMetadataUpdate {
                status: Some(AdrStatus::Accepted),
                owner: Some("alice".to_string()),
                ..Default::default()
            },
        )
        .unwrap();

    // 3. Verify metadata
    let metadata = manager.get_adr_metadata(&tx_id).unwrap();
    assert_eq!(metadata.status, AdrStatus::Accepted);
    assert_eq!(metadata.owner.as_deref(), Some("alice"));
}

#[test]
fn test_adr_lifecycle_supersession() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("ledger.db");
    let repo_root = dir.path().to_path_buf();

    fs::create_dir_all(repo_root.join("docs")).unwrap();
    fs::write(repo_root.join("docs/adr1.md"), "").unwrap();
    fs::write(repo_root.join("docs/adr2.md"), "").unwrap();

    let mut storage = StorageManager::init(&db_path).unwrap();
    let mut manager = TransactionManager::new(&mut storage, repo_root.clone(), Config::default());

    // 1. Create ADR 1
    let adr1_id = manager
        .start_change(TransactionRequest {
            category: Category::Architecture,
            entity: "docs/adr1.md".to_string(),
            ..Default::default()
        })
        .unwrap();
    manager
        .commit_change(
            adr1_id.clone(),
            CommitRequest {
                summary: "Old Decision".to_string(),
                ..Default::default()
            },
            false,
        )
        .unwrap();

    // 2. Create ADR 2
    let adr2_id = manager
        .start_change(TransactionRequest {
            category: Category::Architecture,
            entity: "docs/adr2.md".to_string(),
            ..Default::default()
        })
        .unwrap();
    manager
        .commit_change(
            adr2_id.clone(),
            CommitRequest {
                summary: "New Decision".to_string(),
                ..Default::default()
            },
            false,
        )
        .unwrap();

    // 3. Link ADR 2 to supersede ADR 1 (Proposed API)
    manager.link_adr_supersedes(&adr2_id, &adr1_id).unwrap();

    // 4. Verify
    let meta1 = manager.get_adr_metadata(&adr1_id).unwrap();
    let meta2 = manager.get_adr_metadata(&adr2_id).unwrap();
    assert_eq!(meta1.status, AdrStatus::Superseded);
    assert_eq!(meta1.superseded_by, Some(adr2_id.clone()));
    assert_eq!(meta2.supersedes, Some(adr1_id.clone()));
}
