use crate::common::{run_cli, run_cli_isolated, setup_git_repo};
use std::fs;
use std::process::Command;
use tempfile::tempdir;

/// Isolated child: synthetic home and a known-missing default template, so
/// key lookup and template inspection cannot reach the host.
#[test]
fn doctor_reports_system_health__slow() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    let home = root.join("home");
    fs::create_dir_all(&home).unwrap();

    Command::new("git")
        .arg("init")
        .current_dir(root)
        .output()
        .expect("Failed to run git init");

    let (stdout, stderr, code) = run_cli_no_template(root, &home, &["doctor", "--json"], &[]);
    assert_eq!(
        code, 0,
        "doctor must not block on a bare repo; stderr={stderr}"
    );
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).expect("pure JSON stdout");
    assert_eq!(v["schemaVersion"], 1);
    assert_eq!(v["readyForPublish"], true);
}

/// 0109: `doctor --json` stdout is pure schema-v1 JSON (no human banners).
#[test]
fn doctor_json_stdout_is_pure_schema_v1__slow() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    fs::write(root.join("dummy.txt"), "content").unwrap();

    let (stdout, stderr, code) = run_cli_isolated(root, &["doctor", "--json"]);
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
    assert!(v.get("notEvaluated").is_none());
    assert_eq!(v["readyForPublishScope"]["readyMeans"], "zeroBlockFindings");
    assert_eq!(
        v["readyForPublishScope"]["notRequiredForReady"],
        serde_json::json!(["chain", "fullTests", "signerTrust"])
    );
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
fn version_flags_short_without_sha_long_may_include_embed__slow() {
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
fn doctor_non_engine_repo_has_no_binary_behind_tree__slow() {
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

    let (stdout, stderr, code) = run_cli_isolated(root, &["doctor", "--json"]);
    assert_eq!(
        code, 0,
        "non-engine doctor --json should exit 0; stderr={stderr}"
    );
    let v: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("stdout must be pure JSON");
    let findings = v["findings"].as_array().expect("findings array");
    for code in [
        "binary-behind-tree",
        "binary-ahead-of-tree",
        "binary-diverges-from-tree",
    ] {
        assert!(
            !findings.iter().any(|f| f["code"] == code),
            "consumer layout must not emit {code}: {v}"
        );
    }
}

/// 0205 T0+T12: consumer repos skip GitHub Latest (zero HTTP, no 0205 codes).
#[test]
fn doctor_non_engine_repo_has_no_binary_latest_codes__slow() {
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

    let (stdout, stderr, code) = run_cli_isolated(root, &["doctor", "--json"]);
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
fn doctor_json_environment_github_latest_always_object__slow() {
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

    let (stdout, stderr, code) = run_cli_isolated(root, &["doctor", "--json"]);
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
fn doctor_malformed_config_emits_legacy_config_warn_json__slow() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    fs::write(root.join("dummy.txt"), "content").unwrap();

    let state = root.join(".ledgerful");
    fs::create_dir_all(state.join("state")).unwrap();
    // Deliberately invalid TOML
    fs::write(state.join("config.toml"), "[[[not valid toml").unwrap();

    let (stdout, stderr, code) = run_cli_isolated(root, &["doctor", "--json"]);
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
fn doctor_exit_1_on_block_finding__slow() {
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

    let (stdout, stderr, code) = run_cli_isolated(root, &["doctor", "--json"]);
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
    assert!(v.get("readyForPublishDefinition").is_none());
    assert!(v.get("notEvaluated").is_none());
    assert_eq!(v["readyForPublishScope"]["readyMeans"], "zeroBlockFindings");
    assert_eq!(
        v["readyForPublishScope"]["notRequiredForReady"],
        serde_json::json!(["chain", "fullTests", "signerTrust"])
    );
}

/// 0126: production empty path — init/ensure_state_dir yields 0 docs → search-empty.
#[test]
fn doctor_json_emits_search_empty_on_unindexed_repo__slow() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    fs::write(root.join("dummy.txt"), "content").unwrap();

    // doctor ensures state dir + opens/creates Tantivy → 0 documents without index.
    let (stdout, stderr, code) = run_cli_isolated(root, &["doctor", "--json"]);
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
fn doctor_human_empty_search_index_not_ok__slow() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    fs::write(root.join("dummy.txt"), "content").unwrap();

    let (stdout, stderr, code) = run_cli_isolated(root, &["doctor"]);
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
fn doctor_after_index_no_search_empty__slow() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    fs::write(root.join("lib.rs"), "pub fn hello() {}").unwrap();
    crate::common::git_add_and_commit(root, "lib.rs");

    let (out, err, code) = run_cli_isolated(root, &["init"]);
    assert_eq!(code, 0, "init should succeed; stderr={err}; stdout={out}");

    // Populate Tantivy via search rebuild (same path as document_count==0 auto-index).
    let (out, err, code) = run_cli_isolated(root, &["search", "hello", "--index", "--json"]);
    assert_eq!(
        code, 0,
        "search --index should succeed; stderr={err}; stdout={out}"
    );

    let (stdout, stderr, code) = run_cli_isolated(root, &["doctor", "--json"]);
    assert_eq!(code, 0, "doctor --json; stderr={stderr}");
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).expect("pure JSON");
    let findings = v["findings"].as_array().expect("findings");
    assert!(
        !findings.iter().any(|f| f["code"] == "search-empty"),
        "populated index must not emit search-empty: {v}"
    );

    // Human path: OK (N) with N>0
    let (human, _, code) = run_cli_isolated(root, &["doctor"]);
    assert_eq!(code, 0);
    assert!(
        human.contains("Search index: OK (") && !human.contains("OK (0 documents)"),
        "expected OK (N) with N>0:\n{human}"
    );
}

/// 0422: file-assigned provider key emits `config-plaintext-secret` (values never leak).
#[test]
fn test_doctor_json_plaintext_secret_file_key__slow() {
    const SENTINEL: &str = "test-key-not-a-secret";
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    fs::write(root.join("dummy.txt"), "content").unwrap();
    let state = root.join(".ledgerful");
    fs::create_dir_all(&state).unwrap();
    fs::write(
        state.join("config.toml"),
        format!("[gemini]\napi_key = \"{SENTINEL}\"\n"),
    )
    .unwrap();

    let (stdout, stderr, code) = run_cli_isolated(root, &["doctor", "--json"]);
    assert_eq!(
        code, 0,
        "doctor --json without block should exit 0; stderr={stderr}"
    );
    assert!(
        !stdout.contains(SENTINEL),
        "sentinel leaked to stdout:\n{stdout}"
    );
    assert!(
        !stderr.contains(SENTINEL),
        "sentinel leaked to stderr:\n{stderr}"
    );

    let v: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("stdout must be pure JSON");
    assert_eq!(v["schemaVersion"], 1);
    assert_eq!(v["readyForPublish"], true);
    let findings = v["findings"].as_array().expect("findings array");
    let hit = findings
        .iter()
        .find(|f| f["code"] == "config-plaintext-secret")
        .unwrap_or_else(|| panic!("expected config-plaintext-secret: {v}"));
    assert_eq!(hit["severity"], "warn");
    assert_eq!(hit["category"], "other");
    let msg = hit["message"].as_str().unwrap_or("");
    assert!(
        msg.contains("gemini.api_key"),
        "message must name gemini.api_key: {msg}"
    );
    assert!(!msg.contains(SENTINEL), "message leaked sentinel: {msg}");
    let rem = hit["remediation"].as_str().unwrap_or("");
    assert!(
        rem.contains("ledgerful doctor --json"),
        "remediation must name doctor --json: {rem}"
    );
    assert!(
        !rem.contains(SENTINEL),
        "remediation leaked sentinel: {rem}"
    );
}

/// Child `ledgerful` run with the default template and provider env isolated.
///
/// Homes point at the synthetic `home`, the default template points at a
/// missing path unless `extra` overrides it, and provider keys are removed so
/// nothing from the host can change the outcome.
fn run_cli_no_template(
    root: &std::path::Path,
    home: &std::path::Path,
    args: &[&str],
    extra: &[(&str, &str)],
) -> (String, String, i32) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_ledgerful"));
    cmd.args(args).current_dir(root);
    // One census lives in `common::isolate_cli_env` (DoD-isolation receipt).
    crate::common::isolate_cli_env(&mut cmd, home);
    for (k, v) in extra {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("failed to run ledgerful");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().unwrap_or(-1),
    )
}

