#![allow(non_snake_case)]

use camino::Utf8Path;
use std::fs;
use std::time::SystemTime;
use tempfile::tempdir;

use crate::common::{run_cli, setup_git_repo};

fn stale_verify_block() -> String {
    "\
# ledgerful-verify-gate: fast scoped verification (pre-push only)
if command -v ledgerful &>/dev/null; then
    if ! ledgerful verify --scope fast; then
        echo \"[Ledgerful] Push blocked by verification failure.\"
        echo \"[Ledgerful] Fix the issues or bypass with: git push --no-verify\"
        exit 1
    fi
fi
"
    .to_string()
}

fn current_verify_block() -> String {
    ledgerful::commands::hook_template::verify_gate_block("git push --no-verify")
}

fn current_ledger_block() -> String {
    ledgerful::commands::hook_template::ledger_gate_block("git push --no-verify")
}

fn write_pre_push(root: &Utf8Path, body: &str) {
    let hooks = root.join(".git").join("hooks");
    fs::create_dir_all(&hooks).expect("hooks dir");
    fs::write(
        hooks.join("pre-push"),
        format!("#!/usr/bin/env bash\n{body}"),
    )
    .expect("pre-push");
}

fn assert_isolated_preview(stdout: &str, stderr: &str, code: i32) {
    assert_eq!(
        code, 0,
        "preview must exit 0; stdout={stdout}; stderr={stderr}"
    );
    assert!(
        stdout.contains("Dry-run completed. No hook files were modified."),
        "footer missing: {stdout}"
    );
    assert!(
        !stdout.contains("Ledgerful Doctor"),
        "health banner leaked: {stdout}"
    );
    assert!(
        !stdout.contains("Index Health"),
        "index health leaked: {stdout}"
    );
}

fn parse_preview_json(stdout: &str, stderr: &str, code: i32) -> serde_json::Value {
    assert_eq!(
        code, 0,
        "json preview must exit 0; stdout={stdout}; stderr={stderr}"
    );
    assert!(
        stdout.ends_with('\n'),
        "pretty JSON must end with one trailing newline: {stdout:?}"
    );
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
        panic!("entire stdout must parse as one JSON object: {e}; stdout={stdout}");
    });
    assert!(parsed.is_object(), "expected JSON object, got {parsed}");
    assert_eq!(parsed["schemaVersion"], 1, "{parsed}");
    assert_eq!(parsed["kind"], "hookRefreshPreview", "{parsed}");
    assert_eq!(parsed["executed"], false, "{parsed}");
    assert_eq!(parsed["dryRun"], true, "{parsed}");
    assert!(parsed.get("ok").is_none(), "{parsed}");
    assert!(parsed.get("emptyDiagnostics").is_none(), "{parsed}");
    assert!(parsed.get("findings").is_none(), "{parsed}");
    assert!(parsed.get("summary").is_none(), "{parsed}");
    let dumped = serde_json::to_string(&parsed).expect("dump");
    assert!(
        !dumped.contains('\\'),
        "JSON path/hooksDir must not contain backslash: {dumped}"
    );
    parsed
}

#[test]
fn doctor_hook_refresh_dry_run__stale__isolated_preview_no_writes() {
    let tmp = tempdir().unwrap();
    setup_git_repo(tmp.path());
    let root = Utf8Path::from_path(tmp.path()).expect("utf8");
    write_pre_push(root, &stale_verify_block());
    let hook = root.join(".git").join("hooks").join("pre-push");
    let before = fs::read_to_string(&hook).expect("before");

    let (stdout, stderr, code) =
        run_cli(tmp.path(), &["doctor", "--apply-hook-refresh", "--dry-run"]);
    assert_isolated_preview(&stdout, &stderr, code);
    assert!(
        stdout.contains("DRY-RUN Would refresh"),
        "would-refresh missing: {stdout}"
    );
    assert!(
        stdout.contains("pre-push:verify-gate (.git/hooks/pre-push)"),
        "{stdout}"
    );
    assert_eq!(fs::read_to_string(&hook).expect("after"), before);
    assert!(
        !root.join(".ledgerful").exists(),
        "dry-run must not create .ledgerful"
    );
}

#[test]
fn doctor_hook_refresh_dry_run__already_current__not_noop() {
    let tmp = tempdir().unwrap();
    setup_git_repo(tmp.path());
    let root = Utf8Path::from_path(tmp.path()).expect("utf8");
    write_pre_push(
        root,
        &format!("{}\n{}", current_ledger_block(), current_verify_block()),
    );

    let (stdout, stderr, code) =
        run_cli(tmp.path(), &["doctor", "--apply-hook-refresh", "--dry-run"]);
    assert_isolated_preview(&stdout, &stderr, code);
    assert!(
        stdout.contains("Already current:"),
        "already-current rows required: {stdout}"
    );
    assert!(
        !stdout.contains("No product hook templates to refresh"),
        "already-current must not collapse to noop: {stdout}"
    );
}

