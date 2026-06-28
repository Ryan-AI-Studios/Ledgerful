use crate::common::{DirGuard, git_add_and_commit_no_verify, setup_git_repo};
use camino::Utf8Path;
use ledgerful::commands::hook_post_commit::{
    execute_hook_post_commit, execute_hook_post_commit_for_layout,
};
use ledgerful::commands::index::{IndexArgs, execute_index};
use ledgerful::commands::init::execute_init;
use ledgerful::state::layout::Layout;
use ledgerful::state::storage::StorageManager;

use std::fs;
use std::process::Command;
use std::time::Duration;
use tempfile::tempdir;

/// Bounded poll helper used because the production hook records trends in a
/// background thread. Matches `tests/integration/common/sync.rs`.
fn wait_for_condition<F: Fn() -> bool>(
    check: F,
    timeout: Duration,
    interval: Duration,
) -> Result<(), ()> {
    let start = std::time::Instant::now();
    while start.elapsed() < timeout {
        if check() {
            return Ok(());
        }
        std::thread::sleep(interval);
    }
    Err(())
}

/// Builds a minimal git repo with an indexed `.ledgerful` state directory,
/// ready for the post-commit hook to be invoked. Returns the repo root.
fn setup_indexed_repo() -> tempfile::TempDir {
    let tmp = tempdir().unwrap();
    let root = tmp.path();

    setup_git_repo(root);
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("src/lib.rs"),
        "pub fn hotspot_fn(x: i32) -> i32 {\n    if x > 0 { x + 1 } else { x - 1 }\n}\n",
    )
    .unwrap();
    git_add_and_commit_no_verify(root, "initial");

    // Enough commits for temporal coupling history to succeed, mirroring
    // cli_hotspots::setup_indexed_repo.
    for i in 1..=12 {
        fs::write(
            root.join("src/lib.rs"),
            format!(
                "pub fn hotspot_fn(x: i32) -> i32 {{\n    if x > {i} {{ x + 1 }} else {{ x - 1 }}\n}}\n"
            ),
        )
        .unwrap();
        git_add_and_commit_no_verify(root, &format!("touch {i}"));
    }

    // Set HOME/USERPROFILE to the temp dir so crypto key operations write
    // into the temp dir instead of the real user home. Required because
    // nextest runs each test in its own process and tests may race on the
    // key store.
    let _home_guard = crate::common::crypto_home_guard(root);

    let _guard = DirGuard::new(root);
    execute_init(false).unwrap();
    execute_index(IndexArgs::default()).unwrap();

    // Remove the auto-installed post-commit hook so tests can drive the hook
    // explicitly. The ledger gate pre-commit/pre-push hooks are also removed
    // to avoid them firing during test commits.
    let hooks_dir = root.join(".git").join("hooks");
    for hook in ["post-commit", "pre-commit", "pre-push", "commit-msg"] {
        let _ = fs::remove_file(hooks_dir.join(hook));
    }

    // The current working directory must remain in the temp repo so that the
    // in-process hook entry point `execute_hook_post_commit` (used by the
    // non-blocking test) resolves the repository correctly.
    let _ = std::env::set_current_dir(root);

    tmp
}

fn hotspot_trends_count(root: &std::path::Path) -> i64 {
    let repo_root = Utf8Path::from_path(root).unwrap();
    let storage = StorageManager::open_read_only_sqlite_only(repo_root).unwrap();
    let conn = storage.get_connection();
    conn.query_row("SELECT COUNT(*) FROM hotspot_trends", [], |row| row.get(0))
        .unwrap()
}

fn hotspot_trends_for_commit(root: &std::path::Path, commit_hash: &str) -> i64 {
    let repo_root = Utf8Path::from_path(root).unwrap();
    let storage = StorageManager::open_read_only_sqlite_only(repo_root).unwrap();
    let conn = storage.get_connection();
    conn.query_row(
        "SELECT COUNT(*) FROM hotspot_trends WHERE commit_hash = ?1",
        [commit_hash],
        |row| row.get(0),
    )
    .unwrap()
}