/// Recursively collect every file under `dir` whose bytes contain `needle`.
fn files_containing(dir: &std::path::Path, needle: &str) -> Vec<std::path::PathBuf> {
    let mut hits = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return hits;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            hits.extend(files_containing(&path, needle));
        } else if fs::read(&path)
            .map(|bytes| String::from_utf8_lossy(&bytes).contains(needle))
            .unwrap_or(false)
        {
            hits.push(path);
        }
    }
    hits
}

/// Message of the single `config-plaintext-secret` finding, looked up by code
/// because other warnings sort ahead of it.
fn secret_message_of(v: &serde_json::Value) -> String {
    v["findings"]
        .as_array()
        .expect("findings array")
        .iter()
        .find(|f| f["code"] == "config-plaintext-secret")
        .and_then(|f| f["message"].as_str())
        .expect("config-plaintext-secret finding")
        .to_string()
}

fn doctor_codes(v: &serde_json::Value) -> Vec<String> {
    let mut codes: Vec<String> = v["findings"]
        .as_array()
        .expect("findings array")
        .iter()
        .map(|f| f["code"].as_str().unwrap_or_default().to_string())
        .filter(|c| {
            matches!(
                c.as_str(),
                "config-plaintext-secret" | "config-inspection-incomplete" | "legacy-config"
            )
        })
        .collect();
    codes.sort();
    codes
}

#[test]
fn user_template_secret_project_key_reports_path_not_value__slow() {
    const SENTINEL: &str = "SENTINEL-0554-PROJECT-VALUE";
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    let home = root.join("home");
    fs::create_dir_all(&home).unwrap();
    setup_git_repo(root);
    fs::create_dir_all(root.join(".ledgerful")).unwrap();
    fs::write(
        root.join(".ledgerful").join("config.toml"),
        format!("[gemini]\napi_key = \"{SENTINEL}\"\n"),
    )
    .unwrap();

    let (stdout, stderr, code) = run_cli_no_template(root, &home, &["doctor", "--json"], &[]);
    assert_eq!(code, 0, "plaintext key is warn, not block; stderr={stderr}");
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).expect("pure JSON stdout");
    let finding = v["findings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["code"] == "config-plaintext-secret")
        .expect("config-plaintext-secret");
    assert_eq!(
        finding["message"],
        "Plaintext provider key(s) in project config in the Ledgerful state directory: gemini.api_key. Move them to GEMINI_API_KEY / OLLAMA_CLOUD_API_KEY (or .env) and rotate."
    );
    assert!(!stdout.contains(SENTINEL), "stdout leaked value");
    assert!(!stderr.contains(SENTINEL), "stderr leaked value");
    assert!(
        files_containing(&root.join(".ledgerful"), SENTINEL)
            .iter()
            .all(|p| p.ends_with("config.toml")),
        "only the user's own config may carry the marker"
    );
}

