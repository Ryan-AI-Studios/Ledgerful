//! Track 0114: `ledgerful change-context` CLI integration.

use crate::common::{DirGuard, git_add_and_commit, run_cli, setup_git_repo};
use ledgerful::config::model::Config;
use ledgerful::ledger::{Category, TransactionManager, TransactionRequest};
use ledgerful::state::layout::Layout;
use ledgerful::state::storage::StorageManager;
use std::fs;
use tempfile::tempdir;

#[test]
fn change_context_json_is_single_object_empty_tree() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    fs::write(root.join("README.md"), "hi").unwrap();
    git_add_and_commit(root, "init");

    // Minimal layout so storage opens.
    let layout = Layout::new(root.to_string_lossy().as_ref());
    layout.ensure_state_dir().unwrap();
    let storage =
        StorageManager::init(layout.state_subdir().join("ledger.db").as_std_path()).unwrap();
    storage.shutdown().unwrap();

    let _guard = DirGuard::new(root);
    let (stdout, stderr, code) = run_cli(root, &["change-context", "--json"]);
    assert_eq!(code, 0, "stderr={stderr}");
    let json: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("stdout must be pure JSON");
    assert_eq!(json["schemaVersion"], 1);
    assert_eq!(json["status"], "empty");
    assert!(json.get("doctor").is_some());
    assert!(json.get("ledger").is_some());
    assert_eq!(json["ledger"]["pendingCount"], 0);
    assert_eq!(json["readSetCapped"], false);
    // No cli_summary prefix: first non-whitespace is '{'
    assert!(stdout.trim_start().starts_with('{'));
}

#[test]
fn change_context_pending_ledger_not_silent_empty() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    fs::write(root.join("README.md"), "hi").unwrap();
    git_add_and_commit(root, "init");

    let layout = Layout::new(root.to_string_lossy().as_ref());
    layout.ensure_state_dir().unwrap();
    let mut storage =
        StorageManager::init(layout.state_subdir().join("ledger.db").as_std_path()).unwrap();
    {
        let mut mgr = TransactionManager::new(&mut storage, root.to_path_buf(), Config::default());
        mgr.start_change(TransactionRequest {
            category: Category::Feature,
            entity: "config".to_string(),
            ..Default::default()
        })
        .unwrap();
    }
    storage.shutdown().unwrap();

    let _guard = DirGuard::new(root);
    let (stdout, stderr, code) = run_cli(root, &["change-context", "--json"]);
    assert_eq!(code, 0, "stderr={stderr}");
    let json: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(json["status"], "ready");
    assert!(
        json["ledger"]["pendingCount"].as_u64().unwrap_or(0) >= 1,
        "pending ledger must be visible: {json}"
    );
    let summary = json["summary"].as_str().unwrap_or("");
    assert!(
        summary.to_lowercase().contains("pending"),
        "summary should mention pending: {summary}"
    );
}

#[test]
fn change_context_one_changed_file_ready() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src/lib.rs"), "pub fn a() {}\n").unwrap();
    git_add_and_commit(root, "init");
    fs::write(root.join("src/lib.rs"), "pub fn a() {}\npub fn b() {}\n").unwrap();

    let layout = Layout::new(root.to_string_lossy().as_ref());
    layout.ensure_state_dir().unwrap();
    let storage =
        StorageManager::init(layout.state_subdir().join("ledger.db").as_std_path()).unwrap();
    storage.shutdown().unwrap();

    let _guard = DirGuard::new(root);
    let (stdout, stderr, code) = run_cli(root, &["change-context", "--json"]);
    assert_eq!(code, 0, "stderr={stderr}");
    let json: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(json["status"], "ready");
    let read_set = json["readSet"].as_array().unwrap();
    assert!(
        read_set.iter().any(|e| {
            e["path"].as_str().unwrap_or("").contains("lib.rs")
                && e["reason"].as_str() == Some("changed")
        }),
        "readSet should include lib.rs: {read_set:?}"
    );
    assert!(json.get("riskLevel").is_some());
}

#[test]
fn change_context_max_files_capped() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    fs::create_dir_all(root.join("src")).unwrap();
    for name in ["a.rs", "b.rs", "c.rs"] {
        fs::write(root.join("src").join(name), "pub fn x() {}\n").unwrap();
    }
    git_add_and_commit(root, "init");
    for name in ["a.rs", "b.rs", "c.rs"] {
        fs::write(
            root.join("src").join(name),
            "pub fn x() {}\npub fn y() {}\n",
        )
        .unwrap();
    }

    let layout = Layout::new(root.to_string_lossy().as_ref());
    layout.ensure_state_dir().unwrap();
    let storage =
        StorageManager::init(layout.state_subdir().join("ledger.db").as_std_path()).unwrap();
    storage.shutdown().unwrap();

    let _guard = DirGuard::new(root);
    let (stdout, stderr, code) = run_cli(root, &["change-context", "--json", "--max-files", "1"]);
    assert_eq!(code, 0, "stderr={stderr}");
    let json: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    let read_set = json["readSet"].as_array().unwrap();
    assert!(read_set.len() <= 1);
    assert_eq!(json["readSetCapped"], true);
    assert!(
        json["readSetTotalCandidates"].as_u64().unwrap_or(0) > 1,
        "candidates should exceed cap: {json}"
    );
}

#[test]
fn change_context_base_ref_accepted() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    fs::write(root.join("a.txt"), "1").unwrap();
    git_add_and_commit(root, "first");
    fs::write(root.join("b.txt"), "2").unwrap();
    git_add_and_commit(root, "second");

    let layout = Layout::new(root.to_string_lossy().as_ref());
    layout.ensure_state_dir().unwrap();
    let storage =
        StorageManager::init(layout.state_subdir().join("ledger.db").as_std_path()).unwrap();
    storage.shutdown().unwrap();

    let _guard = DirGuard::new(root);
    // Working tree is clean at HEAD; --base-ref HEAD~1 must time-travel structure
    // so the file added in the second commit appears in the structural set.
    let (stdout, stderr, code) =
        run_cli(root, &["change-context", "--json", "--base-ref", "HEAD~1"]);
    assert_eq!(code, 0, "stderr={stderr}");
    let json: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(json["schemaVersion"], 1);
    assert_eq!(json["baseRef"], "HEAD~1");
    // doctor + ledger always present (present-tense)
    assert!(json.get("doctor").is_some());
    assert!(json.get("ledger").is_some());
    assert_eq!(
        json["status"], "ready",
        "structure vs HEAD~1 should be non-empty: {json}"
    );
    let read_set = json["readSet"].as_array().unwrap();
    assert!(
        read_set
            .iter()
            .any(|e| e["path"].as_str().unwrap_or("").contains("b.txt")),
        "base-ref structure must include b.txt (added after HEAD~1): {read_set:?}"
    );
}

#[test]
fn change_context_human_mode_nonempty() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    fs::write(root.join("README.md"), "hi").unwrap();
    git_add_and_commit(root, "init");

    let layout = Layout::new(root.to_string_lossy().as_ref());
    layout.ensure_state_dir().unwrap();
    let storage =
        StorageManager::init(layout.state_subdir().join("ledger.db").as_std_path()).unwrap();
    storage.shutdown().unwrap();

    let _guard = DirGuard::new(root);
    let (stdout, stderr, code) = run_cli(root, &["change-context"]);
    assert_eq!(code, 0, "stderr={stderr}");
    assert!(!stdout.trim().is_empty());
    assert!(
        stdout.contains("status") || stdout.contains("change-context"),
        "human output: {stdout}"
    );
}