#[test]
fn test_post_commit_hook_records_hotspot_trends() {
    let tmp = setup_indexed_repo();
    let root = tmp.path();

    assert_eq!(
        hotspot_trends_count(root),
        0,
        "precondition: no hotspot_trends rows"
    );

    // Invoke the hook bound to the temp repo (synchronous ledger promotion +
    // background thread). Wait briefly for the background thread to finish
    // writing.
    let layout = Layout::new(Utf8Path::from_path(root).unwrap());
    execute_hook_post_commit_for_layout(&layout).unwrap();
    wait_for_condition(
        || hotspot_trends_count(root) > 0,
        Duration::from_secs(5),
        Duration::from_millis(50),
    )
    .expect("post-commit hook should record hotspot_trends rows");

    let head = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(root)
        .output()
        .unwrap();
    let head_hash = String::from_utf8_lossy(&head.stdout).trim().to_string();

    assert!(
        hotspot_trends_for_commit(root, &head_hash) > 0,
        "expected rows recorded for current HEAD"
    );
}

#[test]
fn test_post_commit_hook_is_non_blocking_and_exits_ok() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();

    // No git repo and no state — hook should still return Ok immediately.
    let _guard = DirGuard::new(root);
    let result = execute_hook_post_commit();
    assert!(
        result.is_ok(),
        "hook must silently exit 0 on missing repo/state"
    );
}

#[test]
fn test_hotspots_trend_shows_multiple_timestamps_after_three_commits() {
    let tmp = setup_indexed_repo();
    let root = tmp.path();

    // Record trends for three distinct commits, waiting after each so the
    // background thread has time to write before we add the next commit.
    for i in 1..=3 {
        fs::write(
            root.join("src/lib.rs"),
            format!(
                "pub fn hotspot_fn(x: i32) -> i32 {{\n    if x > {i}00 {{ x + 1 }} else {{ x - 1 }}\n}}\n"
            ),
        )
        .unwrap();
        git_add_and_commit_no_verify(root, &format!("commit {i}"));
        let layout = Layout::new(Utf8Path::from_path(root).unwrap());
        execute_hook_post_commit_for_layout(&layout).unwrap();
        wait_for_condition(
            || hotspot_trends_count(root) >= i as i64,
            Duration::from_secs(5),
            Duration::from_millis(50),
        )
        .expect("hook should record at least one row per commit");
    }

    let ledgerful_bin = env!("CARGO_BIN_EXE_ledgerful");
    let output = Command::new(ledgerful_bin)
        .args(["hotspots", "trend", "--json"])
        .current_dir(root)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "CLI command failed: {:?}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let json: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let entries = json["entries"].as_array().unwrap();
    assert!(
        entries.len() >= 3,
        "expected 3+ trend entries, got {}: {stdout}",
        entries.len()
    );
}

#[test]
fn test_hotspots_trend_shows_staleness_hint_when_head_differs() {
    let tmp = setup_indexed_repo();
    let root = tmp.path();

    let layout = Layout::new(Utf8Path::from_path(root).unwrap());
    execute_hook_post_commit_for_layout(&layout).unwrap();
    wait_for_condition(
        || hotspot_trends_count(root) > 0,
        Duration::from_secs(5),
        Duration::from_millis(50),
    )
    .expect("hook should record initial trend");

    // Add a new commit so the last recorded hash no longer matches HEAD.
    fs::write(root.join("src/lib.rs"), "pub fn new_fn() {}\n").unwrap();
    git_add_and_commit_no_verify(root, "after hook");

    let ledgerful_bin = env!("CARGO_BIN_EXE_ledgerful");
    let output = Command::new(ledgerful_bin)
        .args(["hotspots", "trend"])
        .current_dir(root)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "CLI command failed: {:?}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Trend data is stale"),
        "expected staleness hint, got: {stdout}"
    );
}
