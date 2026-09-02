//! Track 0224: `ledgerful session` CLI integration.

use crate::common::{DirGuard, git_add_and_commit, run_cli, setup_git_repo};
use ledgerful::config::model::Config;
use ledgerful::impact::packet::ImpactPacket;
use ledgerful::ledger::{Category, TransactionManager, TransactionRequest};
use ledgerful::state::layout::Layout;
use ledgerful::state::reports::{
    LATEST_IMPACT_REPORT, write_clean_tree_tombstone, write_impact_report,
};
use ledgerful::state::storage::StorageManager;
use std::fs;
use tempfile::tempdir;

fn head_sha(dir: &std::path::Path) -> String {
    let out = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(dir)
        .output()
        .expect("rev-parse");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
fn session_json_is_schema_v1_kind_session() {
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
    let (stdout, stderr, code) = run_cli(root, &["session", "--json"]);
    assert_eq!(code, 0, "stderr={stderr}");
    assert!(stdout.trim_start().starts_with('{'));
    let json: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("stdout must be pure JSON");
    assert_eq!(json["schemaVersion"], 1);
    assert_eq!(json["kind"], "session");
    assert!(json.get("git").is_some());
    assert!(json.get("ledger").is_some());
    assert!(json.get("doctor").is_some());
    assert!(json.get("changeContext").is_some());
    assert!(json.get("hotspots").is_some());
    assert!(json.get("impactCache").is_some());
    assert!(json["next"].is_array());
    assert!(json["doctor"].get("warnAction").is_none());
    assert!(json["ledger"]["collisions"].is_array());
    assert_eq!(json["hotspots"]["excludedTests"], true);
    assert!(json["git"]["dirtyCount"].is_number());
}

#[test]
fn session_human_stdout_is_not_json() {
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
    let (stdout, stderr, code) = run_cli(root, &["session"]);
    assert_eq!(code, 0, "stderr={stderr}");
    let text = stdout.trim();
    assert!(
        text.starts_with("Ledgerful session"),
        "human default must be 10-line summary: {text}"
    );
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 10, "human summary must be 10 lines: {text}");
    assert!(
        serde_json::from_str::<serde_json::Value>(text).is_err(),
        "human stdout must fail JSON parse: {text}"
    );
}

#[test]
fn session_does_not_rewrite_latest_impact() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    fs::write(root.join("README.md"), "hi").unwrap();
    git_add_and_commit(root, "init");

    let layout = Layout::new(root.to_string_lossy().as_ref());
    layout.ensure_state_dir().unwrap();
    let seed = ImpactPacket {
        schema_version: "v1".to_string(),
        head_hash: Some("SEED_MARKER_0224_CLI".to_string()),
        risk_reasons: vec!["seed-reason-do-not-clobber".to_string()],
        ..Default::default()
    };
    write_impact_report(&layout, &seed).unwrap();
    let report_path = layout.reports_dir().join(LATEST_IMPACT_REPORT);
    let before = fs::read_to_string(report_path.as_std_path()).unwrap();

    let storage =
        StorageManager::init(layout.state_subdir().join("ledger.db").as_std_path()).unwrap();
    storage.shutdown().unwrap();

    let _guard = DirGuard::new(root);
    let (stdout, stderr, code) = run_cli(root, &["session", "--json"]);
    assert_eq!(code, 0, "stderr={stderr} stdout={stdout}");
    let after = fs::read_to_string(report_path.as_std_path()).unwrap();
    assert_eq!(
        before, after,
        "session --json must not rewrite latest-impact.json"
    );
    let json: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(json["impactCache"]["present"], true);
    assert_eq!(json["impactCache"]["validForHead"], false);
    assert_eq!(json["impactCache"]["treeClean"], false);
}

#[test]
fn session_cleantree_same_head_is_valid() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    fs::write(root.join("README.md"), "hi").unwrap();
    git_add_and_commit(root, "init");
    let live = head_sha(root);

    let layout = Layout::new(root.to_string_lossy().as_ref());
    layout.ensure_state_dir().unwrap();
    write_clean_tree_tombstone(&layout, Some(live), Some("master".to_string())).unwrap();
    let storage =
        StorageManager::init(layout.state_subdir().join("ledger.db").as_std_path()).unwrap();
    storage.shutdown().unwrap();

    let _guard = DirGuard::new(root);
    let (stdout, stderr, code) = run_cli(root, &["session", "--json"]);
    assert_eq!(code, 0, "stderr={stderr}");
    let json: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(json["impactCache"]["present"], true);
    assert_eq!(json["impactCache"]["treeClean"], true);
    assert_eq!(json["impactCache"]["validForHead"], true);
}

#[test]
fn session_pending_and_dirty_includes_collisions() {
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
            entity: "crates/dedupe-chrome".to_string(),
            planned_action: Some("chrome work".to_string()),
            ..Default::default()
        })
        .unwrap();
    }
    storage.shutdown().unwrap();

    fs::create_dir_all(root.join("crates").join("dedupe-chrome")).unwrap();
    fs::write(
        root.join("crates").join("dedupe-chrome").join("foo.rs"),
        "fn x() {}\n",
    )
    .unwrap();

    let _guard = DirGuard::new(root);
    let (stdout, stderr, code) = run_cli(root, &["session", "--json"]);
    assert_eq!(code, 0, "stderr={stderr}");
    let json: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert!(
        json["ledger"]["pendingCount"].as_u64().unwrap_or(0) >= 1,
        "pending: {json}"
    );
    assert!(
        json["git"]["dirtyCount"].as_u64().unwrap_or(0) >= 1,
        "dirty: {json}"
    );
    let collisions = json["ledger"]["collisions"]
        .as_array()
        .expect("collisions[]");
    assert!(
        !collisions.is_empty(),
        "pending chrome + dirty chrome must collide: {json}"
    );
    assert_eq!(json["changeContext"]["status"], "ready");
}

#[test]
fn session_read_set_passthrough_max_files_five() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    fs::create_dir_all(root.join("src")).unwrap();
    for name in ["a.rs", "b.rs", "c.rs", "d.rs", "e.rs", "f.rs"] {
        fs::write(root.join("src").join(name), "pub fn x() {}\n").unwrap();
    }
    git_add_and_commit(root, "init");
    for name in ["a.rs", "b.rs", "c.rs", "d.rs", "e.rs", "f.rs"] {
        fs::write(root.join("src").join(name), "pub fn y() {}\n").unwrap();
    }

    let layout = Layout::new(root.to_string_lossy().as_ref());
    layout.ensure_state_dir().unwrap();
    let storage =
        StorageManager::init(layout.state_subdir().join("ledger.db").as_std_path()).unwrap();
    storage.shutdown().unwrap();

    let _guard = DirGuard::new(root);
    let (stdout, stderr, code) = run_cli(root, &["session", "--json"]);
    assert_eq!(code, 0, "stderr={stderr}");
    let json: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    let read_set = json["changeContext"]["readSet"].as_array().unwrap();
    assert!(
        read_set.len() <= 5,
        "readSet must be built with max_files=5: {read_set:?}"
    );
    let candidates = json["changeContext"]["readSetTotalCandidates"]
        .as_u64()
        .unwrap_or(0);
    if candidates > 5 {
        assert_eq!(
            json["changeContext"]["readSetCapped"], true,
            "passthrough capped must be honest: {json}"
        );
    }
}
