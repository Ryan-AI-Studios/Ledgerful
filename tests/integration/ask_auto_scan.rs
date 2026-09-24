//! Track DX6: `--auto-scan` for stale impact context.
//!
//! These tests exercise the in-memory impact path (`compute_impact_in_memory`)
//! that `ledgerful ask --auto-scan` (and `[ask].auto_scan_default = true`)
//! routes through. The deterministic assertions target the helper directly so
//! they do not depend on an LLM backend being available: the in-memory scan
//! must reflect the live working tree and must NOT persist a packet or report.

use ledgerful::commands::impact::compute_impact_in_memory;
use ledgerful::config::model::Config;
use ledgerful::state::layout::Layout;
use ledgerful::state::storage::StorageManager;
use std::fs;
use tempfile::tempdir;

use crate::common::{DirGuard, git_add_and_commit, setup_git_repo};

/// `compute_impact_in_memory` must detect a dirty working tree, surface the
/// modified file in the packet, and leave the stored snapshot table empty
/// (the in-memory path is strictly non-persisting by DX6 contract).
#[test]
fn test_compute_impact_in_memory_detects_dirty_tree_without_persisting() {
    let tmp = tempdir().unwrap();
    let root = camino::Utf8Path::from_path(tmp.path()).unwrap();
    let _guard = DirGuard::from_utf8(root);

    setup_git_repo(tmp.path());
    // Commit a tracked file, then modify it so the tree is dirty.
    let src_dir = tmp.path().join("src");
    fs::create_dir_all(&src_dir).unwrap();
    let lib = src_dir.join("lib.rs");
    fs::write(&lib, "pub fn original() {}\n").unwrap();
    git_add_and_commit(tmp.path(), "initial");
    // Dirty the tree with a new public function.
    fs::write(&lib, "pub fn original() {}\npub fn added_by_edit() {}\n").unwrap();

    let layout = Layout::new(root);
    layout.ensure_state_dir().unwrap();
    let storage =
        StorageManager::init(layout.state_subdir().join("ledger.db").as_std_path()).unwrap();
    let config = Config::default();

    let packet = compute_impact_in_memory(&storage, &config).unwrap();

    // Fresh packet reflects the live dirty tree.
    assert!(
        !packet.tree_clean,
        "dirty tree must be reported as not clean"
    );
    assert_eq!(
        packet.changes.len(),
        1,
        "exactly one modified file expected, got {:?}",
        packet
            .changes
            .iter()
            .map(|c| c.path.display().to_string())
            .collect::<Vec<_>>()
    );
    let changed = &packet.changes[0];
    assert!(
        changed.path.ends_with("lib.rs"),
        "changed path should be lib.rs, got {}",
        changed.path.display()
    );

    // DX6 contract: the in-memory path must NOT persist the packet.
    assert!(
        storage.get_latest_packet().unwrap().is_none(),
        "in-memory scan must not write a snapshot to storage"
    );

    storage.shutdown().unwrap();
}

/// On a clean tree the in-memory scan must return a packet with no changes
/// and `tree_clean = true` (ask then treats this as global context).
#[test]
fn test_compute_impact_in_memory_clean_tree() {
    let tmp = tempdir().unwrap();
    let root = camino::Utf8Path::from_path(tmp.path()).unwrap();
    let _guard = DirGuard::from_utf8(root);

    setup_git_repo(tmp.path());
    let src_dir = tmp.path().join("src");
    fs::create_dir_all(&src_dir).unwrap();
    fs::write(src_dir.join("lib.rs"), "pub fn original() {}\n").unwrap();
    git_add_and_commit(tmp.path(), "initial");
    // No further edits â†’ clean tree.

    let layout = Layout::new(root);
    layout.ensure_state_dir().unwrap();
    let storage =
        StorageManager::init(layout.state_subdir().join("ledger.db").as_std_path()).unwrap();
    let config = Config::default();

    let packet = compute_impact_in_memory(&storage, &config).unwrap();

    assert!(packet.tree_clean, "clean tree must be reported as clean");
    assert!(packet.changes.is_empty(), "clean tree must have no changes");

    storage.shutdown().unwrap();
}