#[test]
fn user_template_secret_malformed_project_is_incomplete_and_value_free__slow() {
    const SENTINEL: &str = "SENTINEL-0554-MALFORMED-VALUE";
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    let home = root.join("home");
    fs::create_dir_all(&home).unwrap();
    setup_git_repo(root);
    fs::create_dir_all(root.join(".ledgerful")).unwrap();
    fs::write(
        root.join(".ledgerful").join("config.toml"),
        format!("[gemini\napi_key = \"{SENTINEL}\"\n"),
    )
    .unwrap();

    let (stdout, stderr, code) = run_cli_no_template(root, &home, &["doctor", "--json"], &[]);
    assert_eq!(code, 0, "stderr={stderr}");
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).expect("pure JSON stdout");
    assert_eq!(
        doctor_codes(&v),
        ["config-inspection-incomplete", "legacy-config"]
    );
    assert!(!stdout.contains(SENTINEL), "stdout leaked value");
    assert!(!stderr.contains(SENTINEL), "stderr leaked value");
    let legacy = v["findings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["code"] == "legacy-config")
        .unwrap();
    assert_eq!(
        legacy["message"],
        "Failed to load project config in the Ledgerful state directory (parse). Using defaults at runtime; review the project config and rerun ledgerful doctor --json."
    );
}

#[test]
fn user_template_secret_project_directory_codes__slow() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    let home = root.join("home");
    fs::create_dir_all(&home).unwrap();
    setup_git_repo(root);
    fs::create_dir_all(root.join(".ledgerful").join("config.toml")).unwrap();

    let (stdout, stderr, code) = run_cli_no_template(root, &home, &["doctor", "--json"], &[]);
    assert_eq!(code, 0, "stderr={stderr}");
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).expect("pure JSON stdout");
    assert_eq!(
        doctor_codes(&v),
        ["config-inspection-incomplete", "legacy-config"]
    );
}

#[test]
fn user_template_secret_override_template_scanned_only_without_project__slow() {
    const SENTINEL: &str = "SENTINEL-0554-TEMPLATE-VALUE";
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    let home = root.join("home");
    fs::create_dir_all(&home).unwrap();
    setup_git_repo(root);
    let template = root.join("template.toml");
    fs::write(
        &template,
        format!("[local_model]\nollama_key = \"{SENTINEL}\"\n"),
    )
    .unwrap();
    let template_str = template.to_string_lossy().into_owned();

    // Control: no project config, so the selected override template is inspected.
    let (stdout, stderr, code) = run_cli_no_template(
        root,
        &home,
        &["doctor", "--json"],
        &[("LEDGERFUL_DEFAULT_CONFIG", template_str.as_str())],
    );
    assert_eq!(code, 0, "stderr={stderr}");
    assert!(
        stdout.contains("template selected by LEDGERFUL_DEFAULT_CONFIG: local_model.ollama_key"),
        "override template must be attributed: {stdout}"
    );
    assert!(!stdout.contains(SENTINEL), "stdout leaked template value");

    // A project config wins, so the template is not inspected at all.
    fs::create_dir_all(root.join(".ledgerful")).unwrap();
    fs::write(
        root.join(".ledgerful").join("config.toml"),
        "[core]\nstrict = false\n",
    )
    .unwrap();
    let (stdout, _stderr, _code) = run_cli_no_template(
        root,
        &home,
        &["doctor", "--json"],
        &[("LEDGERFUL_DEFAULT_CONFIG", template_str.as_str())],
    );
    assert!(
        !stdout.contains("config-plaintext-secret"),
        "project wins; template must not be inspected: {stdout}"
    );
}

#[test]
fn doctor_json_fix_dry_run_blocked_when_project_config_missing__slow() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    let home = root.join("home");
    fs::create_dir_all(&home).unwrap();
    setup_git_repo(root);
    fs::create_dir_all(root.join(".ledgerful")).unwrap();

    let (stdout, stderr, code) = run_cli_no_template(
        root,
        &home,
        &["doctor", "--json", "--fix", "--dry-run"],
        &[],
    );
    assert_eq!(code, 0, "dry-run preview exits zero; stderr={stderr}");
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).expect("pure JSON stdout");
    assert_eq!(v["schemaVersion"], 1);
    let fix = &v["fix"];
    assert_eq!(fix["dryRun"], true);
    let kinds: Vec<&str> = fix["actions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["kind"].as_str().unwrap())
        .collect();
    assert!(
        kinds.iter().any(|k| *k == "pin" || *k == "bumpMinSig"),
        "fixture must carry a write-bearing action: {kinds:?}"
    );
    assert_eq!(fix["blocked"], true);
    assert_eq!(
        fix["refusal"],
        "Doctor config fixes require a readable regular project config with valid TOML and supported fix target types. Write access is checked when applying. If the project config in the Ledgerful state directory is missing, run ledgerful init; otherwise repair its contents, type or permissions. Inspect the project config, then rerun ledgerful doctor --fix --yes."
    );
    assert!(
        !root.join(".ledgerful").join("config.toml").exists(),
        "dry-run must not materialize a project config"
    );
}

#[test]
fn doctor_json_fix_allowed_preview_omits_blocked_fields__slow() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    let home = root.join("home");
    fs::create_dir_all(&home).unwrap();
    setup_git_repo(root);
    fs::create_dir_all(root.join(".ledgerful")).unwrap();
    fs::write(
        root.join(".ledgerful").join("config.toml"),
        "[core]\nstrict = false\n",
    )
    .unwrap();

    let (stdout, stderr, code) = run_cli_no_template(
        root,
        &home,
        &["doctor", "--json", "--fix", "--dry-run"],
        &[],
    );
    assert_eq!(code, 0, "stderr={stderr}");
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).expect("pure JSON stdout");
    assert!(
        v["fix"].get("blocked").is_none(),
        "allowed preview: {}",
        v["fix"]
    );
    assert!(
        v["fix"].get("refusal").is_none(),
        "allowed preview: {}",
        v["fix"]
    );
}

