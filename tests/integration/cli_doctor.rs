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

    let result = execute_doctor(false, false, false, false, false);

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
    // 0137 B5b: currency fields always present (schemaVersion stays 1).
    assert!(
        v["environment"]["binaryVersion"].is_string(),
        "environment.binaryVersion required: {v}"
    );
    assert!(
        v["environment"]["buildSha"].is_string(),
        "environment.buildSha required: {v}"
    );
}

/// 0137 B5: short `-V` is package version only; long `--version` may include SHA.
#[test]
fn version_flags_short_without_sha_long_may_include_embed() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);

    let (short_out, short_err, short_code) = run_cli(root, &["-V"]);
    assert_eq!(
        short_code, 0,
        "-V should exit 0; stderr={short_err} stdout={short_out}"
    );
    let short = short_out.trim();
    assert!(
        short.starts_with("ledgerful "),
        "short -V should be clap package version: {short:?}"
    );
    assert!(
        !short.contains('('),
        "short -V must not include SHA parentheses: {short:?}"
    );

    let (long_out, long_err, long_code) = run_cli(root, &["--version"]);
    assert_eq!(
        long_code, 0,
        "--version should exit 0; stderr={long_err} stdout={long_out}"
    );
    let long = long_out.trim();
    assert!(
        long.starts_with("ledgerful "),
        "long --version should start with binary name: {long:?}"
    );
    let sha = env!("LEDGERFUL_GIT_SHA");
    if sha != "unknown" && !sha.is_empty() {
        assert!(
            long.contains(sha),
            "long --version should include embed SHA {sha} when known: {long:?}"
        );
    } else {
        assert!(
            !long.contains('('),
            "unknown embed: long --version should match short form: {long:?}"
        );
    }
}

/// 0137: non-engine temp layout must not emit `binary-behind-tree`.
#[test]
fn doctor_non_engine_repo_has_no_binary_behind_tree() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    fs::write(root.join("dummy.txt"), "content").unwrap();
    // Consumer-like Cargo.toml (not package name ledgerful / no CLI layout).
    fs::write(
        root.join("Cargo.toml"),
        r#"[package]
name = "consumer-app"
version = "1.0.0"
"#,
    )
    .unwrap();

    let (stdout, stderr, code) = run_cli(root, &["doctor", "--json"]);
    assert_eq!(
        code, 0,
        "non-engine doctor --json should exit 0; stderr={stderr}"
    );
    let v: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("stdout must be pure JSON");
    let findings = v["findings"].as_array().expect("findings array");
    assert!(
        !findings.iter().any(|f| f["code"] == "binary-behind-tree"),
        "consumer layout must not emit binary-behind-tree: {v}"
    );
}

/// 0205 T0+T12: consumer repos skip GitHub Latest (zero HTTP, no 0205 codes).
#[test]
fn doctor_non_engine_repo_has_no_binary_latest_codes() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    fs::write(root.join("dummy.txt"), "content").unwrap();
    fs::write(
        root.join("Cargo.toml"),
        r#"[package]
name = "consumer-app"
version = "1.0.0"
"#,
    )
    .unwrap();

    let (stdout, stderr, code) = run_cli(root, &["doctor", "--json"]);
    assert_eq!(
        code, 0,
        "non-engine doctor --json should exit 0; stderr={stderr}"
    );
    let v: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("stdout must be pure JSON");
    let findings = v["findings"].as_array().expect("findings array");
    assert!(
        !findings.iter().any(|f| f["code"] == "binary-behind-latest"),
        "consumer layout must not emit binary-behind-latest: {v}"
    );
    assert!(
        !findings
            .iter()
            .any(|f| f["code"] == "binary-ahead-of-latest"),
        "consumer layout must not emit binary-ahead-of-latest: {v}"
    );
    let gl = &v["environment"]["githubLatest"];
    assert!(gl.is_object(), "githubLatest always present: {v}");
    assert_eq!(gl["status"], "skipped");
    assert!(gl.get("tag").is_none(), "skipped omits tag: {gl}");
    assert!(gl.get("sha").is_none(), "skipped omits sha: {gl}");
    assert!(gl.get("running").is_none(), "skipped omits running: {gl}");
    assert!(gl.get("worktree").is_none(), "skipped omits worktree: {gl}");
}

