use crate::common::{git_add_and_commit, non_interactive, run_cli, setup_git_repo};
use ledgerful::commands::init::execute_init;
use serde_json::Value;
use serial_test::serial;
use std::fs;
use std::path::Path;
use tempfile::tempdir;

fn init_repo(root: &Path) {
    setup_git_repo(root);
    fs::write(root.join("dummy.txt"), "content").unwrap();
    git_add_and_commit(root, "initial");
    let _guard = crate::common::DirGuard::new(root);
    execute_init(false, false).unwrap();
}

#[test]
#[serial(cwd)]
fn ledger_validator_list_empty_human_names_zero_and_next() {
    let _ni = non_interactive();
    let tmp = tempdir().unwrap();
    init_repo(tmp.path());

    let (out, err, code) = run_cli(tmp.path(), &["ledger", "validator", "list"]);
    assert_eq!(code, 0, "empty list should exit 0: {err}");
    assert!(out.contains("0 registered"), "missing 0 registered: {out}");
    assert!(
        out.to_lowercase().contains("none run at commit"),
        "missing consequence: {out}"
    );
    assert!(
        out.contains("register validator --help"),
        "missing next --help: {out}"
    );
    assert!(
        !out.contains("Executable"),
        "empty list must not print table header: {out}"
    );
    assert!(
        !out.contains("All enabled validators are healthy"),
        "list must not claim doctor health pass: {out}"
    );
}

#[test]
#[serial(cwd)]
fn ledger_validator_list_empty_json_is_bare_array() {
    let _ni = non_interactive();
    let tmp = tempdir().unwrap();
    init_repo(tmp.path());

    let (out, err, code) = run_cli(tmp.path(), &["ledger", "validator", "list", "--json"]);
    assert_eq!(code, 0, "empty list --json should exit 0: {err}");
    assert!(
        out.trim_start().starts_with('['),
        "json stdout must start with '[': {out}"
    );
    let v: Value = serde_json::from_str(out.trim()).expect("list --json parse");
    assert_eq!(v.as_array().map(|a| a.len()), Some(0));
    assert!(
        !out.contains("Registered Commit Validators"),
        "json must not print human title: {out}"
    );
}

#[test]
#[serial(cwd)]
fn ledger_validator_list_populated_shows_args_dash_and_counts() {
    let _ni = non_interactive();
    let tmp = tempdir().unwrap();
    init_repo(tmp.path());

    let (reg_out, reg_err, reg_code) = run_cli(
        tmp.path(),
        &[
            "ledger",
            "register",
            "validator",
            "--category",
            "ALL",
            "-x",
            "echo",
            "empty-check",
        ],
    );
    assert_eq!(
        reg_code, 0,
        "register validator failed: {reg_err} {reg_out}"
    );
    assert!(
        reg_out.contains("Next: ledgerful ledger validator list"),
        "register validator success must name list: {reg_out}"
    );

    let (out, err, code) = run_cli(tmp.path(), &["ledger", "validator", "list"]);
    assert_eq!(code, 0, "populated list should exit 0: {err}");
    assert!(out.contains("empty-check"), "missing name: {out}");
    assert!(out.contains("echo"), "missing executable: {out}");
    assert!(out.contains("Args"), "missing Args header: {out}");
    let data_line = out
        .lines()
        .find(|l| l.contains("empty-check") && l.contains("echo"))
        .unwrap_or("");
    assert!(
        data_line.split_whitespace().any(|tok| tok == "-"),
        "Args cell must be literal '-' on the name/executable row: {out}"
    );
    assert!(
        out.contains("1 registered") && out.contains("1 enabled"),
        "missing footer counts: {out}"
    );

    let (json_out, json_err, json_code) =
        run_cli(tmp.path(), &["ledger", "validator", "list", "--json"]);
    assert_eq!(json_code, 0, "populated --json failed: {json_err}");
    let v: Value = serde_json::from_str(json_out.trim()).expect("populated json");
    let arr = v.as_array().expect("bare array");
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["name"], "empty-check");
    assert_eq!(arr[0]["executable"], "echo");
    assert_eq!(arr[0]["enabled"], true);
}

#[test]
#[serial(cwd)]
fn ledger_validator_list_disable_then_shows_enabled_false() {
    let _ni = non_interactive();
    let tmp = tempdir().unwrap();
    init_repo(tmp.path());

    let (reg_out, reg_err, reg_code) = run_cli(
        tmp.path(),
        &[
            "ledger",
            "register",
            "validator",
            "--category",
            "ALL",
            "-x",
            "echo",
            "empty-check",
        ],
    );
    assert_eq!(
        reg_code, 0,
        "register validator failed: {reg_err} {reg_out}"
    );
    assert!(
        reg_out.contains("Next: ledgerful ledger validator list"),
        "register validator success must name list: {reg_out}"
    );

    let (dis_out, dis_err, dis_code) = run_cli(
        tmp.path(),
        &["ledger", "validator", "disable", "empty-check"],
    );
    assert_eq!(
        dis_code, 0,
        "disable must succeed on writable open: {dis_err} {dis_out}"
    );

    let (out, err, code) = run_cli(tmp.path(), &["ledger", "validator", "list"]);
    assert_eq!(code, 0, "list after disable failed: {err}");
    assert!(
        out.contains("no") || out.contains("DISABLED"),
        "human should show disabled: {out}"
    );
    assert!(
        out.contains("1 registered") && out.contains("0 enabled"),
        "footer should be 1 registered / 0 enabled: {out}"
    );

    let (json_out, json_err, json_code) =
        run_cli(tmp.path(), &["ledger", "validator", "list", "--json"]);
    assert_eq!(json_code, 0, "json after disable failed: {json_err}");
    let v: Value = serde_json::from_str(json_out.trim()).expect("json");
    assert_eq!(v[0]["enabled"], false);
}
