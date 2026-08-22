use crate::common::{git_add_and_commit, run_cli, setup_git_repo};
use ledgerful::commands::init::execute_init;
use serde_json::Value;
use serial_test::serial;
use std::fs;
use std::path::Path;
use tempfile::tempdir;

fn init_repo(root: &Path) {
    setup_git_repo(root);
    fs::write(root.join("dummy.txt"), "content").unwrap();
    git_add_and_commit(root, "initial");
    let _guard = crate::common::DirGuard::new(root);
    execute_init(false, false).unwrap();
}

#[test]
#[serial(cwd)]
fn directory_flag_binds_other_repo_work_root() {
    let a = tempdir().unwrap();
    let b = tempdir().unwrap();
    init_repo(a.path());
    init_repo(b.path());

    let parent_cwd = std::env::current_dir().unwrap();
    let b_str = b.path().to_str().expect("utf-8 temp path");
    let (out, err, code) = run_cli(a.path(), &["-C", b_str, "ledger", "status", "--json"]);
    assert_eq!(code, 0, " -C other repo should succeed: {err}");
    let v: Value = serde_json::from_str(out.trim()).expect("status json");
    assert_eq!(v["schemaVersion"], 1);
    let work_root = v["workRoot"].as_str().expect("workRoot");
    let state_dir = v["stateDir"].as_str().expect("stateDir");
    let b_canon = fs::canonicalize(b.path()).unwrap();
    let reported = Path::new(work_root);
    let reported_canon = fs::canonicalize(reported).unwrap_or_else(|_| reported.to_path_buf());
    assert_eq!(
        reported_canon, b_canon,
        "workRoot must bind -C repo, not spawn cwd; workRoot={work_root}"
    );
    assert!(
        state_dir.ends_with(".ledgerful") || state_dir.replace('\\', "/").ends_with(".ledgerful"),
        "stateDir must end with .ledgerful; got {state_dir}"
    );
    assert_eq!(
        std::env::current_dir().unwrap(),
        parent_cwd,
        "parent process cwd must be unchanged after -C child"
    );
}

#[test]
#[serial(cwd)]
fn directory_flag_missing_path_fails_closed() {
    let tmp = tempdir().unwrap();
    init_repo(tmp.path());
    let missing = tmp.path().join("does-not-exist-0200");
    let missing_str = missing.to_str().expect("utf-8");
    let parent_cwd = std::env::current_dir().unwrap();
    let (_out, err, code) = run_cli(
        tmp.path(),
        &["-C", missing_str, "ledger", "status", "--json"],
    );
    assert_ne!(code, 0, "missing -C path must fail closed; stderr={err}");
    assert_eq!(
        std::env::current_dir().unwrap(),
        parent_cwd,
        "failed -C must not change parent cwd"
    );
}

#[test]
#[serial(cwd)]
fn top_level_status_compact_matches_ledger_status_compact() {
    let tmp = tempdir().unwrap();
    init_repo(tmp.path());

    let (top_out, top_err, top_code) = run_cli(tmp.path(), &["status", "--compact"]);
    let (led_out, led_err, led_code) = run_cli(tmp.path(), &["ledger", "status", "--compact"]);
    assert_eq!(top_code, 0, "status --compact failed: {top_err}");
    assert_eq!(led_code, 0, "ledger status --compact failed: {led_err}");
    assert_eq!(
        top_out.trim(),
        led_out.trim(),
        "top-level status --compact must match ledger status --compact"
    );
    assert!(
        top_out.contains("Ledger [") && top_out.contains("]:"),
        "compact must name workRoot as Ledger [<path>]: …; got {top_out:?}"
    );
    assert!(
        top_out.contains("pending") && top_out.contains("unaudited drift"),
        "compact must keep pending/drift counts; got {top_out:?}"
    );
}

#[test]
#[serial(cwd)]
fn status_json_names_work_root_and_state_dir() {
    let tmp = tempdir().unwrap();
    init_repo(tmp.path());

    let (out, err, code) = run_cli(tmp.path(), &["ledger", "status", "--json"]);
    assert_eq!(code, 0, "ledger status --json failed: {err}");
    let v: Value = serde_json::from_str(out.trim()).expect("status json");
    assert_eq!(v["schemaVersion"], 1);
    let work_root = v["workRoot"].as_str().expect("workRoot present");
    let state_dir = v["stateDir"].as_str().expect("stateDir present");
    let tmp_canon = fs::canonicalize(tmp.path()).unwrap();
    let reported =
        fs::canonicalize(work_root).unwrap_or_else(|_| Path::new(work_root).to_path_buf());
    assert_eq!(
        reported, tmp_canon,
        "workRoot must be the fixture worktree; workRoot={work_root}"
    );
    assert!(
        state_dir.replace('\\', "/").ends_with(".ledgerful"),
        "stateDir must end with .ledgerful; got {state_dir}"
    );
}