/// `compute_impact_in_memory` must degrade gracefully when there is no git
/// repository rather than aborting the caller's `ask` flow.
#[test]
fn test_compute_impact_in_memory_errors_outside_git_repo() {
    let tmp = tempdir().unwrap();
    let root = camino::Utf8Path::from_path(tmp.path()).unwrap();
    let _guard = DirGuard::from_utf8(root);

    // No setup_git_repo: not a git repository.
    let layout = Layout::new(root);
    layout.ensure_state_dir().unwrap();
    let storage =
        StorageManager::init(layout.state_subdir().join("ledger.db").as_std_path()).unwrap();
    let config = Config::default();

    let result = compute_impact_in_memory(&storage, &config);

    assert!(
        result.is_err(),
        "in-memory scan must error outside a git repo so ask can fall back"
    );

    storage.shutdown().unwrap();
}

/// DX6 end-to-end: with `--auto-scan`, `ask` computes a fresh packet from the
/// live dirty tree and suppresses the stale-impact warning (the packet is
/// fresh by construction). Without `--auto-scan`, the same stale cached
/// packet still triggers the warning. Uses the real binary so the assertion
/// covers the full `execute_ask` path, not just the helper.
#[test]
fn test_ask_auto_scan_suppresses_stale_warning_with_fresh_packet() {
    use std::process::Command;

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

    // Record a cached packet against a dirty tree, then advance HEAD so the
    // cached packet is stale (head_hash != current HEAD).
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
    git_add_and_commit(root, "advance head past the cached packet");

    // Dirty the tree again so `--auto-scan` finds a fresh, non-empty diff.
    fs::write(root.join("a.txt"), "v3").unwrap();

    // A DiffTask query ("the change") is NOT pruned by DX5, so the only thing
    // that can suppress the stale warning here is `fresh_packet` from DX6.
    let query = "walk me through the change";
    // The impact-stale warning's unique phrase (the index-staleness warning
    // elsewhere in `execute_ask` also says "stale", so assert on this phrase
    // to isolate the impact-stale signal).
    let impact_stale_phrase = "using it as ask context anyway";

    // With --auto-scan: fresh packet â†’ impact-stale warning suppressed, and the
    // auto-scan notice is printed to stderr. `--timeout 1` makes the LLM call
    // (which runs AFTER the warning/notice stage) fail fast so the test does
    // not depend on a live backend and stays quick; the assertions are on
    // stderr emitted before any LLM contact.
    let with_scan = Command::new(ledgerful_bin)
        .args(["ask", "--auto-scan", "--timeout", "1", query])
        .current_dir(root)
        .env("LEDGERFUL_NON_INTERACTIVE", "1")
        .env_remove("GEMINI_API_KEY")
        .output()
        .unwrap();
    let with_scan_err = String::from_utf8_lossy(&with_scan.stderr);
    assert!(
        with_scan_err.to_lowercase().contains("auto-scanning"),
        "auto-scan notice must appear on stderr, got: {with_scan_err}"
    );
    assert!(
        !with_scan_err.contains(impact_stale_phrase),
        "fresh auto-scan packet must suppress the impact-stale warning, got stderr: {with_scan_err}"
    );

    // Without --auto-scan: the stale cached packet must still warn. Same
    // `--timeout 1` rationale â€” the warning is emitted before the LLM call.
    let without_scan = Command::new(ledgerful_bin)
        .args(["ask", "--timeout", "1", query])
        .current_dir(root)
        .env("LEDGERFUL_NON_INTERACTIVE", "1")
        .env_remove("GEMINI_API_KEY")
        .output()
        .unwrap();
    let without_scan_err = String::from_utf8_lossy(&without_scan.stderr);
    assert!(
        without_scan_err.contains(impact_stale_phrase),
        "cached stale packet must still warn without --auto-scan, got stderr: {without_scan_err}"
    );
}
