//! Track 0211: live-clean git overrides a cached dirty SQLite impact packet.
//!
//! Sibling of `ask_auto_scan.rs`. After `scan --impact` on a dirty tree, the
//! working tree is committed clean while the snapshot stays dirty. Default
//! `ask` must not inject that packet as current changes.

use std::fs;
use std::process::Command;
use tempfile::tempdir;

use crate::common::{git_add_and_commit, setup_git_repo};

/// Dirty `scan --impact`, then commit so porcelain is empty: default `ask`
/// stderr must contain the shared no-pending constant and must not print
/// Auto-scanning or the stale-cache "using it as ask context anyway" line.
#[test]
fn test_ask_empty_tree_stderr_contains_no_pending_changes() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);

    fs::write(root.join("a.txt"), "v1").unwrap();
    git_add_and_commit(root, "initial");

    let ledgerful_bin = env!("CARGO_BIN_EXE_ledgerful");
    Command::new(ledgerful_bin)
        .arg("init")
        .current_dir(root)
        .output()
        .unwrap();

    fs::write(root.join("a.txt"), "v2").unwrap();
    let scan_out = Command::new(ledgerful_bin)
        .args(["scan", "--impact"])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        scan_out.status.success(),
        "scan --impact failed: {}",
        String::from_utf8_lossy(&scan_out.stderr)
    );
    git_add_and_commit(root, "commit so porcelain is empty");

    let query = "what is change-context";
    let impact_stale_phrase = "using it as ask context anyway";

    let ask_out = Command::new(ledgerful_bin)
        .args(["ask", "--timeout", "1", query])
        .current_dir(root)
        .env("LEDGERFUL_NON_INTERACTIVE", "1")
        .env_remove("GEMINI_API_KEY")
        .output()
        .unwrap();
    let ask_err = String::from_utf8_lossy(&ask_out.stderr);
    assert!(
        ask_err.contains("No pending changes found"),
        "live-clean ask must print the no-pending constant, got: {ask_err}"
    );
    assert!(
        !ask_err.contains(impact_stale_phrase),
        "live-clean wall must not use the cached packet as ask context, got: {ask_err}"
    );
    assert!(
        !ask_err.to_lowercase().contains("auto-scanning"),
        "live-clean wall must skip Auto-scanning, got: {ask_err}"
    );
}
