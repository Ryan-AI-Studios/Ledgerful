use camino::Utf8Path;
use ledgerful::commands::init::execute_init;
use ledgerful::commands::reset::execute_reset;
use std::fs;
use tempfile::tempdir;

use crate::common::{DirGuard, setup_git_repo};

fn setup_repo() -> (tempfile::TempDir, camino::Utf8PathBuf, DirGuard) {
    let tmp = tempdir().unwrap();
    let root = Utf8Path::from_path(tmp.path()).unwrap().to_path_buf();
    setup_git_repo(tmp.path());
    let guard = DirGuard::from_utf8(&root);
    (tmp, root, guard)
}

#[test]
fn test_reset_missing_state_is_safe() {
    let (_tmp, root, _guard) = setup_repo();

    execute_reset(false, false, false, false, false, false).unwrap();

    assert!(!root.join(".ledgerful").exists());
}

#[test]
fn test_reset_preserves_config_and_rules_by_default() {
    let (_tmp, root, _guard) = setup_repo();
    execute_init(false).unwrap();

    let state_dir = root.join(".ledgerful");
    let logs_dir = state_dir.join("logs");
    let reports_dir = state_dir.join("reports");
    let state_subdir = state_dir.join("state");

    fs::write(logs_dir.join("watch.log"), "hello").unwrap();
    fs::write(reports_dir.join("latest-impact.json"), "{}").unwrap();
    fs::write(state_subdir.join("ledger.db"), "db").unwrap();
    fs::write(state_subdir.join("ledger.db-wal"), "wal").unwrap();
    fs::write(state_subdir.join("ledger.db-shm"), "shm").unwrap();
    fs::write(state_subdir.join("current-batch.json"), "{}").unwrap();

    execute_reset(false, false, false, false, false, false).unwrap();

    assert!(state_dir.exists());
    assert!(state_dir.join("config.toml").exists());
    assert!(state_dir.join("rules.toml").exists());
    assert!(!logs_dir.exists());
    assert!(!reports_dir.exists());
    // State subdir persists because ledger.db is preserved by default
    assert!(state_subdir.exists());
    assert!(state_subdir.join("ledger.db").exists());
    assert!(!state_subdir.join("current-batch.json").exists());
}

#[test]
fn test_reset_preserves_ledger_by_default() {
    let (_tmp, root, _guard) = setup_repo();
    execute_init(false).unwrap();

    let state_dir = root.join(".ledgerful");
    let state_subdir = state_dir.join("state");

    fs::write(state_subdir.join("ledger.db"), "db").unwrap();
    fs::write(state_subdir.join("ledger.db-wal"), "wal").unwrap();

    // Default reset preserves ledger.db
    execute_reset(false, false, false, false, false, false).unwrap();

    assert!(state_subdir.join("ledger.db").exists());
    assert!(state_subdir.join("ledger.db-wal").exists());
}

#[test]
fn test_reset_include_ledger_removes_db() {
    let (_tmp, root, _guard) = setup_repo();
    execute_init(false).unwrap();

    let state_dir = root.join(".ledgerful");
    let state_subdir = state_dir.join("state");

    fs::write(state_subdir.join("ledger.db"), "db").unwrap();

    // --include-ledger removes ledger.db (requires --yes)
    execute_reset(false, false, true, false, true, false).unwrap();

    assert!(!state_subdir.join("ledger.db").exists());
}

#[test]
fn test_reset_remove_config_and_rules_requires_confirmation() {
    let (_tmp, root, _guard) = setup_repo();
    execute_init(false).unwrap();

    let err = execute_reset(true, true, false, false, false, false).unwrap_err();
    assert!(format!("{err}").contains("--yes"));
    assert!(root.join(".ledgerful").join("config.toml").exists());
    assert!(root.join(".ledgerful").join("rules.toml").exists());
}

#[test]
fn test_reset_remove_config_and_rules_with_confirmation() {
    let (_tmp, root, _guard) = setup_repo();
    execute_init(false).unwrap();

    execute_reset(true, true, false, false, true, false).unwrap();

    let state_dir = root.join(".ledgerful");
    assert!(!state_dir.join("config.toml").exists());
    assert!(!state_dir.join("rules.toml").exists());
}

#[test]
fn test_reset_all_requires_confirmation() {
    let (_tmp, root, _guard) = setup_repo();
    execute_init(false).unwrap();

    let err = execute_reset(false, false, false, true, false, false).unwrap_err();
    assert!(format!("{err}").contains("--yes"));
    assert!(root.join(".ledgerful").exists());
}

#[test]
fn test_reset_all_removes_entire_tree() {
    let (_tmp, root, _guard) = setup_repo();
    execute_init(false).unwrap();

    execute_reset(false, false, false, true, true, false).unwrap();

    assert!(!root.join(".ledgerful").exists());
}

#[test]
fn test_reset_is_idempotent() {
    let (_tmp, root, _guard) = setup_repo();
    execute_init(false).unwrap();

    execute_reset(false, false, false, false, false, false).unwrap();
    execute_reset(false, false, false, false, false, false).unwrap();

    assert!(root.join(".ledgerful").exists());
    assert!(root.join(".ledgerful").join("config.toml").exists());
}

#[test]
fn test_reset_never_touches_outside_ledgerful() {
    let (_tmp, root, _guard) = setup_repo();
    execute_init(false).unwrap();

    let outside = root.join("keep.txt");
    fs::write(&outside, "keep").unwrap();

    execute_reset(false, false, false, false, false, false).unwrap();

    assert!(outside.exists());
    assert_eq!(fs::read_to_string(outside).unwrap(), "keep");
}

#[test]
fn test_reset_dry_run_does_not_modify_anything() {
    let (_tmp, root, _guard) = setup_repo();
    execute_init(false).unwrap();

    let state_dir = root.join(".ledgerful");
    let logs_dir = state_dir.join("logs");
    fs::write(logs_dir.join("watch.log"), "hello").unwrap();

    // Run with dry_run = true
    execute_reset(false, false, false, false, false, true).unwrap();

    // Verify it still exists
    assert!(logs_dir.join("watch.log").exists());
}
