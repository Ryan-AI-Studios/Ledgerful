//! Track DX1 â€” interactive-surface bootstrapping E2E degrade tests.
//!
//! These tests exercise the non-interactive degrade contract: when
//! `LEDGERFUL_NON_INTERACTIVE=1` is set (as the harness does globally via
//! `DirGuard`), `hotspots trend`,
//! `security boundaries`, and `observability coverage` must NOT block on stdin,
//! must exit 0, must print the same read-only empty-state messages they always
//! have (no prompt text), and must produce NO side effects â€” no `policies/`
//! or `observability/` files created under the temp repo.
//!
//! The interactive generate+write path is covered by unit tests in
//! `src/commands/dx1_templates.rs` (deterministic generators + tempdir write
//! helpers) and `src/util/term.rs` (`prompt_yes_no_with` over injected input),
//! which is the deterministic, TTY-free way to test that logic.

use crate::common::{DirGuard, git_add_and_commit, non_interactive, setup_git_repo};
use camino::Utf8Path;
use ledgerful::commands::index::{IndexArgs, execute_index};
use ledgerful::commands::init::execute_init;
use ledgerful::state::storage::StorageManager;
use serial_test::serial;
use std::fs;
use std::process::{Command, Stdio};
use tempfile::tempdir;

/// Build a minimal git repo with an indexed `.ledgerful` state directory,
/// ready for the `ledgerful` binary to be invoked against it. Mirrors the
/// shape of `cli_hotspots::setup_indexed_repo` but deliberately kept small (a
/// handful of commits) so hotspot history is empty and the surfaces we test
/// here hit their empty-state branches.
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
    git_add_and_commit(root, "initial");

    for i in 1..=3 {
        fs::write(
            root.join("src/lib.rs"),
            format!(
                "pub fn hotspot_fn(x: i32) -> i32 {{\n    if x > {i} {{ x + 1 }} else {{ x - 1 }}\n}}\n"
            ),
        )
        .unwrap();
        git_add_and_commit(root, &format!("touch {i}"));
    }

    let _guard = DirGuard::new(root);
    let _env = non_interactive();
    execute_init(false).unwrap();
    execute_index(IndexArgs::default()).unwrap();

    tmp
}

/// Count rows in `hotspot_history` for the temp repo (read-only contract
/// check: a non-interactive degrade run must not mutate history).
fn hotspot_history_count(root: &std::path::Path) -> i64 {
    let repo_root = Utf8Path::from_path(root).unwrap();
    let storage = StorageManager::open_read_only_sqlite_only(repo_root).unwrap();
    let conn = storage.get_connection();
    conn.query_row("SELECT COUNT(*) FROM hotspot_history", [], |row| row.get(0))
        .unwrap()
}

/// Non-interactive degrade: `hotspots trend` must not block, must exit 0, must
/// print the read-only empty-state messages (no prompt text), and must not
/// create hotspot history.
#[test]
#[serial(env, cwd)]
fn test_hotspots_trend_non_interactive_degrades_read_only() {
    let tmp = setup_indexed_repo();
    let root = tmp.path();

    let ledgerful_bin = env!("CARGO_BIN_EXE_ledgerful");
    let output = Command::new(ledgerful_bin)
        .args(["hotspots", "trend"])
        // Explicit non-interactive override + null stdin so the run cannot block
        // even if the TTY check were to misfire.
        .env("LEDGERFUL_NON_INTERACTIVE", "1")
        .stdin(Stdio::null())
        .current_dir(root)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "non-interactive `hotspots trend` must exit 0, got: {:?}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        stdout.contains("No trend history yet for this repository."),
        "expected read-only empty-state message, got: {stdout}"
    );
    assert!(
        stdout.contains("ledgerful hotspots trend --bootstrap"),
        "expected the exact bootstrap command hint, got: {stdout}"
    );
    // No prompt must be printed in non-interactive mode.
    assert!(
        !stdout.contains("[Y/n]"),
        "non-interactive run must not print a prompt: {stdout}"
    );

    // Read-only contract: history untouched, no side effects.
    assert_eq!(
        hotspot_history_count(root),
        0,
        "non-interactive `hotspots trend` must not mutate hotspot_history"
    );
}

/// Non-interactive degrade: `security boundaries` must not block, must exit 0,
/// must print a read-only empty-state message, and must not create a
/// `policies/` directory under the temp repo.
#[test]
#[serial(env, cwd)]
fn test_security_boundaries_non_interactive_degrades_read_only() {
    let tmp = setup_indexed_repo();
    let root = tmp.path();

    let ledgerful_bin = env!("CARGO_BIN_EXE_ledgerful");
    let output = Command::new(ledgerful_bin)
        .args(["security", "boundaries"])
        .env("LEDGERFUL_NON_INTERACTIVE", "1")
        .stdin(Stdio::null())
        .current_dir(root)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "non-interactive `security boundaries` must exit 0, got: {:?}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);

    // The repo has no Cedar policy files, so the surface reports an empty
    // state. Either the "no Cedar policy data" branch or the "knowledge graph
    // has not been built" branch may fire depending on whether indexing
    // populated graph nodes; both are read-only and neither prints a prompt.
    assert!(
        !stdout.contains("[Y/n]"),
        "non-interactive run must not print a prompt: {stdout}"
    );
    // No side effects: policies/ must not be created on the degrade path.
    assert!(
        !root.join("policies").exists(),
        "non-interactive `security boundaries` must not create policies/"
    );
}

/// Non-interactive degrade: `observability coverage` must not block, must
/// exit 0, must print the read-only empty-state messages, and must not create
/// an `observability/` directory under the temp repo.
#[test]
#[serial(env, cwd)]
fn test_observability_coverage_non_interactive_degrades_read_only() {
    let tmp = setup_indexed_repo();
    let root = tmp.path();

    let ledgerful_bin = env!("CARGO_BIN_EXE_ledgerful");
    let output = Command::new(ledgerful_bin)
        .args(["observability", "coverage"])
        .env("LEDGERFUL_NON_INTERACTIVE", "1")
        .stdin(Stdio::null())
        .current_dir(root)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "non-interactive `observability coverage` must exit 0, got: {:?}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        stdout.contains("No OpenSLO coverage data found."),
        "expected read-only empty-state message, got: {stdout}"
    );
    assert!(
        !stdout.contains("[Y/n]"),
        "non-interactive run must not print a prompt: {stdout}"
    );
    // No side effects: observability/ must not be created on the degrade path.
    assert!(
        !root.join("observability").exists(),
        "non-interactive `observability coverage` must not create observability/"
    );
}
