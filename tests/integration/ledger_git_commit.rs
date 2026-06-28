use std::path::Path;
use std::process::Command;

use tempfile::TempDir;

fn binary() -> &'static str {
    option_env!("CARGO_BIN_EXE_ledgerful").unwrap_or("target/debug/ledgerful")
}

fn run_ok(root: &Path, args: &[&str]) -> String {
    let output = Command::new(binary())
        .current_dir(root)
        .args(args)
        .output()
        .expect("failed to execute ledgerful");
    assert!(
        output.status.success(),
        "command failed: {:?}\nstdout:\n{}\nstderr:\n{}",
        args,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).to_string()
}

fn run_ok_with_stderr(root: &Path, args: &[&str]) -> (String, String) {
    let output = Command::new(binary())
        .current_dir(root)
        .args(args)
        .output()
        .expect("failed to execute ledgerful");
    assert!(
        output.status.success(),
        "command failed: {:?}\nstdout:\n{}\nstderr:\n{}",
        args,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    (
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
    )
}

fn run_git(root: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(root)
        .args(args)
        .output()
        .expect("failed to execute git");
    assert!(
        output.status.success(),
        "git failed: {:?}\nstdout:\n{}\nstderr:\n{}",
        args,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).to_string()
}

fn extract_tx_id(stdout: &str) -> String {
    let pattern = regex::Regex::new(
        "[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}",
    )
    .unwrap();
    pattern
        .find(stdout)
        .expect("missing transaction id")
        .as_str()
        .to_string()
}

fn setup_repo() -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    run_git(root, &["init"]);
    run_git(root, &["config", "user.email", "ledgerful@example.invalid"]);
    run_git(root, &["config", "user.name", "Ledgerful Test"]);
    run_git(root, &["config", "commit.gpgsign", "false"]);
    run_git(root, &["config", "core.hooksPath", ""]);
    run_ok(root, &["init"]);
    dir
}

#[test]
fn ledger_commit_with_git_creates_git_commit() {
    let dir = setup_repo();
    let root = dir.path();
    let entity = root.join("src").join("main.rs");
    std::fs::create_dir_all(entity.parent().unwrap()).unwrap();
    std::fs::write(&entity, "fn main() {}\n").unwrap();
    run_git(root, &["add", "src/main.rs"]);

    let start = run_ok(
        root,
        &[
            "ledger",
            "start",
            "src/main.rs",
            "--category",
            "FEATURE",
            "--message",
            "add main",
        ],
    );
    let tx_id = extract_tx_id(&start);

    let output = run_ok(
        root,
        &[
            "ledger",
            "commit",
            &tx_id,
            "--summary",
            "Add main",
            "--reason",
            "Exercise git integration",
            "--with-git",
            "--no-signoff",
        ],
    );
    assert!(output.contains("Transaction committed."));
    assert!(output.contains("Git commit created."));

    let log = run_git(root, &["log", "-1", "--pretty=%B"]);
    assert!(log.contains("[FEATURE] Add main"));
    assert!(log.contains(&format!("Ledger: {tx_id}")));
}

#[test]
fn ledger_commit_with_git_dry_run_skips_git_commit() {
    let dir = setup_repo();
    let root = dir.path();
    let entity = root.join("src").join("lib.rs");
    std::fs::create_dir_all(entity.parent().unwrap()).unwrap();
    std::fs::write(&entity, "pub fn value() -> i32 { 1 }\n").unwrap();
    run_git(root, &["add", "src/lib.rs"]);

    let start = run_ok(
        root,
        &[
            "ledger",
            "start",
            "src/lib.rs",
            "--category",
            "FEATURE",
            "--message",
            "add lib",
        ],
    );
    let tx_id = extract_tx_id(&start);

    let output = run_ok(
        root,
        &[
            "ledger",
            "commit",
            &tx_id,
            "--summary",
            "Add lib",
            "--reason",
            "Exercise dry run",
            "--with-git",
            "--dry-run",
            "--git-message",
            "custom dry run message",
        ],
    );
    assert!(output.contains("Transaction committed."));
    assert!(output.contains("Dry run: git commit -m"));
    assert!(output.contains("custom dry run message"));

    let log_result = Command::new("git")
        .current_dir(root)
        .args(["log", "-1", "--pretty=%B"])
        .output()
        .expect("failed to execute git log");
    assert!(!log_result.status.success());
}

#[test]
fn ledger_commit_with_git_without_staged_files_keeps_ledger_commit() {
    let dir = setup_repo();
    let root = dir.path();
    let entity = root.join("src").join("empty.rs");
    std::fs::create_dir_all(entity.parent().unwrap()).unwrap();
    std::fs::write(&entity, "pub fn empty() {}\n").unwrap();

    let start = run_ok(
        root,
        &[
            "ledger",
            "start",
            "src/empty.rs",
            "--category",
            "FEATURE",
            "--message",
            "add empty",
        ],
    );
    let tx_id = extract_tx_id(&start);

    let (stdout, stderr) = run_ok_with_stderr(
        root,
        &[
            "ledger",
            "commit",
            &tx_id,
            "--summary",
            "Add empty",
            "--reason",
            "Exercise non-fatal git failure",
            "--with-git",
        ],
    );
    assert!(stdout.contains("Transaction committed."));
    assert!(stderr.contains("Git commit skipped because no files are staged"));

    let status = run_ok(root, &["ledger", "status", "--compact"]);
    assert!(status.contains("0") && status.contains("pending"));

    let log_result = Command::new("git")
        .current_dir(root)
        .args(["log", "-1", "--pretty=%B"])
        .output()
        .expect("failed to execute git log");
    assert!(!log_result.status.success());
}