#[test]
fn doctor_fix_yes_blocked_refuses_with_exit_one_and_no_writes__slow() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    let home = root.join("home");
    fs::create_dir_all(&home).unwrap();
    setup_git_repo(root);
    fs::create_dir_all(root.join(".ledgerful")).unwrap();

    // Seed a prior sidecar run; a refused apply must leave it byte-identical.
    let (_seed_out, seed_err, seed_code) =
        run_cli_no_template(root, &home, &["doctor", "--json"], &[]);
    assert_eq!(seed_code, 0, "seed run; stderr={seed_err}");
    let sidecar = root
        .join(".ledgerful")
        .join("state")
        .join("doctor-results.json");
    let before = fs::read(&sidecar).expect("prior sidecar must exist");

    let (stdout, stderr, code) =
        run_cli_no_template(root, &home, &["doctor", "--json", "--fix", "--yes"], &[]);
    assert_eq!(code, 1, "blocked apply exits 1; stdout={stdout}");
    assert_eq!(
        fs::read(&sidecar).expect("sidecar still present"),
        before,
        "refused apply must preserve the prior sidecar bytes"
    );
    assert!(
        stderr.contains("Doctor config fixes require a readable regular project config"),
        "fixed refusal on stderr: {stderr}"
    );
    assert!(
        serde_json::from_str::<serde_json::Value>(stdout.trim()).is_err(),
        "no successful JSON envelope on refusal"
    );
    assert!(!root.join(".ledgerful").join("config.toml").exists());
}

#[test]
fn user_template_secret_bare_fix_keeps_validation_error_and_exit_one__slow() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    let home = root.join("home");
    fs::create_dir_all(&home).unwrap();
    setup_git_repo(root);
    let (_stdout, stderr, code) = run_cli_no_template(root, &home, &["doctor", "--fix"], &[]);
    assert_eq!(code, 1);
    assert!(
        stderr.contains("doctor --fix requires --yes or --dry-run"),
        "validation error retained: {stderr}"
    );
}

/// Compatibility control (0554): public `config set` keeps its current
/// missing-project fallback. The template is synthetic and nonsecret. This pins
/// current behavior pending 0554-opencode-M-2-public-materialization; it is not
/// approval of copying credentials.
#[test]
fn user_template_secret_public_set_current_behavior_pending_followup__slow() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    let home = root.join("home");
    fs::create_dir_all(&home).unwrap();
    setup_git_repo(root);
    fs::create_dir_all(root.join(".ledgerful")).unwrap();
    let template = root.join("template.toml");
    fs::write(&template, "[core]\nstrict = false\n").unwrap();
    let template_str = template.to_string_lossy().to_string();

    let (stdout, stderr, code) = run_cli_no_template(
        root,
        &home,
        &["config", "set", "core.strict=true"],
        &[("LEDGERFUL_DEFAULT_CONFIG", template_str.as_str())],
    );
    assert_eq!(
        code, 0,
        "public set succeeds; stdout={stdout} stderr={stderr}"
    );
    let written = fs::read_to_string(root.join(".ledgerful").join("config.toml")).unwrap();
    assert!(written.contains("strict = true"), "set applied: {written}");
}

/// Determinism (0554): the same repo state and fixture produce identical
/// config-* findings on two consecutive `doctor --json` runs.
#[test]
fn user_template_secret_findings_deterministic_twice__slow() {
    const SENTINEL: &str = "SENTINEL-0554-DETERMINISM-VALUE";
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    let home = root.join("home");
    fs::create_dir_all(&home).unwrap();
    setup_git_repo(root);
    fs::create_dir_all(root.join(".ledgerful")).unwrap();
    fs::write(
        root.join(".ledgerful").join("config.toml"),
        format!("[gemini]\napi_key = \"{SENTINEL}\"\n"),
    )
    .unwrap();

    let config_findings = || {
        let (stdout, stderr, code) = run_cli_no_template(root, &home, &["doctor", "--json"], &[]);
        assert_eq!(code, 0, "doctor runs; stderr={stderr}");
        let v: serde_json::Value = serde_json::from_str(stdout.trim()).expect("pure JSON stdout");
        let found: Vec<serde_json::Value> = v["findings"]
            .as_array()
            .expect("findings array")
            .iter()
            .filter(|f| f["code"].as_str().unwrap_or("").starts_with("config-"))
            .cloned()
            .collect();
        found
    };
    let first = config_findings();
    let second = config_findings();
    assert!(
        first.iter().any(|f| f["code"] == "config-plaintext-secret"),
        "fixture carries the finding: {first:?}"
    );
    assert_eq!(first, second, "config-* findings identical across runs");
}

/// Key-name boundary (0554): the legacy unknown-key message keeps the ignored
/// key name by design; the changed plaintext-key findings never do. Credential
/// values never appear anywhere.
#[test]
fn user_template_secret_unknown_key_name_contract__slow() {
    const VALUE: &str = "SENTINEL-0554-KEYNAME-VALUE";
    const KEY_MARKER: &str = "zz_0554_unknown_key_marker";
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    let home = root.join("home");
    fs::create_dir_all(&home).unwrap();
    setup_git_repo(root);
    fs::create_dir_all(root.join(".ledgerful")).unwrap();
    fs::write(
        root.join(".ledgerful").join("config.toml"),
        format!("{KEY_MARKER} = 1\n\n[gemini]\napi_key = \"{VALUE}\"\n"),
    )
    .unwrap();

    let (stdout, stderr, code) = run_cli_no_template(root, &home, &["doctor", "--json"], &[]);
    assert_eq!(code, 0, "stderr={stderr}");
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).expect("pure JSON stdout");
    let findings = v["findings"].as_array().unwrap();

    let legacy = findings
        .iter()
        .find(|f| f["code"] == "legacy-config")
        .expect("unknown key surfaces through legacy-config");
    assert!(
        legacy["message"].as_str().unwrap().contains(KEY_MARKER),
        "legacy unknown-key semantics retained: {legacy}"
    );

    for f in findings.iter().filter(|f| {
        matches!(
            f["code"].as_str(),
            Some("config-plaintext-secret" | "config-inspection-incomplete")
        )
    }) {
        let text = format!("{}{}", f["message"], f["remediation"]);
        assert!(
            !text.contains(KEY_MARKER),
            "inspection finding leaked key name: {f}"
        );
        assert!(
            !text.contains(VALUE),
            "inspection finding leaked value: {f}"
        );
    }
    assert!(!stdout.contains(VALUE), "stdout leaked value");
    assert!(!stderr.contains(VALUE), "stderr leaked value");
}