/// 0205 F5: `environment.githubLatest` is always an object with string `status`.
#[test]
fn doctor_json_environment_github_latest_always_object() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    fs::write(root.join("dummy.txt"), "content").unwrap();
    fs::write(
        root.join("Cargo.toml"),
        r#"[package]
name = "consumer-app"
version = "1.0.0"
"#,
    )
    .unwrap();

    let (stdout, stderr, code) = run_cli(root, &["doctor", "--json"]);
    assert_eq!(code, 0, "doctor --json should exit 0; stderr={stderr}");
    let v: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("stdout must be pure JSON");
    assert_eq!(v["schemaVersion"], 1);
    let gl = &v["environment"]["githubLatest"];
    assert!(
        gl.is_object(),
        "environment.githubLatest must be an object: {v}"
    );
    assert!(
        gl["status"].is_string(),
        "githubLatest.status must be a string: {gl}"
    );
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

/// 0126: production empty path — init/ensure_state_dir yields 0 docs → search-empty.
#[test]
fn doctor_json_emits_search_empty_on_unindexed_repo() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    fs::write(root.join("dummy.txt"), "content").unwrap();

    // doctor ensures state dir + opens/creates Tantivy → 0 documents without index.
    let (stdout, stderr, code) = run_cli(root, &["doctor", "--json"]);
    assert_eq!(
        code, 0,
        "search-empty is warn not block; exit 0; stderr={stderr}"
    );
    let v: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("stdout must be pure JSON");
    assert_eq!(
        v["readyForPublish"], true,
        "warn must not block publish: {v}"
    );
    let findings = v["findings"].as_array().expect("findings array");
    let empty = findings
        .iter()
        .find(|f| f["code"] == "search-empty")
        .unwrap_or_else(|| panic!("expected search-empty finding: {v}"));
    assert_eq!(empty["severity"], "warn");
    assert_eq!(empty["category"], "index");
    let rem = empty["remediation"]
        .as_str()
        .expect("search-empty remediation Some");
    assert!(
        rem.contains("ledgerful index"),
        "remediation must contain ledgerful index: {rem}"
    );
    assert!(
        !empty["message"].as_str().unwrap_or("").contains("OK"),
        "message must not claim OK: {empty}"
    );
}

/// 0126: human Index Health must not say OK when empty.
/// Asserts exact B1.2 line + search-empty finding print (not bare "empty",
/// which graph-empty also satisfies on greenfield doctor).
#[test]
fn doctor_human_empty_search_index_not_ok() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    fs::write(root.join("dummy.txt"), "content").unwrap();

    let (stdout, stderr, code) = run_cli(root, &["doctor"]);
    assert_eq!(code, 0, "doctor human exit; stderr={stderr}");
    // Non-OK empty line (substring OK must not appear with zero docs).
    assert!(
        !stdout.contains("OK (0 documents)"),
        "must not report healthy OK with zero docs:\n{stdout}"
    );
    // B1.2 normative Index Health line (exact) — not bare "empty"/graph-empty.
    assert!(
        stdout.contains("Search index: Empty (0 documents — run 'ledgerful index')"),
        "expected B1.2 Search index Empty line:\n{stdout}"
    );
    // Structured finding print: [warn] [search-empty] …
    assert!(
        stdout.contains("search-empty"),
        "expected search-empty finding code in human output:\n{stdout}"
    );
}

/// 0126: after real index with files, search-empty is gone; OK (N) with N>0.
#[test]
fn doctor_after_index_no_search_empty() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    fs::write(root.join("lib.rs"), "pub fn hello() {}").unwrap();
    crate::common::git_add_and_commit(root, "lib.rs");

    let (out, err, code) = run_cli(root, &["init"]);
    assert_eq!(code, 0, "init should succeed; stderr={err}; stdout={out}");

    // Populate Tantivy via search rebuild (same path as document_count==0 auto-index).
    let (out, err, code) = run_cli(root, &["search", "hello", "--index", "--json"]);
    assert_eq!(
        code, 0,
        "search --index should succeed; stderr={err}; stdout={out}"
    );

    let (stdout, stderr, code) = run_cli(root, &["doctor", "--json"]);
    assert_eq!(code, 0, "doctor --json; stderr={stderr}");
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).expect("pure JSON");
    let findings = v["findings"].as_array().expect("findings");
    assert!(
        !findings.iter().any(|f| f["code"] == "search-empty"),
        "populated index must not emit search-empty: {v}"
    );

    // Human path: OK (N) with N>0
    let (human, _, code) = run_cli(root, &["doctor"]);
    assert_eq!(code, 0);
    assert!(
        human.contains("Search index: OK (") && !human.contains("OK (0 documents)"),
        "expected OK (N) with N>0:\n{human}"
    );
}
