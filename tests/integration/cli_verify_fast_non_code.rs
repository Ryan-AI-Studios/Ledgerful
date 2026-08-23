//! 0203 — `--scope fast` NonCodeCheap + dry-run `scope:` first line.
//! Hermetic temp git repos; does not dirty the engine worktree.

use crate::common::{git_add_and_commit, git_cmd, setup_git_repo};
use std::fs;
use std::path::Path;
use std::process::{Command, Output};
use tempfile::tempdir;

fn spawn_verify(root: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ledgerful"))
        .args(args)
        .current_dir(root)
        .env("LEDGERFUL_NON_INTERACTIVE", "1")
        .env_remove("LEDGERFUL_STATE_DIR")
        .output()
        .expect("spawn ledgerful")
}

fn stdout_text(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn first_nonempty_line(stdout: &str) -> &str {
    stdout
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim()
}

fn write_rust_manifest(root: &Path) {
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"overlay-fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .expect("Cargo.toml");
}

fn init_committed_rust_repo(root: &Path) {
    setup_git_repo(root);
    write_rust_manifest(root);
    git_add_and_commit(root, "init");
}

#[test]
fn test_fast_dry_run_dirty_changelog_docs_no_snapshot_is_cheap() {
    // DoD-1 / P2: no `.ledgerful` packet / no index; CHANGELOG + docs dirty
    // still cheap (must not refuse "no impact packet").
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    init_committed_rust_repo(root);
    fs::write(root.join("CHANGELOG.md"), "## Unreleased\n").unwrap();
    fs::create_dir_all(root.join("docs")).unwrap();
    fs::write(root.join("docs").join("installation.md"), "install\n").unwrap();

    let out = spawn_verify(root, &["verify", "--scope", "fast", "--dry-run"]);
    let stdout = stdout_text(&out);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "docs/CHANGELOG cheap dry-run must exit 0; stdout={stdout:?} stderr={stderr:?}"
    );
    assert_eq!(
        first_nonempty_line(&stdout),
        "scope: fast",
        "first product line: {stdout:?}"
    );
    assert!(
        !stdout.contains("nextest") && !stdout.to_ascii_lowercase().contains("cargo test"),
        "must not schedule nextest/cargo test: {stdout:?}"
    );
    assert!(
        stdout.contains("cargo fmt") && stdout.contains("clippy"),
        "fmt+clippy must be present: {stdout:?}"
    );
    assert!(
        !stdout.contains("no impact packet"),
        "P2: None-packet dirty docs must not refuse: {stdout:?} {stderr:?}"
    );
}

#[test]
fn test_default_verify_dry_run_first_line_names_full_scope() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    init_committed_rust_repo(root);

    let out = spawn_verify(root, &["verify", "--dry-run"]);
    let stdout = stdout_text(&out);
    assert_eq!(
        first_nonempty_line(&stdout),
        "scope: full (pre-push uses --scope fast)",
        "default dry-run first line: {stdout:?}"
    );
    assert!(
        !stdout.contains("CLI default"),
        "must not claim CLI default: {stdout:?}"
    );
}

#[test]
fn test_explicit_full_dry_run_same_static_scope_line() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    init_committed_rust_repo(root);

    let out = spawn_verify(root, &["verify", "--scope", "full", "--dry-run"]);
    let stdout = stdout_text(&out);
    assert_eq!(
        first_nonempty_line(&stdout),
        "scope: full (pre-push uses --scope fast)",
        "explicit --scope full dry-run: {stdout:?}"
    );
    assert!(!stdout.contains("CLI default"));
}

#[test]
fn test_refuse_dry_run_scope_line_above_info() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    init_committed_rust_repo(root);
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src").join("foo.rs"), "fn x() {}\n").unwrap();

    let out = spawn_verify(root, &["verify", "--scope", "fast", "--dry-run"]);
    let stdout = stdout_text(&out);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "unmapped src must refuse; stdout={stdout:?} stderr={stderr:?}"
    );
    assert_eq!(
        first_nonempty_line(&stdout),
        "scope: fast",
        "refuse dry-run first line: {stdout:?}"
    );
    let scope_at = stdout.find("scope:").expect("scope line");
    let info_at = stdout
        .find("fast scope unavailable")
        .or_else(|| stdout.find("ℹ"))
        .expect("ℹ reason");
    assert!(
        scope_at < info_at,
        "scope line must be above ℹ; stdout={stdout:?}"
    );
}

#[test]
fn test_command_dry_run_first_line_is_scope() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    init_committed_rust_repo(root);

    // Manual command is positional (`verify <command> --dry-run`), not `--command`.
    let out = spawn_verify(root, &["verify", "echo hello", "--dry-run"]);
    let stdout = stdout_text(&out);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "manual command dry-run must exit 0; stdout={stdout:?} stderr={stderr:?}"
    );
    assert_eq!(
        first_nonempty_line(&stdout),
        "scope: full (pre-push uses --scope fast)",
        "manual command --dry-run first line: {stdout:?}"
    );

    let out_fast = spawn_verify(
        root,
        &["verify", "--scope", "fast", "echo hello", "--dry-run"],
    );
    let stdout_fast = stdout_text(&out_fast);
    let stderr_fast = String::from_utf8_lossy(&out_fast.stderr);
    assert!(
        out_fast.status.success(),
        "fast manual command dry-run must exit 0; stdout={stdout_fast:?} stderr={stderr_fast:?}"
    );
    assert_eq!(
        first_nonempty_line(&stdout_fast),
        "scope: fast",
        "--scope fast manual command --dry-run first line: {stdout_fast:?}"
    );
}

#[test]
fn test_verify_json_dry_run_still_errors() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    init_committed_rust_repo(root);

    let out = spawn_verify(root, &["verify", "--json", "--dry-run"]);
    assert!(!out.status.success(), "verify --json --dry-run must error");
    let stdout = stdout_text(&out);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let combined = format!("{stdout}{stderr}");
    assert!(
        combined.contains("cannot be combined") || combined.contains("dry-run"),
        "reject message; combined={combined:?}"
    );
    if !stdout.trim().is_empty() {
        assert!(
            !stdout.contains("schemaVersion"),
            "must not emit VerifyCliJson; stdout={stdout:?}"
        );
    }
}

#[test]
fn test_fast_dry_run_dirty_src_is_not_silently_cheap() {
    // Mixed/src dirty with no mapping still refuses (DoD-3 frozen).
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    init_committed_rust_repo(root);
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src").join("lib.rs"), "pub fn x() {}\n").unwrap();
    git_cmd(root, &["add", "src/lib.rs"]); // still fine if untracked too

    let out = spawn_verify(root, &["verify", "--scope", "fast", "--dry-run"]);
    assert!(!out.status.success());
    let stdout = stdout_text(&out);
    assert_eq!(first_nonempty_line(&stdout), "scope: fast");
}