/// Positive-risk fixture (0554): a sentinel template with min_sig_version=1 and
/// no project config. The old behavior materialized a config through the pin /
/// bump path; the guard must refuse first, leave the template byte-identical,
/// and print no sentinel.
#[test]
fn user_template_secret_positive_risk_refused_no_materialization__slow() {
    const SENTINEL: &str = "SENTINEL-0554-POSITIVE-RISK";
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    let home = root.join("home");
    fs::create_dir_all(&home).unwrap();
    setup_git_repo(root);
    fs::create_dir_all(root.join(".ledgerful")).unwrap();
    let template = root.join("template.toml");
    let original = format!("[gemini]\napi_key = \"{SENTINEL}\"\n\n[intent]\nmin_sig_version = 1\n");
    fs::write(&template, &original).unwrap();
    let template_str = template.to_string_lossy().into_owned();

    let (stdout, stderr, code) = run_cli_no_template(
        root,
        &home,
        &["doctor", "--json", "--fix", "--yes"],
        &[("LEDGERFUL_DEFAULT_CONFIG", template_str.as_str())],
    );
    assert_eq!(code, 1, "blocked apply exits 1; stdout={stdout}");
    assert!(
        stderr.contains("Doctor config fixes require a readable regular project config"),
        "fixed refusal: {stderr}"
    );
    assert!(
        !root.join(".ledgerful").join("config.toml").exists(),
        "no materialized config"
    );
    assert_eq!(
        fs::read_to_string(&template).unwrap(),
        original,
        "template unchanged"
    );
    assert!(!stdout.contains(SENTINEL), "stdout leaked sentinel");
    assert!(!stderr.contains(SENTINEL), "stderr leaked sentinel");
}

/// Existing-project control: the same write-bearing plan applies when the
/// regular project config exists.
#[test]
fn user_template_secret_existing_project_control_applies__slow() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    let home = root.join("home");
    fs::create_dir_all(&home).unwrap();
    // The pin action needs a local signing identity under the synthetic HOME.
    // Seed a hermetic one so the pin is applicable rather than pinUnavailable.
    ledgerful::ledger::get_or_create_keys_in(&home.join(".ledgerful").join("keys")).unwrap();
    setup_git_repo(root);
    fs::create_dir_all(root.join(".ledgerful")).unwrap();
    fs::write(
        root.join(".ledgerful").join("config.toml"),
        "[core]\nstrict = false\n",
    )
    .unwrap();

    let (stdout, stderr, code) =
        run_cli_no_template(root, &home, &["doctor", "--json", "--fix", "--yes"], &[]);
    assert_eq!(
        code, 0,
        "existing-project apply succeeds; stdout={stdout} stderr={stderr}"
    );
    let written = fs::read_to_string(root.join(".ledgerful").join("config.toml")).unwrap();
    assert!(
        written.contains("min_sig_version = 2"),
        "bump applied: {written}"
    );
}

/// No-action control: a trusted key and min_sig_version=2 leave an empty plan,
/// which succeeds without a project config and creates none.
#[test]
fn user_template_secret_no_action_control_creates_no_config__slow() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    let home = root.join("home");
    fs::create_dir_all(&home).unwrap();
    setup_git_repo(root);
    fs::create_dir_all(root.join(".ledgerful")).unwrap();
    let template = root.join("template.toml");
    fs::write(
        &template,
        format!(
            "[intent]\ntrusted_public_keys = [\"{}\"]\nmin_sig_version = 2\n",
            "e".repeat(64)
        ),
    )
    .unwrap();
    let template_str = template.to_string_lossy().into_owned();

    let (stdout, stderr, code) = run_cli_no_template(
        root,
        &home,
        &["doctor", "--json", "--fix", "--yes"],
        &[("LEDGERFUL_DEFAULT_CONFIG", template_str.as_str())],
    );
    assert_eq!(
        code, 0,
        "empty plan succeeds; stdout={stdout} stderr={stderr}"
    );
    assert!(
        !root.join(".ledgerful").join("config.toml").exists(),
        "no config created"
    );
}

/// Human `-v` debug oracle (0554): the fixed marker proves the load-failure
/// branch ran; the malformed content's sentinel must stay absent.
#[test]
fn user_template_secret_debug_marker_is_value_free__slow() {
    const SENTINEL: &str = "SENTINEL-0554-DEBUG-VALUE";
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    let home = root.join("home");
    fs::create_dir_all(&home).unwrap();
    setup_git_repo(root);
    fs::create_dir_all(root.join(".ledgerful")).unwrap();
    fs::write(
        root.join(".ledgerful").join("config.toml"),
        format!("[gemini\napi_key = \"{SENTINEL}\"\n"),
    )
    .unwrap();

    let (stdout, stderr, _code) = run_cli_no_template(root, &home, &["doctor", "-v"], &[]);
    assert!(
        stderr.contains("doctor config load failed (continuing with defaults)")
            || stdout.contains("doctor config load failed (continuing with defaults)"),
        "fixed debug marker must prove the branch ran; stderr={stderr}"
    );
    assert!(!stdout.contains(SENTINEL), "stdout leaked value");
    assert!(!stderr.contains(SENTINEL), "stderr leaked value");
}

