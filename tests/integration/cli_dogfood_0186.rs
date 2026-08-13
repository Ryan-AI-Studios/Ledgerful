//! 0186 — committed engine dogfood pack: hermetic CLI populate + fixture-isolation.

use crate::common::{git_add_and_commit, setup_git_repo};
use std::fs;
use std::path::Path;
use tempfile::tempdir;

fn cargo_root() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn examples_config_parses_with_two_service_definitions() {
    let path = cargo_root().join("docs/examples/config.toml");
    let content = fs::read_to_string(&path).unwrap();
    let cfg: ledgerful::config::model::Config =
        toml::from_str(&content).expect("examples config.toml must parse");
    assert_eq!(cfg.services.definitions.len(), 2);
    assert_eq!(cfg.services.definitions[0].name, "ledgerful");
    assert_eq!(cfg.services.definitions[1].name, "ledgerful-mcp");
    assert!(cfg.coverage.enabled, "0186-D example is fully-enabled");
    assert!(cfg.coverage.services.enabled);
    assert!(cfg.coverage.deploy.enabled);
}

fn run_cli(dir: &Path, args: &[&str]) -> (String, String, i32) {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_ledgerful"))
        .args(args)
        .current_dir(dir)
        .env("LEDGERFUL_NON_INTERACTIVE", "1")
        .output()
        .expect("failed to run ledgerful");
    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
        output.status.code().unwrap_or(-1),
    )
}

fn init_temp_repo() -> tempfile::TempDir {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src").join("lib.rs"), "pub fn main() {}\n").unwrap();
    git_add_and_commit(root, "initial");
    let (stdout, stderr, code) = run_cli(root, &["init"]);
    assert_eq!(code, 0, "init failed; stdout={stdout} stderr={stderr}");
    tmp
}

/// 0186-C: copying committed `.env.example` + `index --incremental` populates schema.
#[test]
fn env_example_incremental_index_makes_config_schema_ready() {
    let tmp = init_temp_repo();
    let root = tmp.path();
    let src = cargo_root().join(".env.example");
    fs::copy(&src, root.join(".env.example")).unwrap();
    git_add_and_commit(root, "add env.example");

    let (stdout, stderr, code) = run_cli(root, &["index", "--incremental"]);
    assert_eq!(
        code, 0,
        "index --incremental failed; stdout={stdout} stderr={stderr}"
    );

    let (stdout, stderr, code) = run_cli(root, &["config", "schema", "--json"]);
    assert_eq!(code, 0, "config schema --json; stderr={stderr}");
    let v: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("expected populated schema JSON array: {e}\n{stdout}"));
    let rows = v.as_array().unwrap_or_else(|| {
        panic!("populated schema must be a bare array, got empty envelope: {stdout}")
    });
    assert_eq!(
        rows.len(),
        21,
        "expected frozen v1 21 declarations after incremental index, got {}: {stdout}",
        rows.len()
    );
    let names: Vec<&str> = rows
        .iter()
        .map(|r| r["varName"].as_str().unwrap_or(""))
        .collect();
    assert_eq!(
        names,
        [
            "GEMINI_API_KEY",
            "LEDGERFUL_ASK_MODEL_1",
            "LEDGERFUL_ASK_PROVIDER_1",
            "LEDGERFUL_BRIDGE",
            "LEDGERFUL_CLOUD_POLICY",
            "LEDGERFUL_CONFIG_HOME",
            "LEDGERFUL_DEFAULT_CONFIG",
            "LEDGERFUL_NON_INTERACTIVE",
            "LEDGERFUL_NO_NETWORK",
            "LEDGERFUL_NO_TUI",
            "LEDGERFUL_PARENT_PID",
            "LEDGERFUL_QUIET",
            "LEDGERFUL_STATE_DIR",
            "LEDGERFUL_STRICT_OBSERVE_SIGNAL",
            "LEDGERFUL_SYNC_SECRET",
            "LEDGERFUL_TABLE_STYLE",
            "LEDGERFUL_WEB_PEER_ALLOWLIST",
            "LEDGERFUL_WEB_TOKEN",
            "OLLAMA_API_KEY",
            "OLLAMA_CLOUD_API_KEY",
            "OPENROUTER_API_KEY",
        ]
    );
    assert!(
        rows.iter()
            .any(|r| r["varName"] == "GEMINI_API_KEY" && r["isSecret"] == true),
        "GEMINI_API_KEY must be declared secret: {stdout}"
    );

    let (surfaces, stderr, code) = run_cli(root, &["surfaces", "--json"]);
    assert_eq!(code, 0, "surfaces --json; stderr={stderr}");
    let s: serde_json::Value = serde_json::from_str(surfaces.trim()).expect("surfaces JSON");
    let schema = s["surfaces"]
        .as_array()
        .expect("surfaces")
        .iter()
        .find(|row| row["id"] == "schema")
        .expect("schema row");
    assert_eq!(schema["status"], "ready", "0186-C schema ready: {surfaces}");
}

