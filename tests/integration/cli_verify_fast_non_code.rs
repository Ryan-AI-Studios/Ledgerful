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

fn write_suite_budget(root: &Path, seconds: u64) {
    let state = root.join(".ledgerful");
    fs::create_dir_all(&state).unwrap();
    fs::write(
        state.join("config.toml"),
        format!("[verify]\nsuite_timeout_secs = {seconds}\n"),
    )
    .unwrap();
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
fn test_default_verify_dry_run_first_line_names_fast_scope() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    init_committed_rust_repo(root);

    let out = spawn_verify(root, &["verify", "--dry-run"]);
    let stdout = stdout_text(&out);
    assert_eq!(
        first_nonempty_line(&stdout),
        "scope: fast",
        "omitted --scope --dry-run first line: {stdout:?}"
    );
    assert!(
        !stdout.contains("CLI default"),
        "must not claim CLI default: {stdout:?}"
    );
    assert!(
        !stdout.contains("nextest") && !stdout.to_ascii_lowercase().contains("cargo test"),
        "clean omitted dry-run must not schedule nextest/cargo test: {stdout:?}"
    );
    assert!(
        stdout.contains("cargo fmt") && stdout.contains("clippy"),
        "fmt+clippy must be present: {stdout:?}"
    );
    assert!(
        !stdout.contains("Predicted Impacts"),
        "fully-clean auto-policy dry-run skips predict: {stdout:?}"
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
    assert!(
        stdout.contains("cargo fmt") && stdout.contains("clippy"),
        "full dry-run must include fmt+clippy: {stdout:?}"
    );
    assert!(
        stdout.contains("nextest") && stdout.to_ascii_lowercase().contains("cargo test"),
        "full dry-run must list nextest and doctest cargo test: {stdout:?}"
    );
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
        "scope: fast",
        "omitted-scope manual command --dry-run first line: {stdout:?}"
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

    let out_full = spawn_verify(
        root,
        &["verify", "--scope", "full", "echo hello", "--dry-run"],
    );
    let stdout_full = stdout_text(&out_full);
    let stderr_full = String::from_utf8_lossy(&out_full.stderr);
    assert!(
        out_full.status.success(),
        "full manual command dry-run must exit 0; stdout={stdout_full:?} stderr={stderr_full:?}"
    );
    assert_eq!(
        first_nonempty_line(&stdout_full),
        "scope: full (pre-push uses --scope fast)",
        "explicit --scope full manual command --dry-run: {stdout_full:?}"
    );
    assert!(!stdout_full.contains("CLI default"));
}

#[test]
fn test_verify_json_dry_run_emits_verify_dry_run_kind() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    init_committed_rust_repo(root);

    let out = spawn_verify(root, &["verify", "--json", "--dry-run"]);
    let stdout = stdout_text(&out);
    let v: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("dry-run JSON is now allowed");
    assert_eq!(v["kind"], "verifyDryRun");
    assert_eq!(v["executed"], false);
    assert!(v.get("ok").is_none());
    assert!(v["gitAvailable"].is_boolean());
}