/// Child run that leaves LEDGERFUL_DEFAULT_CONFIG unset so USERPROFILE / HOME
/// selection is exercised. Both homes point at the synthetic `home`.
fn run_cli_user_profile_template(
    root: &std::path::Path,
    home: &std::path::Path,
    args: &[&str],
) -> (String, String, i32) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_ledgerful"));
    cmd.args(args).current_dir(root);
    for key in [
        "GEMINI_API_KEY",
        "OLLAMA_CLOUD_API_KEY",
        "OLLAMA_API_KEY",
        "LEDGERFUL_DEFAULT_CONFIG",
    ] {
        cmd.env_remove(key);
    }
    cmd.env("USERPROFILE", home).env("HOME", home);
    let out = cmd.output().expect("failed to run ledgerful");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().unwrap_or(-1),
    )
}

/// Loader parity (DoD-1): the diagnostic source and the runtime loader must
/// read the same template. The marker `watch.debounce_ms` proves the loader
/// used the override; the secret proves the diagnostic inspected it.
#[test]
fn user_template_secret_loader_parity_override_marker__slow() {
    const VALUE: &str = "PARITY-VALUE-0554-OVERRIDE";
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    let home = root.join("home");
    fs::create_dir_all(&home).unwrap();
    setup_git_repo(root);
    let template = root.join("override.toml");
    fs::write(
        &template,
        format!("[watch]\ndebounce_ms = 4321\n\n[gemini]\napi_key = \"{VALUE}\"\n"),
    )
    .unwrap();
    let template_str = template.to_string_lossy().into_owned();
    let extra = [("LEDGERFUL_DEFAULT_CONFIG", template_str.as_str())];

    let (view, view_err, view_code) = run_cli_no_template(
        root,
        &home,
        &["config", "view", "--json", "--section", "watch"],
        &extra,
    );
    assert_eq!(view_code, 0, "config view stderr={view_err}");
    assert!(
        view.contains("4321"),
        "loader must read the override: {view}"
    );

    let (stdout, stderr, code) = run_cli_no_template(root, &home, &["doctor", "--json"], &extra);
    assert_eq!(code, 0, "stderr={stderr}");
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).expect("pure JSON stdout");
    assert_eq!(doctor_codes(&v), ["config-plaintext-secret"]);
    assert_eq!(
        secret_message_of(&v),
        "Plaintext provider key(s) in template selected by LEDGERFUL_DEFAULT_CONFIG: gemini.api_key. Move them to GEMINI_API_KEY / OLLAMA_CLOUD_API_KEY (or .env) and rotate."
    );
    assert!(!stdout.contains(VALUE), "stdout leaked value");
    assert!(!view.contains(VALUE), "config view leaked value");
}

/// Loader parity for the USERPROFILE origin: with no override the loader and
/// the diagnostic both select USERPROFILE/.ledgerful/default-config.toml.
#[test]
fn user_template_secret_loader_parity_userprofile_marker__slow() {
    const VALUE: &str = "PARITY-VALUE-0554-USERPROFILE";
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    let home = root.join("home");
    fs::create_dir_all(home.join(".ledgerful")).unwrap();
    setup_git_repo(root);
    fs::write(
        home.join(".ledgerful").join("default-config.toml"),
        format!("[watch]\ndebounce_ms = 2468\n\n[gemini]\napi_key = \"{VALUE}\"\n"),
    )
    .unwrap();

    let (view, view_err, view_code) = run_cli_user_profile_template(
        root,
        &home,
        &["config", "view", "--json", "--section", "watch"],
    );
    assert_eq!(view_code, 0, "config view stderr={view_err}");
    assert!(
        view.contains("2468"),
        "loader must read USERPROFILE: {view}"
    );

    let (stdout, stderr, code) = run_cli_user_profile_template(root, &home, &["doctor", "--json"]);
    assert_eq!(code, 0, "stderr={stderr}");
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).expect("pure JSON stdout");
    assert_eq!(doctor_codes(&v), ["config-plaintext-secret"]);
    assert_eq!(
        secret_message_of(&v),
        "Plaintext provider key(s) in user template USERPROFILE/.ledgerful/default-config.toml: gemini.api_key. Move them to GEMINI_API_KEY / OLLAMA_CLOUD_API_KEY (or .env) and rotate."
    );
    assert!(!stdout.contains(VALUE), "stdout leaked value");
    assert!(!stderr.contains(VALUE), "stderr leaked value");
    assert!(!view.contains(VALUE), "config view leaked value");
}

/// Unix/WSL symlink policy (DoD-1): a symlink to a regular project config is
/// inspected; a symlink to a directory is refused as a metadata gap.
#[cfg(unix)]
#[test]
fn user_template_secret_unix_symlink_regular_inspected_directory_refused__slow() {
    use std::os::unix::fs::symlink;
    const VALUE: &str = "SYMLINK-VALUE-0554";
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    let home = root.join("home");
    fs::create_dir_all(&home).unwrap();
    setup_git_repo(root);
    fs::create_dir_all(root.join(".ledgerful")).unwrap();

    let regular = root.join("real-config.toml");
    fs::write(&regular, format!("[gemini]\napi_key = \"{VALUE}\"\n")).unwrap();
    symlink(&regular, root.join(".ledgerful").join("config.toml")).unwrap();
    let (stdout, stderr, code) = run_cli_no_template(root, &home, &["doctor", "--json"], &[]);
    assert_eq!(code, 0, "stderr={stderr}");
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).expect("pure JSON stdout");
    assert_eq!(doctor_codes(&v), ["config-plaintext-secret"]);
    assert!(!stdout.contains(VALUE), "stdout leaked value");

    fs::remove_file(root.join(".ledgerful").join("config.toml")).unwrap();
    let dir_target = root.join("config-dir");
    fs::create_dir_all(&dir_target).unwrap();
    symlink(&dir_target, root.join(".ledgerful").join("config.toml")).unwrap();
    let (stdout, stderr, code) = run_cli_no_template(root, &home, &["doctor", "--json"], &[]);
    assert_eq!(code, 0, "stderr={stderr}");
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).expect("pure JSON stdout");
    let incomplete = v["findings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["code"] == "config-inspection-incomplete")
        .expect("directory target must be an inspection gap");
    assert_eq!(
        incomplete["message"],
        "Plaintext provider-key inspection incomplete for project config in the Ledgerful state directory: metadata."
    );
}

