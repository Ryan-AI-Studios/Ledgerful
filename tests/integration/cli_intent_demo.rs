//! Non-TTY `intent demo` refuse (0429).

use crate::common::{run_cli_env, setup_git_repo};
use std::fs;
use tempfile::tempdir;

#[test]
fn intent_demo_non_interactive_refuses() {
    let tmp = tempdir().expect("tempdir");
    setup_git_repo(tmp.path());
    fs::write(tmp.path().join("dummy.txt"), "content").expect("write dummy");

    let (stdout, stderr, code) = run_cli_env(
        tmp.path(),
        &["intent", "demo"],
        &[("LEDGERFUL_NON_INTERACTIVE", "1")],
    );
    assert_ne!(code, 0, "non-interactive intent demo must fail: {stderr}");
    assert!(
        stdout.is_empty(),
        "refuse must stay on stderr, stdout={stdout}"
    );
    assert!(
        stderr.contains("Cannot launch intent demo: terminal is non-interactive or not a TTY"),
        "missing refuse: {stderr}"
    );
    assert!(
        stderr.contains("Next: run `ledgerful intent demo` in an interactive terminal"),
        "missing Next: {stderr}"
    );
    assert!(
        stderr.contains("This command is a TUI demo, not a non-interactive workflow"),
        "shortened Next: {stderr}"
    );
}