#[test]
fn doctor_hook_refresh_json_dry_run__pretty_envelope() {
    let tmp = tempdir().unwrap();
    setup_git_repo(tmp.path());
    let root = Utf8Path::from_path(tmp.path()).expect("utf8");
    write_pre_push(root, &stale_verify_block());

    let (stdout, stderr, code) = run_cli(
        tmp.path(),
        &["doctor", "--json", "--apply-hook-refresh", "--dry-run"],
    );
    let parsed = parse_preview_json(&stdout, &stderr, code);
    assert_eq!(parsed["hooksDir"], ".git/hooks");
    let labels: Vec<&str> = parsed["wouldRefresh"]
        .as_array()
        .expect("wouldRefresh")
        .iter()
        .filter_map(|row| row["label"].as_str())
        .collect();
    assert!(
        labels.iter().any(|l| l.contains("verify-gate")),
        "wouldRefresh={parsed}"
    );
}

#[test]
fn doctor_hook_refresh_json_without_dry_run__still_rejected() {
    let tmp = tempdir().unwrap();
    setup_git_repo(tmp.path());
    let (stdout, stderr, code) = run_cli(tmp.path(), &["doctor", "--json", "--apply-hook-refresh"]);
    assert_ne!(code, 0, "must reject; stdout={stdout}; stderr={stderr}");
    let combined = format!("{stdout}{stderr}");
    assert!(
        combined.contains("doctor --json cannot be combined with --apply-hook-refresh"),
        "{combined}"
    );
}

#[test]
fn doctor_hook_refresh_dry_run__missing_hooks__warn_noop() {
    let tmp = tempdir().unwrap();
    setup_git_repo(tmp.path());
    let hooks = tmp.path().join(".git").join("hooks");
    if hooks.exists() {
        fs::remove_dir_all(&hooks).expect("remove hooks");
    }

    let (stdout, stderr, code) =
        run_cli(tmp.path(), &["doctor", "--apply-hook-refresh", "--dry-run"]);
    assert_isolated_preview(&stdout, &stderr, code);
    assert!(stdout.contains("WARN:"), "{stdout}");
    assert!(
        stdout.contains("No product hook templates to refresh"),
        "{stdout}"
    );
}

#[test]
fn doctor_hook_refresh_dry_run__husky__refused() {
    let tmp = tempdir().unwrap();
    setup_git_repo(tmp.path());
    fs::create_dir_all(tmp.path().join(".husky")).expect("husky");

    let (stdout, stderr, code) =
        run_cli(tmp.path(), &["doctor", "--apply-hook-refresh", "--dry-run"]);
    assert_isolated_preview(&stdout, &stderr, code);
    assert!(stdout.contains("REFUSED:"), "{stdout}");
    assert!(stdout.contains("husky"), "{stdout}");
}

#[test]
fn doctor_hook_refresh_dry_run__seeded_results_untouched() {
    let tmp = tempdir().unwrap();
    setup_git_repo(tmp.path());
    let root = Utf8Path::from_path(tmp.path()).expect("utf8");
    write_pre_push(root, &stale_verify_block());
    let state = root.join(".ledgerful").join("state");
    fs::create_dir_all(&state).expect("state");
    let results = state.join("doctor-results.json");
    fs::write(&results, "{\"seed\":true}\n").expect("seed");
    let before = fs::read(&results).expect("read seed");
    let mtime_before = fs::metadata(&results)
        .and_then(|m| m.modified())
        .unwrap_or(SystemTime::UNIX_EPOCH);

    let (stdout, stderr, code) =
        run_cli(tmp.path(), &["doctor", "--apply-hook-refresh", "--dry-run"]);
    assert_isolated_preview(&stdout, &stderr, code);
    let after = fs::read(&results).expect("read after");
    assert_eq!(after, before, "doctor-results.json must not be rewritten");
    let mtime_after = fs::metadata(&results)
        .and_then(|m| m.modified())
        .unwrap_or(SystemTime::UNIX_EPOCH);
    assert_eq!(
        mtime_after, mtime_before,
        "doctor-results.json mtime must be unchanged"
    );
}

#[test]
fn doctor_hook_refresh_dry_run__exit_0_when_health_would_block() {
    let tmp = tempdir().unwrap();
    setup_git_repo(tmp.path());
    let root = Utf8Path::from_path(tmp.path()).expect("utf8");
    write_pre_push(root, &stale_verify_block());
    let state = root.join(".ledgerful");
    fs::create_dir_all(state.join("state")).expect("state");
    fs::write(
        state.join("config.toml"),
        r#"
[gate]
mode = "enforce"

[intent]
required = "never"
require_signing = true
"#,
    )
    .expect("config");

    let (health_out, health_err, health_code) = run_cli(tmp.path(), &["doctor", "--json"]);
    assert_eq!(
        health_code, 1,
        "health path must still block; stdout={health_out}; stderr={health_err}"
    );

    let (stdout, stderr, code) =
        run_cli(tmp.path(), &["doctor", "--apply-hook-refresh", "--dry-run"]);
    assert_isolated_preview(&stdout, &stderr, code);
}

#[test]
fn doctor_hook_refresh_write_path__still_continues_health() {
    let tmp = tempdir().unwrap();
    setup_git_repo(tmp.path());
    let root = Utf8Path::from_path(tmp.path()).expect("utf8");
    write_pre_push(root, &stale_verify_block());

    let (stdout, stderr, _code) = run_cli(tmp.path(), &["doctor", "--apply-hook-refresh"]);
    assert!(
        !stdout.contains("Dry-run completed. No hook files were modified."),
        "write path must not print dry-run footer: {stdout}"
    );
    assert!(
        stdout.contains("Ledgerful Doctor") || stdout.contains("Doctor:"),
        "write path must continue into health; stdout={stdout}; stderr={stderr}"
    );
}
