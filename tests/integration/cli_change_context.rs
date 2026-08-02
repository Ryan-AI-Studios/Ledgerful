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
fn change_context_invalid_base_ref_is_not_ready() {
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
    let (stdout, stderr, code) = run_cli(
        root,
        &[
            "change-context",
            "--json",
            "--base-ref",
            "definitely-not-a-ref-0114",
        ],
    );
    assert_eq!(code, 0, "stderr={stderr}");
    let json: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(
        json["status"], "not_ready",
        "invalid base-ref must yield not_ready: {json}"
    );
    assert!(
        json["reason"].as_str().is_some_and(|r| !r.is_empty())
            || json["summary"]
                .as_str()
                .is_some_and(|s| s.to_lowercase().contains("not ready")),
        "must explain not_ready: {json}"
    );
    assert!(json.get("doctor").is_some());
    assert!(json.get("ledger").is_some());
    let next = json["nextActions"].as_array().cloned().unwrap_or_default();
    assert!(
        !next.is_empty(),
        "not_ready should suggest nextActions: {json}"
    );
}

#[test]
fn change_context_option_like_base_ref_is_not_ready() {
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

    let poison = root.join("poisoned-out.txt");
    // Use --base-ref=VALUE so clap does not re-parse the value as a flag.
    let flag = format!("--base-ref=--output={}", poison.display());
    let _guard = DirGuard::new(root);
    let (stdout, stderr, code) = run_cli(root, &["change-context", "--json", &flag]);
    assert_eq!(code, 0, "stderr={stderr} stdout={stdout}");
    let json: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(
        json["status"], "not_ready",
        "option-like base-ref must not succeed: {json}"
    );
    assert!(
        !poison.exists(),
        "option-like base-ref must not create poison file"
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

/// 0127: pure-add package → changeHints greenfield + suggested tests / notes.
#[test]
fn change_context_greenfield_pure_add_package() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    fs::write(root.join("README.md"), "hi").unwrap();
    git_add_and_commit(root, "init");

    // Dirty pure-add package (untracked → git status Added)
    fs::create_dir_all(root.join("src/newpkg")).unwrap();
    fs::write(root.join("src/newpkg/mod.rs"), "pub fn brand_new() {}\n").unwrap();
    fs::write(
        root.join("src/newpkg/cli.rs"),
        "pub fn run() { brand_new(); }\n",
    )
    .unwrap();
    fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();

    let layout = Layout::new(root.to_string_lossy().as_ref());
    layout.ensure_state_dir().unwrap();
    let storage =
        StorageManager::init(layout.state_subdir().join("ledger.db").as_std_path()).unwrap();
    storage.shutdown().unwrap();

    let _guard = DirGuard::new(root);
    let (stdout, stderr, code) = run_cli(root, &["change-context", "--json"]);
    assert_eq!(code, 0, "stderr={stderr}");
    let json: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(json["schemaVersion"], 1);
    assert_eq!(json["status"], "ready");

    let hints = json
        .get("changeHints")
        .expect("changeHints must be present for pure-add package");
    assert_eq!(
        hints["kind"].as_str(),
        Some("greenfield"),
        "expected greenfield: {hints}"
    );
    let suggested = hints["suggestedTests"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let notes = hints["notes"].as_array().cloned().unwrap_or_default();
    assert!(
        !suggested.is_empty() || !notes.is_empty(),
        "suggestedTests >= 1 or honesty notes: {hints}"
    );
    let summary = json["summary"].as_str().unwrap_or("");
    assert!(
        summary.contains("greenfield-ish"),
        "summary must mention greenfield-ish: {summary}"
    );
    // No conductor / meshops product coupling in packet
    let raw = stdout.to_ascii_lowercase();
    assert!(!raw.contains("meshops"), "no meshops hardcode: {stdout}");
    assert!(!stdout.contains("0127-"), "no track-id coupling: {stdout}");
}

/// 0127: modify-only control must not false-greenfield.
#[test]
fn change_context_modify_only_not_false_greenfield() {
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
    // changeHints may be present (kind none) when files changed
    if let Some(hints) = json.get("changeHints") {
        assert_ne!(
            hints["kind"].as_str(),
            Some("greenfield"),
            "modify-only must not be greenfield: {hints}"
        );
        let summary = json["summary"].as_str().unwrap_or("");
        assert!(
            !summary.contains("greenfield-ish"),
            "summary must not claim greenfield for modify-only: {summary}"
        );
    }
}

/// 0127: empty tree omits changeHints key.
#[test]
fn change_context_empty_omits_change_hints() {
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
    let (stdout, stderr, code) = run_cli(root, &["change-context", "--json"]);
    assert_eq!(code, 0, "stderr={stderr}");
    let json: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(json["status"], "empty");
    assert!(
        json.get("changeHints").is_none(),
        "empty tree must omit changeHints: {json}"
    );
}
