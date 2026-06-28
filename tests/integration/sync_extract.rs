use ledgerful::config::model::Config;
use ledgerful::ledger::*;
use ledgerful::state::storage::StorageManager;
use ledgerful::sync::extract::extract;
use std::fs;
use tempfile::tempdir;

#[test]
fn test_extract_picks_up_new_committed_entries() {
    let dir = tempdir().unwrap();
    let repo_root = dir.path().to_path_buf();

    // Create .ledgerful/state directory
    let state_dir = repo_root.join(".ledgerful").join("state");
    fs::create_dir_all(&state_dir).unwrap();

    let db_path = state_dir.join("ledger.db");
    let mut storage = StorageManager::init(&db_path).unwrap();

    // Create a file for the ledger
    let entity_path = repo_root.join("src/lib.rs");
    fs::create_dir_all(entity_path.parent().unwrap()).unwrap();
    fs::write(&entity_path, "").unwrap();

    let mut tx_mgr = TransactionManager::new(&mut storage, repo_root.clone(), Config::default());

    // Create 5 committed entries
    for i in 0..5 {
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

    // For now, extract might fail because it's todo!()
    // but this establishes the red test.
    // We need to pass the repo_root or state_dir.
    // The plan says state_dir.
    let mut csprng = rand::rngs::OsRng;
    let sign_key = ed25519_dalek::SigningKey::generate(&mut csprng);
    let device_id = "test-device";

    let result = extract(&repo_root.join(".ledgerful"), device_id, &sign_key, 100);

    // This will currently panic with todo!()
    match result {
        Ok(bundle) => {
            assert_eq!(bundle.manifest.entries.len(), 5);
        }
        Err(e) => {
            panic!("Extract failed: {:?}", e);
        }
    }
}
