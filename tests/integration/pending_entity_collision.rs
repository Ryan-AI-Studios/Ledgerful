//! 0223 — pending entity collision on `ledger start` (hermetic; not the live engine TX).

use crate::common::{git_add_and_commit, non_interactive, run_cli, setup_git_repo};
use serial_test::serial;
use std::fs;
use tempfile::tempdir;

#[test]
#[serial(env)]
fn ledger_start_collision_exits_2_owner_self_collision_and_force_starts() {
    let _ni = non_interactive();
    let tmp = tempdir().expect("tempdir");
    let root = tmp.path();
    setup_git_repo(root);
    fs::write(root.join("README.md"), "init\n").expect("readme");
    git_add_and_commit(root, "init");

    let (out, err, code) = run_cli(root, &["init", "--force"]);
    assert_eq!(code, 0, "init failed: stdout={out} stderr={err}");

    let (out, err, code) = run_cli(
        root,
        &[
            "ledger",
            "start",
            "crates/dedupe-chrome",
            "--category",
            "FEATURE",
            "--message",
            "chrome work",
        ],
    );
    assert_eq!(code, 0, "first start failed: stdout={out} stderr={err}");

    let (status_out, status_err, status_code) = run_cli(root, &["ledger", "status", "--json"]);
    assert_eq!(
        status_code, 0,
        "status --json failed: stdout={status_out} stderr={status_err}"
    );
    assert!(
        !status_out.contains("collisions"),
        "status --json v1 must not grow collisions[]; stdout={status_out}"
    );

    fs::create_dir_all(root.join("crates").join("dedupe-chrome")).expect("mkdir chrome");
    fs::write(
        root.join("crates").join("dedupe-chrome").join("foo.rs"),
        "fn x() {}\n",
    )
    .expect("dirty chrome");

    let (out, err, code) = run_cli(
        root,
        &[
            "ledger",
            "start",
            "crates/other",
            "--category",
            "FEATURE",
            "--message",
            "adjacent",
        ],
    );
    assert_eq!(
        code, 2,
        "owner self-collision must exit 2; stdout={out} stderr={err}"
    );
    let combined = format!("{out}{err}");
    assert!(
        combined.contains("[Ledgerful] Collision: pending tx")
            && combined.contains("owns crates/dedupe-chrome"),
        "grep line missing: {combined}"
    );
    assert!(
        combined.contains("category: FEATURE") && combined.contains("message: chrome work"),
        "collision report must include category and message: {combined}"
    );

    let (out, err, code) = run_cli(
        root,
        &[
            "ledger",
            "start",
            "crates/other",
            "--category",
            "FEATURE",
            "--message",
            "adjacent",
            "--force",
        ],
    );
    assert_eq!(code, 0, "--force must start: stdout={out} stderr={err}");
    assert!(
        out.contains("Transaction started"),
        "force start stdout: {out}"
    );
}

#[test]
#[serial(env)]
fn ledger_start_disjoint_dirty_and_entity_succeeds() {
    let _ni = non_interactive();
    let tmp = tempdir().expect("tempdir");
    let root = tmp.path();
    setup_git_repo(root);
    fs::write(root.join("README.md"), "init\n").expect("readme");
    git_add_and_commit(root, "init");

    let (out, err, code) = run_cli(root, &["init", "--force"]);
    assert_eq!(code, 0, "init failed: stdout={out} stderr={err}");

    let (out, err, code) = run_cli(
        root,
        &[
            "ledger",
            "start",
            "crates/dedupe-chrome",
            "--category",
            "FEATURE",
            "--message",
            "chrome work",
        ],
    );
    assert_eq!(code, 0, "first start failed: stdout={out} stderr={err}");

    fs::create_dir_all(root.join("crates").join("other").join("src")).expect("mkdir other");
    fs::write(
        root.join("crates").join("other").join("src").join("lib.rs"),
        "fn y() {}\n",
    )
    .expect("dirty other");

    let (out, err, code) = run_cli(
        root,
        &[
            "ledger",
            "start",
            "crates/other",
            "--category",
            "FEATURE",
            "--message",
            "other crate",
        ],
    );
    assert_eq!(
        code, 0,
        "disjoint dirty+entity must start: stdout={out} stderr={err}"
    );
    assert!(
        out.contains("Transaction started"),
        "disjoint start stdout: {out}"
    );
}
