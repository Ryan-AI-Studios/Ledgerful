//! CLI fixtures for `ledgerful web status` (0328).
//!
//! `get_layout()` requires a git repo. `LEDGERFUL_STATE_DIR` (absolute)
//! is the state directory so PID files never land on the engine tree.

use crate::common::{DirGuard, TempEnv, non_interactive, setup_git_repo};
use serial_test::serial;
use std::fs;
use std::path::Path;
use std::process::Command;
use tempfile::tempdir;

const BINARY: &str = env!("CARGO_BIN_EXE_ledgerful");

fn run_web_status(cwd: &Path, state_dir: &Path, json: bool) -> std::process::Output {
    let mut cmd = Command::new(BINARY);
    cmd.args(["web", "status"]);
    if json {
        cmd.arg("--json");
    }
    cmd.current_dir(cwd)
        .env("LEDGERFUL_NON_INTERACTIVE", "1")
        .env(
            "LEDGERFUL_STATE_DIR",
            state_dir
                .to_str()
                .expect("LEDGERFUL_STATE_DIR must be UTF-8"),
        )
        .output()
        .expect("failed to run ledgerful web status")
}

fn pid_path(state_dir: &Path) -> std::path::PathBuf {
    state_dir.join("tmp").join("web.pid")
}

fn write_pid(state_dir: &Path, contents: &str) {
    let path = pid_path(state_dir);
    fs::create_dir_all(path.parent().expect("pid parent")).expect("create tmp");
    fs::write(&path, contents).expect("write web.pid");
}

fn fixture() -> (tempfile::TempDir, tempfile::TempDir, tempfile::TempDir) {
    let home = tempdir().expect("home");
    let work = tempdir().expect("work");
    let state = tempdir().expect("state");
    setup_git_repo(work.path());
    (home, work, state)
}

#[test]
#[serial(env, cwd)]
fn web_status_json_no_pid_file() {
    let _ni = non_interactive();
    let (home, work, state) = fixture();
    let _home = TempEnv::set("USERPROFILE", home.path().to_str().expect("utf8 home"));
    let _cwd = DirGuard::new(work.path());

    let first = run_web_status(work.path(), state.path(), true);
    assert!(
        first.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&first.stderr)
    );
    let stdout = String::from_utf8_lossy(&first.stdout);
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim()).expect("json");
    assert_eq!(parsed["schemaVersion"], 1);
    assert!(parsed.get("kind").is_none(), "no kind: {parsed}");
    assert_eq!(parsed["state"], "noPidFile");
    assert!(parsed.get("pid").is_none(), "pid omitted: {parsed}");
    assert_eq!(parsed["next"], "ledgerful web start");
    let pid_file = parsed["pidFile"].as_str().expect("pidFile");
    assert!(pid_file.contains("web.pid"), "pidFile={pid_file}");
    let state_utf = state.path().to_str().expect("utf8 state");
    assert!(
        pid_file
            .replace('\\', "/")
            .contains(&state_utf.replace('\\', "/")),
        "pidFile {pid_file} must live under temp state {state_utf}"
    );

    let second = run_web_status(work.path(), state.path(), true);
    assert_eq!(first.stdout, second.stdout, "two dumps identical");
    assert!(!stdout.contains('\n') || stdout.ends_with('\n'));
    assert_eq!(stdout.chars().filter(|c| *c == '\n').count(), 1);

    let human = run_web_status(work.path(), state.path(), false);
    let human_out = String::from_utf8_lossy(&human.stdout);
    assert!(human_out.contains("no PID file at"));
    assert!(human_out.contains("Next: ledgerful web start"));
}

#[test]
#[serial(env, cwd)]
fn web_status_json_stale_and_reused_and_invalid() {
    let _ni = non_interactive();
    let (home, work, state) = fixture();
    let _home = TempEnv::set("USERPROFILE", home.path().to_str().expect("utf8 home"));
    let _cwd = DirGuard::new(work.path());

    write_pid(state.path(), &u32::MAX.to_string());
    let stale = run_web_status(work.path(), state.path(), true);
    assert!(stale.status.success());
    let parsed: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&stale.stdout).trim()).unwrap();
    assert_eq!(parsed["state"], "stalePid");
    assert_eq!(parsed["pid"], u32::MAX);
    assert_eq!(parsed["next"], "ledgerful web start");
    let human = run_web_status(work.path(), state.path(), false);
    let human_out = String::from_utf8_lossy(&human.stdout);
    assert!(human_out.contains("stale PID"));
    assert!(human_out.contains("Next: ledgerful web start"));

    write_pid(state.path(), &std::process::id().to_string());
    let reused = run_web_status(work.path(), state.path(), true);
    assert!(reused.status.success());
    let parsed: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&reused.stdout).trim()).unwrap();
    assert_eq!(
        parsed["state"], "reusedPid",
        "test-runner PID must be reusedPid, not running: {parsed}"
    );
    assert_eq!(parsed["pid"], std::process::id());
    let human = run_web_status(work.path(), state.path(), false);
    let human_out = String::from_utf8_lossy(&human.stdout);
    assert!(human_out.contains("reused PID"));

    write_pid(state.path(), "not-a-pid");
    let invalid = run_web_status(work.path(), state.path(), true);
    assert!(!invalid.status.success(), "invalid PID must fail closed");
    assert!(
        invalid.stdout.is_empty(),
        "no partial JSON: {:?}",
        String::from_utf8_lossy(&invalid.stdout)
    );
    assert!(
        pid_path(state.path()).exists(),
        "status must not remove the file"
    );
}