/// Cedar pack + `--analyze-graph` makes `security boundaries` non-empty.
#[test]
fn daemon_api_cedar_analyze_graph_makes_security_ready() {
    let tmp = init_temp_repo();
    let root = tmp.path();
    let src = cargo_root().join("policies").join("daemon-api.cedar");
    fs::create_dir_all(root.join("policies")).unwrap();
    fs::copy(&src, root.join("policies").join("daemon-api.cedar")).unwrap();
    git_add_and_commit(root, "add cedar pack");

    let (stdout, stderr, code) = run_cli(root, &["index", "--analyze-graph"]);
    assert_eq!(
        code, 0,
        "index --analyze-graph failed; stdout={stdout} stderr={stderr}"
    );

    let (stdout, stderr, code) = run_cli(root, &["security", "boundaries", "--json"]);
    assert_eq!(code, 0, "security boundaries --json; stderr={stderr}");
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).expect("boundaries JSON");
    assert!(
        v.get("emptyReason").is_none(),
        "expected populated security boundaries, got: {stdout}"
    );
    let auth = v["boundaries"]["auth_nodes"]
        .as_array()
        .expect("auth_nodes");
    let policy_n = auth.iter().filter(|n| n["category"] == "policy").count();
    let action_n = auth.iter().filter(|n| n["category"] == "action").count();
    assert_eq!(policy_n, 8, "expected 8 policy nodes: {stdout}");
    assert_eq!(action_n, 8, "expected 8 action nodes: {stdout}");

    let (surfaces, stderr, code) = run_cli(root, &["surfaces", "--json"]);
    assert_eq!(code, 0, "surfaces --json; stderr={stderr}");
    let s: serde_json::Value = serde_json::from_str(surfaces.trim()).expect("surfaces JSON");
    let security = s["surfaces"]
        .as_array()
        .expect("surfaces")
        .iter()
        .find(|row| row["id"] == "security")
        .expect("security row");
    assert_eq!(
        security["status"], "ready",
        "security ready after analyze-graph: {surfaces}"
    );
}

/// 0185-A: fixture Cedar under `tests/fixtures/policies` is not product graph content.
#[test]
fn fixture_only_under_tests_fixtures_leaves_security_empty() {
    let tmp = init_temp_repo();
    let root = tmp.path();
    let src = cargo_root().join("tests/fixtures/policies/dogfood_policy.cedar");
    let dest_dir = root.join("tests").join("fixtures").join("policies");
    fs::create_dir_all(&dest_dir).unwrap();
    fs::copy(&src, dest_dir.join("dogfood_policy.cedar")).unwrap();
    assert!(
        !root.join("policies").exists(),
        "must not place fixture at repo-root policies/"
    );
    git_add_and_commit(root, "add fixture only");

    let (stdout, stderr, code) = run_cli(root, &["index", "--analyze-graph"]);
    assert_eq!(
        code, 0,
        "index --analyze-graph failed; stdout={stdout} stderr={stderr}"
    );

    let (stdout, stderr, code) = run_cli(root, &["security", "boundaries", "--json"]);
    assert_eq!(code, 0, "security boundaries --json; stderr={stderr}");
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).expect("boundaries JSON");
    assert!(
        v.get("emptyReason").is_some(),
        "fixture-only must stay empty (0185-A), got: {stdout}"
    );

    let (surfaces, stderr, code) = run_cli(root, &["surfaces", "--json"]);
    assert_eq!(code, 0, "surfaces --json; stderr={stderr}");
    let s: serde_json::Value = serde_json::from_str(surfaces.trim()).expect("surfaces JSON");
    let security = s["surfaces"]
        .as_array()
        .expect("surfaces")
        .iter()
        .find(|row| row["id"] == "security")
        .expect("security row");
    assert_eq!(
        security["status"], "empty",
        "fixture-only security stays empty: {surfaces}"
    );
}