/// `debounce_ms = 0` is a semantic validation failure, not a raw inspection
/// gap: the raw table still parses, so the secret is reported and the typed
/// load failure is legacy-config only.
#[test]
fn user_template_secret_debounce_zero_is_legacy_plus_secret__slow() {
    const VALUE: &str = "DEBOUNCE-VALUE-0554";
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    let home = root.join("home");
    fs::create_dir_all(&home).unwrap();
    setup_git_repo(root);
    fs::create_dir_all(root.join(".ledgerful")).unwrap();
    fs::write(
        root.join(".ledgerful").join("config.toml"),
        format!("[gemini]\napi_key = \"{VALUE}\"\n\n[watch]\ndebounce_ms = 0\n"),
    )
    .unwrap();

    let (stdout, stderr, code) = run_cli_no_template(root, &home, &["doctor", "--json"], &[]);
    assert_eq!(code, 0, "stderr={stderr}");
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).expect("pure JSON stdout");
    assert_eq!(
        doctor_codes(&v),
        ["config-plaintext-secret", "legacy-config"]
    );
    assert!(!stdout.contains(VALUE), "stdout leaked value");
}

/// Invalid-only project (wrong-typed provider key): typed load fails and the
/// raw inspection reports the type gap. No secret claim.
#[test]
fn user_template_secret_project_invalid_only_code_set__slow() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    let home = root.join("home");
    fs::create_dir_all(&home).unwrap();
    setup_git_repo(root);
    fs::create_dir_all(root.join(".ledgerful")).unwrap();
    fs::write(
        root.join(".ledgerful").join("config.toml"),
        "[gemini]\napi_key = 42\n",
    )
    .unwrap();

    let (stdout, stderr, code) = run_cli_no_template(root, &home, &["doctor", "--json"], &[]);
    assert_eq!(code, 0, "stderr={stderr}");
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).expect("pure JSON stdout");
    assert_eq!(
        doctor_codes(&v),
        ["config-inspection-incomplete", "legacy-config"]
    );
}

/// Mixed project: one string provider key and one wrong-typed alias. The
/// doctor set is exactly the three codes, and the invalid suffix names only
/// the allowlisted path.
#[test]
fn user_template_secret_project_mixed_code_set__slow() {
    const VALUE: &str = "MIXED-VALUE-0554";
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    let home = root.join("home");
    fs::create_dir_all(&home).unwrap();
    setup_git_repo(root);
    fs::create_dir_all(root.join(".ledgerful")).unwrap();
    fs::write(
        root.join(".ledgerful").join("config.toml"),
        format!("[gemini]\napi_key = \"{VALUE}\"\n\n[local_model]\nollama_key = 42\n"),
    )
    .unwrap();

    let (stdout, stderr, code) = run_cli_no_template(root, &home, &["doctor", "--json"], &[]);
    assert_eq!(code, 0, "stderr={stderr}");
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).expect("pure JSON stdout");
    assert_eq!(
        doctor_codes(&v),
        [
            "config-inspection-incomplete",
            "config-plaintext-secret",
            "legacy-config"
        ]
    );
    let incomplete = v["findings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["code"] == "config-inspection-incomplete")
        .unwrap();
    assert!(
        incomplete["message"]
            .as_str()
            .unwrap()
            .ends_with("Invalid provider key or container type(s): local_model.ollama_key."),
        "invalid suffix names only the allowlisted path: {incomplete}"
    );
    assert!(!stdout.contains(VALUE), "stdout leaked value");
}

/// Wrong-typed template fields are an inspection gap for that template and
/// never add a project-only legacy-config finding (DoD-3).
#[test]
fn user_template_secret_template_invalid_type_adds_no_legacy_config__slow() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    let home = root.join("home");
    fs::create_dir_all(&home).unwrap();
    setup_git_repo(root);
    fs::create_dir_all(root.join(".ledgerful")).unwrap();
    let template = root.join("template.toml");
    fs::write(&template, "[gemini]\napi_key = 42\n").unwrap();
    let template_str = template.to_string_lossy().into_owned();

    let (stdout, stderr, code) = run_cli_no_template(
        root,
        &home,
        &["doctor", "--json"],
        &[("LEDGERFUL_DEFAULT_CONFIG", template_str.as_str())],
    );
    assert_eq!(code, 0, "stderr={stderr}");
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).expect("pure JSON stdout");
    let codes = doctor_codes(&v);
    assert!(
        codes.iter().all(|c| c != "legacy-config"),
        "template type gaps must not add legacy-config: {codes:?}"
    );
    assert_eq!(codes, ["config-inspection-incomplete"]);
}

