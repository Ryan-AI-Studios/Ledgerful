use crate::common::{DirGuard, git_add_and_commit, setup_git_repo};
use ledgerful::commands::init::execute_init;
use ledgerful::config::model::Config;
use ledgerful::ledger::{Category, CommitRequest, TransactionManager, TransactionRequest};
use ledgerful::state::storage::StorageManager;
use std::fs;
use std::process::Command;
use tempfile::tempdir;

fn seed_three_commits(root: &std::path::Path) -> [String; 3] {
    setup_git_repo(root);
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src/a.rs"), "fn a() {}").unwrap();
    fs::write(root.join("src/b.rs"), "fn b() {}").unwrap();
    fs::write(root.join("src/c.rs"), "fn c() {}").unwrap();
    git_add_and_commit(root, "initial");

    let _guard = DirGuard::new(root);
    execute_init(false, false).unwrap();

    let db_path = root.join(".ledgerful/state/ledger.db");
    let mut storage = StorageManager::init(&db_path).unwrap();
    let mut manager = TransactionManager::new(&mut storage, root.to_path_buf(), Config::default());

    let mut ids = Vec::new();
    for (entity, summary) in [
        ("src/a.rs", "first oldest"),
        ("src/b.rs", "second"),
        ("src/c.rs", "third newest"),
    ] {
        let tx = manager
            .start_change(TransactionRequest {
                category: Category::Feature,
                entity: entity.to_string(),
                ..Default::default()
            })
            .unwrap();
        manager
            .commit_change(
                tx.clone(),
                CommitRequest {
                    summary: summary.to_string(),
                    reason: "provenance paging fixture".to_string(),
                    ..Default::default()
                },
                false,
            )
            .unwrap();
        ids.push(tx);
    }
    [ids[0].clone(), ids[1].clone(), ids[2].clone()]
}

#[test]
fn export_provenance_limit_is_bare_array() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    let _ids = seed_three_commits(root);

    let output = Command::new(env!("CARGO_BIN_EXE_ledgerful"))
        .args([
            "ledger",
            "export-provenance",
            "--limit",
            "2",
            "--offset",
            "0",
        ])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{:?}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(value.is_array(), "stdout must stay a bare array: {value}");
    assert_eq!(value.as_array().unwrap().len(), 2);
    assert!(value.as_array().unwrap()[0].get("schemaVersion").is_none());
}

#[test]
fn export_provenance_truncated_stderr() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    let _ids = seed_three_commits(root);

    let output = Command::new(env!("CARGO_BIN_EXE_ledgerful"))
        .args(["ledger", "export-provenance", "--limit", "2"])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("truncated:"),
        "expected truncated line, got {stderr}"
    );
}

#[test]
fn export_provenance_page_is_oldest_first() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    let ids = seed_three_commits(root);

    let full = Command::new(env!("CARGO_BIN_EXE_ledgerful"))
        .args(["ledger", "export-provenance"])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        full.status.success(),
        "{:?}",
        String::from_utf8_lossy(&full.stderr)
    );
    let full_json: serde_json::Value = serde_json::from_slice(&full.stdout).unwrap();
    let full_rows = full_json.as_array().expect("full array");
    assert!(full_rows.len() >= 3);

    let page = Command::new(env!("CARGO_BIN_EXE_ledgerful"))
        .args([
            "ledger",
            "export-provenance",
            "--limit",
            "2",
            "--offset",
            "0",
        ])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(page.status.success());
    let page_json: serde_json::Value = serde_json::from_slice(&page.stdout).unwrap();
    let page_rows = page_json.as_array().expect("page array");
    assert_eq!(page_rows.len(), 2);
    assert_eq!(page_rows[0]["tx_id"], full_rows[0]["tx_id"]);
    assert_eq!(page_rows[1]["tx_id"], full_rows[1]["tx_id"]);
    assert_eq!(page_rows[0]["committed_at"], full_rows[0]["committed_at"]);
    assert_ne!(page_rows[0]["tx_id"], ids[2]);
}

#[test]
fn export_provenance_limit_zero_refused() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    let _ids = seed_three_commits(root);

    let output = Command::new(env!("CARGO_BIN_EXE_ledgerful"))
        .args(["ledger", "export-provenance", "--limit", "0"])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(!output.status.success(), "limit 0 must refuse");
    assert!(
        output.stdout.is_empty(),
        "stdout must be empty on refuse: {:?}",
        String::from_utf8_lossy(&output.stdout)
    );
}
