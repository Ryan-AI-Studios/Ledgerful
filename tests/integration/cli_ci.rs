//! 0326 — declared CI inventory provenance on `ci list`.

use crate::common::{git_add_and_commit, run_cli, setup_git_repo};
use std::fs;
use tempfile::tempdir;

fn init_repo_with_workflow(yaml: &str) -> tempfile::TempDir {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    fs::create_dir_all(root.join(".github").join("workflows")).unwrap();
    fs::write(root.join(".github").join("workflows").join("ci.yml"), yaml).unwrap();
    git_add_and_commit(root, "workflow");
    let (stdout, stderr, code) = run_cli(root, &["init"]);
    assert_eq!(code, 0, "init failed; stdout={stdout} stderr={stderr}");
    let (stdout, stderr, code) = run_cli(root, &["index", "--incremental"]);
    assert_eq!(
        code, 0,
        "index --incremental failed; stdout={stdout} stderr={stderr}"
    );
    tmp
}

fn parse_object(stdout: &str, label: &str) -> serde_json::Value {
    let v: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("{label} stdout must parse as JSON: {e}\n{stdout}"));
    assert!(
        v.is_object(),
        "{label} must be an object envelope, got: {stdout}"
    );
    v
}

#[test]
fn ci_list_json_empty_catalog_has_scope_no_empty_reason() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    fs::write(root.join("README.md"), "x").unwrap();
    git_add_and_commit(root, "initial");
    let (stdout, stderr, code) = run_cli(root, &["init"]);
    assert_eq!(code, 0, "init failed; stdout={stdout} stderr={stderr}");
    let (stdout, stderr, code) = run_cli(root, &["index", "--incremental"]);
    assert_eq!(code, 0, "index failed; stdout={stdout} stderr={stderr}");

    let (stdout, stderr, code) = run_cli(root, &["ci", "list", "--json"]);
    assert_eq!(code, 0, "ci list --json; stderr={stderr}");
    let v = parse_object(&stdout, "ci list --json empty");
    assert_eq!(v["schemaVersion"], 1);
    assert_eq!(v["resultCount"], 0);
    assert_eq!(v["gates"], serde_json::json!([]));
    assert!(v.get("emptyReason").is_none(), "got {stdout}");
    assert_eq!(v["scope"]["inventory"], "declaredWorkflowJobs");
    assert_eq!(
        v["scope"]["notIncluded"],
        serde_json::json!(["branchProtectionRequired", "liveRunStatus"])
    );
    assert!(v.get("required").is_none());
    assert!(v.get("status").is_none());
}

#[test]
fn ci_list_json_emits_filepath_triggers_qualifiers_and_scope() {
    let yaml = r#"
name: CI
on:
  push:
    branches: [main]
  workflow_dispatch:
jobs:
  call:
    uses: org/repo/.github/workflows/ci.yml@main
  clippy:
    needs: web-build
    if: github.event_name == 'push'
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - if: runner.os != 'Windows'
        run: cargo clippy
"#;
    let tmp = init_repo_with_workflow(yaml);
    let (stdout, stderr, code) = run_cli(tmp.path(), &["ci", "list", "--json"]);
    assert_eq!(code, 0, "ci list --json; stderr={stderr}");
    let v = parse_object(&stdout, "ci list --json populated");
    assert_eq!(v["schemaVersion"], 1);
    assert_eq!(v["scope"]["inventory"], "declaredWorkflowJobs");
    let gates = v["gates"].as_array().expect("gates");
    assert_eq!(gates.len(), 2, "{stdout}");
    assert_eq!(gates[0]["job"], "call");
    assert_eq!(gates[1]["job"], "clippy");

    let call = gates.iter().find(|g| g["job"] == "call").expect("call");
    assert_eq!(call["filePath"], ".github/workflows/ci.yml");
    assert!(call.get("filePath").unwrap().is_string());
    assert_eq!(
        call["triggers"],
        serde_json::json!(["push", "workflow_dispatch"])
    );
    assert_eq!(call["uses"], "org/repo/.github/workflows/ci.yml@main");
    assert!(call.get("jobIf").is_none());
    assert!(call.get("required").is_none());

    let clippy = gates.iter().find(|g| g["job"] == "clippy").expect("clippy");
    assert_eq!(clippy["jobIf"], "github.event_name == 'push'");
    assert_eq!(clippy["needs"], serde_json::json!(["web-build"]));
    assert!(
        clippy.get("uses").is_none(),
        "step uses must not leak: {clippy}"
    );

    let (human, stderr, code) = run_cli(tmp.path(), &["ci", "list"]);
    assert_eq!(code, 0, "ci list; stderr={stderr}");
    assert!(
        human.contains(
            "Declared workflow jobs from the index — not GitHub required checks or live run status."
        ),
        "{human}"
    );
    assert!(human.contains(".github/workflows/ci.yml"), "{human}");
    assert!(
        human.contains("uses: org/repo/.github/workflows/ci.yml@main"),
        "{human}"
    );
}

#[test]
fn data_models_list_json_does_not_gain_ci_scope() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    fs::write(root.join("README.md"), "x").unwrap();
    git_add_and_commit(root, "initial");
    let (stdout, stderr, code) = run_cli(root, &["init"]);
    assert_eq!(code, 0, "init failed; stdout={stdout} stderr={stderr}");
    let (stdout, stderr, code) = run_cli(root, &["index", "--incremental"]);
    assert_eq!(code, 0, "index failed; stdout={stdout} stderr={stderr}");
    let (stdout, stderr, code) = run_cli(root, &["data-models", "list", "--json"]);
    assert_eq!(code, 0, "data-models list --json; stderr={stderr}");
    let v = parse_object(&stdout, "data-models list --json");
    assert!(
        v.get("scope").is_none(),
        "data-models must not inherit ci scope: {stdout}"
    );
}