/// Attribution sentinel in the override filename and a credential sentinel in
/// the invalid template body must stay out of every output and every file the
/// command writes under `.ledgerful`, including doctor-results.json.
#[test]
fn user_template_secret_override_filename_sentinel_absent_everywhere__slow() {
    const ATTR: &str = "ATTR-SENTINEL-0554-FILENAME";
    const CRED: &str = "CRED-SENTINEL-0554-BODY";
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    let home = root.join("home");
    fs::create_dir_all(&home).unwrap();
    setup_git_repo(root);
    fs::create_dir_all(root.join(".ledgerful")).unwrap();
    // Outside the repo tree so the template is not a working-tree change.
    let outside = tempdir().unwrap();
    let template = outside.path().join(format!("{ATTR}.toml"));
    fs::write(&template, format!("api_key = \"{CRED}\"\n[[broken\n")).unwrap();
    let template_str = template.to_string_lossy().into_owned();
    let extra = [("LEDGERFUL_DEFAULT_CONFIG", template_str.as_str())];

    let (json_out, json_err, _) = run_cli_no_template(root, &home, &["doctor", "--json"], &extra);
    let (human_out, human_err, _) = run_cli_no_template(root, &home, &["doctor", "-v"], &extra);
    let (fix_out, fix_err, _) = run_cli_no_template(
        root,
        &home,
        &["doctor", "--json", "--fix", "--dry-run"],
        &extra,
    );
    for (label, text) in [
        ("json stdout", &json_out),
        ("json stderr", &json_err),
        ("human stdout", &human_out),
        ("human stderr", &human_err),
        ("fix stdout", &fix_out),
        ("fix stderr", &fix_err),
    ] {
        assert!(!text.contains(ATTR), "{label} leaked filename sentinel");
        assert!(!text.contains(CRED), "{label} leaked credential sentinel");
    }
    let tree = root.join(".ledgerful");
    for needle in [ATTR, CRED] {
        let hits = files_containing(&tree, needle);
        assert!(hits.is_empty(), "state files leaked {needle}: {hits:?}");
    }
    assert!(
        human_err.contains("doctor config load failed (continuing with defaults)")
            || human_out.contains("doctor config load failed (continuing with defaults)"),
        "fixed debug marker must prove the branch ran; stderr={human_err}"
    );
}

/// Startup fallback WARN (DoD-4): a malformed override triggers the fixed
/// neutral warning on stderr, with no path or parser text.
#[test]
fn user_template_secret_fixed_startup_warn_on_stderr__slow() {
    const ATTR: &str = "WARN-ATTR-SENTINEL-0554";
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    let home = root.join("home");
    fs::create_dir_all(&home).unwrap();
    setup_git_repo(root);
    fs::create_dir_all(root.join(".ledgerful")).unwrap();
    let outside = tempdir().unwrap();
    let template = outside.path().join(format!("{ATTR}.toml"));
    fs::write(&template, "[[broken\n").unwrap();
    let template_str = template.to_string_lossy().into_owned();

    let (stdout, stderr, code) = run_cli_no_template(
        root,
        &home,
        &["doctor", "--json"],
        &[("LEDGERFUL_DEFAULT_CONFIG", template_str.as_str())],
    );
    assert_eq!(code, 0, "stderr={stderr}");
    assert!(
        stderr.contains("Configuration load failed. Using default configuration."),
        "fixed startup WARN expected on stderr: {stderr}"
    );
    assert!(!stderr.contains(ATTR), "stderr leaked override filename");
    assert!(!stdout.contains(ATTR), "stdout leaked override filename");
}

/// Human blocked dry-run (DoD-4): the refusal is the last fix-preview line
/// before the ordinary human report, not the last stdout line.
#[test]
fn user_template_secret_human_fix_dry_run_refusal_is_last_preview_line__slow() {
    const REFUSAL: &str = "Doctor config fixes require a readable regular project config with valid TOML and supported fix target types. Write access is checked when applying. If the project config in the Ledgerful state directory is missing, run ledgerful init; otherwise repair its contents, type or permissions. Inspect the project config, then rerun ledgerful doctor --fix --yes.";
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    let home = root.join("home");
    fs::create_dir_all(&home).unwrap();
    setup_git_repo(root);
    fs::create_dir_all(root.join(".ledgerful")).unwrap();

    let (stdout, stderr, code) =
        run_cli_no_template(root, &home, &["doctor", "--fix", "--dry-run"], &[]);
    assert_eq!(code, 0, "zero-block preview exits zero; stderr={stderr}");
    let lines: Vec<&str> = stdout.lines().collect();
    let refusal_at = lines
        .iter()
        .position(|l| *l == REFUSAL)
        .unwrap_or_else(|| panic!("refusal line missing: {stdout}"));
    let action_at = lines
        .iter()
        .position(|l| l.starts_with("Would "))
        .unwrap_or_else(|| panic!("preview action line missing: {stdout}"));
    assert!(
        action_at < refusal_at,
        "refusal follows the preview actions"
    );
    assert!(
        lines[refusal_at + 1..].iter().any(|l| !l.trim().is_empty()),
        "ordinary human report follows the refusal: {stdout}"
    );
}

/// Global rollup fallback WARN (DoD-4): a malformed user-level config emits the
/// fixed neutral global warning on stderr, with no path or parser text.
#[test]
fn user_template_secret_global_rollup_warn_on_stderr__slow() {
    const ATTR: &str = "GLOBAL-ATTR-SENTINEL-0554";
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    let home = root.join("home");
    fs::create_dir_all(&home).unwrap();
    setup_git_repo(root);
    let user_state = root.join(format!("user-{ATTR}")).join(".ledgerful");
    fs::create_dir_all(&user_state).unwrap();
    fs::write(user_state.join("config.toml"), "[[broken\n").unwrap();
    let user_state_str = user_state.to_string_lossy().into_owned();

    let (stdout, stderr, _code) = run_cli_no_template(
        root,
        &home,
        &["ledger", "status", "--global"],
        &[("LEDGERFUL_CONFIG_HOME", user_state_str.as_str())],
    );
    assert!(
        stderr.contains("Global rollup configuration load failed. Using default configuration."),
        "fixed global WARN expected on stderr: {stderr}"
    );
    assert!(!stderr.contains(ATTR), "stderr leaked the user config path");
    assert!(!stdout.contains(ATTR), "stdout leaked the user config path");
}
