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
    execute_init(false, false).unwrap();
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

/// Non-interactive degrade contract: each read-only surface must exit 0,
/// must not print a `[Y/n]` prompt, and must not create its side-effect path.
/// The first case also asserts the exact empty-state messages + bootstrap hint.
#[rstest::rstest]
#[case::hotspots_trend(
    &["hotspots", "trend"],
    Some("No trend history yet for this repository."),
    Some("ledgerful hotspots trend --bootstrap"),
    None,
    true,
)]
#[case::security_boundaries(
    &["security", "boundaries"],
    None,
    None,
    Some("policies"),
    true,
)]
#[case::observability_coverage(
    &["observability", "coverage"],
    Some("No OpenSLO coverage data found."),
    None,
    Some("observability"),
    true,
)]
#[serial(env, cwd)]
fn non_interactive_degrades_read_only(
    #[case] command: &[&str],
    #[case] expected_empty_message: Option<&str>,
    #[case] expected_bootstrap_hint: Option<&str>,
    #[case] side_effect_path: Option<&str>,
    #[case] expect_success: bool,
) {
    let tmp = setup_indexed_repo();
    let root = tmp.path();

    let ledgerful_bin = env!("CARGO_BIN_EXE_ledgerful");
    let output = Command::new(ledgerful_bin)
        .args(command)
        .env("LEDGERFUL_NON_INTERACTIVE", "1")
        .stdin(Stdio::null())
        .current_dir(root)
        .output()
        .unwrap();

    assert_eq!(
        output.status.success(),
        expect_success,
        "command {:?} exit-status mismatch, stderr: {:?}",
        command,
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);

    if let Some(msg) = expected_empty_message {
        assert!(
            stdout.contains(msg),
            "expected read-only empty-state message {msg:?}, got: {stdout}"
        );
    }
    if let Some(hint) = expected_bootstrap_hint {
        assert!(
            stdout.contains(hint),
            "expected bootstrap hint {hint:?}, got: {stdout}"
        );
    }
    assert!(
        !stdout.contains("[Y/n]"),
        "non-interactive run must not print a prompt: {stdout}"
    );
    if let Some(path) = side_effect_path {
        assert!(
            !root.join(path).exists(),
            "non-interactive {:?} must not create {path}/",
            command
        );
    }
    // hotspots trend is a read-only degrade: history must stay empty.
    if command == ["hotspots", "trend"] {
        assert_eq!(
            hotspot_history_count(root),
            0,
            "non-interactive `hotspots trend` must not mutate hotspot_history"
        );
    }
}
