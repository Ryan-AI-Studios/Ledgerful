use crate::common::{DirGuard, run_cli, setup_git_repo};
use ledgerful::commands::doctor::execute_doctor;
use std::fs;
use std::process::Command;
use tempfile::tempdir;

#[test]
fn doctor_reports_system_health() {
    let tmp = tempdir().unwrap();

    Command::new("git")
        .arg("init")
        .current_dir(tmp.path())
        .output()
        .expect("Failed to run git init");

    let _guard = DirGuard::new(tmp.path());

    let result = execute_doctor(false, false, false);

    assert!(result.is_ok());
}

/// 0109: `doctor --json` stdout is pure schema-v1 JSON (no human banners).
#[test]
fn doctor_json_stdout_is_pure_schema_v1() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    fs::write(root.join("dummy.txt"), "content").unwrap();

    let (stdout, stderr, code) = run_cli(root, &["doctor", "--json"]);
    assert_eq!(
        code, 0,
        "doctor --json without block should exit 0; stderr={stderr}"
    );

    // Pure JSON: no human printers (finding *messages* may mention sccache/SCIP).
    assert!(
        !stdout.contains("Ledgerful Doctor"),
        "human banner leaked to stdout:\n{stdout}"
    );
    assert!(
        !stdout.contains("Optional Accelerators"),
        "optional section leaked:\n{stdout}"
    );
    assert!(
        !stdout.contains("Hint:"),
        "sccache/SCIP Hint line leaked:\n{stdout}"
    );
    assert!(
        !stdout.contains("GPU VRAM"),
        "VRAM section leaked:\n{stdout}"
    );
    assert!(
        !stdout.contains("Cold or CI builds may benefit")
            || stdout.contains("\"code\": \"sccache-hint\""),
        "sccache should appear only as structured finding, got:\n{stdout}"
    );
    // Human-only Hint: prefix must not appear (structured findings have no Hint:).
    assert!(!stdout.lines().any(|l| l.trim_start().starts_with("Hint:")));

    let v: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("stdout must be pure JSON");
    assert_eq!(v["schemaVersion"], 1);
    assert!(v["schemaVersion"].is_number(), "schemaVersion must be u32");
    assert!(v["readyForPublish"].is_boolean());
    assert!(v["summary"]["block"].is_number());
    assert!(v["summary"]["warn"].is_number());
    assert!(v["summary"]["info"].is_number());
    assert!(v["findings"].is_array());
    assert!(v.get("readyForPublishDefinition").is_none());
    if let Some(arr) = v["findings"].as_array() {
        for f in arr {
            assert!(f["code"].is_string());
            assert!(f["severity"].is_string());
            assert!(f["category"].is_string());
            assert!(f["message"].is_string());
        }
    }
    assert!(v["environment"]["workRoot"].is_string());
}

/// 0109: malformed config is a structured warn, not a hard abort (JSON still pure).
#[test]
fn doctor_malformed_config_emits_legacy_config_warn_json() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    fs::write(root.join("dummy.txt"), "content").unwrap();

    let state = root.join(".ledgerful");
    fs::create_dir_all(state.join("state")).unwrap();
    // Deliberately invalid TOML
    fs::write(state.join("config.toml"), "[[[not valid toml").unwrap();

    let (stdout, stderr, code) = run_cli(root, &["doctor", "--json"]);
    assert_eq!(
        code, 0,
        "malformed config is warn/migration, not block; exit should be 0; stderr={stderr}"
    );
    assert!(
        !stdout.contains("Ledgerful Doctor"),
        "human banner on malformed config path:\n{stdout}"
    );
    let v: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("stdout must still be pure JSON");
    assert_eq!(v["readyForPublish"], true);
    let findings = v["findings"].as_array().expect("findings array");
    assert!(
        findings.iter().any(|f| {
            f["code"] == "legacy-config" && f["severity"] == "warn" && f["category"] == "migration"
        }),
        "expected legacy-config warn finding: {v}"
    );
}

/// 0109 / 0074 residual: exit 1 when a block finding is present (enforce + intent never).
#[test]
fn doctor_exit_1_on_block_finding() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    fs::write(root.join("dummy.txt"), "content").unwrap();

    // Minimal state with enforce + intent.required=never → INTENT_NEVER_UNDER_ENFORCE block.
    let state = root.join(".ledgerful");
    fs::create_dir_all(state.join("state")).unwrap();
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
    .unwrap();

    let (stdout, stderr, code) = run_cli(root, &["doctor", "--json"]);
    assert_eq!(
        code, 1,
        "block finding must exit 1; stdout={stdout}\nstderr={stderr}"
    );
    let v: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("json still emitted on block path");
    assert_eq!(v["readyForPublish"], false);
    assert!(
        v["summary"]["block"].as_u64().unwrap_or(0) >= 1,
        "expected block count >= 1: {v}"
    );
}