#[test]
fn test_fast_dry_run_timeout_caps_json_steps() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    init_committed_rust_repo(root);

    let capped = spawn_verify(
        root,
        &[
            "verify",
            "--scope",
            "fast",
            "--timeout",
            "25",
            "--json",
            "--dry-run",
        ],
    );
    let capped_stdout = stdout_text(&capped);
    let capped_stderr = String::from_utf8_lossy(&capped.stderr);
    assert!(
        capped.status.success(),
        "capped dry-run must exit 0; stdout={capped_stdout:?} stderr={capped_stderr:?}"
    );
    let v: serde_json::Value =
        serde_json::from_str(capped_stdout.trim()).expect("capped dry-run JSON");
    assert_eq!(v["executed"], false);
    assert!(v.get("ok").is_none(), "dry-run must not emit ok: {v}");
    let steps = v["steps"].as_array().expect("steps");
    assert_eq!(steps.len(), 2, "{v}");
    assert_eq!(steps[0]["timeoutSecs"], 25);
    assert_eq!(steps[1]["timeoutSecs"], 25);
    let fmt_cmd = steps[0]["command"].as_str().unwrap_or("");
    let clippy_cmd = steps[1]["command"].as_str().unwrap_or("");
    assert!(fmt_cmd.contains("cargo fmt"), "{fmt_cmd}");
    assert!(clippy_cmd.contains("clippy"), "{clippy_cmd}");

    let plain = spawn_verify(root, &["verify", "--scope", "fast", "--json", "--dry-run"]);
    let plain_stdout = stdout_text(&plain);
    let plain_stderr = String::from_utf8_lossy(&plain.stderr);
    assert!(
        plain.status.success(),
        "default dry-run must exit 0; stdout={plain_stdout:?} stderr={plain_stderr:?}"
    );
    let plain_v: serde_json::Value =
        serde_json::from_str(plain_stdout.trim()).expect("default dry-run JSON");
    let plain_steps = plain_v["steps"].as_array().expect("steps");
    assert_eq!(plain_steps.len(), 2, "{plain_v}");
    assert_eq!(plain_steps[0]["timeoutSecs"], 60);
    assert_eq!(plain_steps[1]["timeoutSecs"], 400);
    assert!(
        plain_steps[0]["command"]
            .as_str()
            .unwrap_or("")
            .contains("cargo fmt"),
        "{plain_v}"
    );
    assert!(
        plain_steps[1]["command"]
            .as_str()
            .unwrap_or("")
            .contains("clippy"),
        "{plain_v}"
    );

    let human = spawn_verify(
        root,
        &["verify", "--scope", "fast", "--timeout", "25", "--dry-run"],
    );
    let human_stdout = stdout_text(&human);
    let human_stderr = String::from_utf8_lossy(&human.stderr);
    assert!(
        human.status.success(),
        "human dry-run must exit 0; stdout={human_stdout:?} stderr={human_stderr:?}"
    );
    assert!(
        human_stdout.contains("(timeout: 25s)"),
        "human dry-run must show capped timeout: {human_stdout:?}"
    );
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

#[test]
fn suite_budget_survives_omitted_timeout_and_explicit_cap_still_mins() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    init_committed_rust_repo(root);
    write_suite_budget(root, 900);

    let open = spawn_verify(root, &["verify", "--scope", "full", "--json", "--dry-run"]);
    let open_stdout = stdout_text(&open);
    let open_stderr = String::from_utf8_lossy(&open.stderr);
    assert!(
        open.status.success(),
        "omitted timeout dry-run must exit 0; stdout={open_stdout:?} stderr={open_stderr:?}"
    );
    let open_v: serde_json::Value =
        serde_json::from_str(open_stdout.trim()).expect("omitted dry-run JSON");
    assert_eq!(open_v["schemaVersion"], 1);
    let steps = open_v["steps"].as_array().expect("steps");
    let nextest = steps
        .iter()
        .find(|step| {
            step["command"]
                .as_str()
                .unwrap_or("")
                .contains("cargo nextest")
                || step["command"]
                    .as_str()
                    .unwrap_or("")
                    .starts_with("cargo test ")
        })
        .expect("test step");
    assert_eq!(nextest["timeoutSecs"], 900, "{open_v}");
    assert_eq!(nextest["budgetSource"], "suite", "{open_v}");
    let fmt = steps
        .iter()
        .find(|step| step["command"].as_str().unwrap_or("").contains("cargo fmt"))
        .expect("fmt step");
    assert_eq!(fmt["timeoutSecs"], 400, "auto full fmt stays 400: {open_v}");
    assert_eq!(fmt["budgetSource"], "format");

    let capped = spawn_verify(
        root,
        &[
            "verify",
            "--scope",
            "full",
            "--timeout",
            "25",
            "--json",
            "--dry-run",
        ],
    );
    let capped_stdout = stdout_text(&capped);
    assert!(capped.status.success(), "{capped_stdout:?}");
    let capped_v: serde_json::Value =
        serde_json::from_str(capped_stdout.trim()).expect("capped dry-run JSON");
    let capped_test = capped_v["steps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|step| step["budgetSource"] == "suite")
        .expect("suite step");
    assert_eq!(capped_test["timeoutSecs"], 25, "{capped_v}");
}
