//! 0324 DoD-3: isolated tempdir proves a populated `services diff` after
//! fixture-only coverage + `index --analyze-graph`.

use crate::common::{git_add_and_commit, setup_git_repo};
use serde_json::Value;
use std::fs;
use std::process::Command;
use tempfile::tempdir;

#[test]
fn services_diff_json_names_declared_service_after_analyze_graph() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    fs::create_dir_all(root.join("src").join("billing")).unwrap();
    fs::write(root.join("src").join("lib.rs"), "pub mod billing;\n").unwrap();
    fs::write(
        root.join("src").join("billing").join("mod.rs"),
        r#"
use axum::{routing::get, Router};
async fn health() {}
pub fn app() -> Router {
    Router::new().route("/billing/health", get(health))
}
"#,
    )
    .unwrap();
    git_add_and_commit(root, "initial");

    let exe = env!("CARGO_BIN_EXE_ledgerful");
    let init = Command::new(exe)
        .arg("init")
        .current_dir(root)
        .env("LEDGERFUL_NON_INTERACTIVE", "1")
        .output()
        .unwrap();
    assert!(
        init.status.success(),
        "init failed: {}",
        String::from_utf8_lossy(&init.stderr)
    );

    fs::write(
        root.join(".ledgerful").join("config.toml"),
        r#"
[coverage]
enabled = true

[coverage.services]
enabled = true

[[services.definitions]]
name = "billing-api"
root = "src/billing"
"#,
    )
    .unwrap();

    let index = Command::new(exe)
        .args(["index", "--analyze-graph"])
        .current_dir(root)
        .env("LEDGERFUL_NON_INTERACTIVE", "1")
        .output()
        .unwrap();
    assert!(
        index.status.success(),
        "index --analyze-graph failed; stdout={} stderr={}",
        String::from_utf8_lossy(&index.stdout),
        String::from_utf8_lossy(&index.stderr)
    );

    let out = Command::new(exe)
        .args(["services", "diff", "--json"])
        .current_dir(root)
        .env("LEDGERFUL_NON_INTERACTIVE", "1")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "services diff --json failed; stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let v: Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("services JSON: {e}\n{stdout}"));
    assert_eq!(v["schemaVersion"], 1);
    let results = v["results"]
        .as_array()
        .unwrap_or_else(|| panic!("results[]: {stdout}"));
    assert!(
        results.iter().any(|row| row["service"] == "billing-api"),
        "expected named billing-api row, got: {stdout}"
    );
    assert!(
        v.get("emptyReason").is_none(),
        "must not be empty: {stdout}"
    );
}

fn write_http_fixture(root: &std::path::Path) {
    fs::create_dir_all(root.join("src").join("billing")).unwrap();
    fs::write(root.join("src").join("lib.rs"), "pub mod billing;\n").unwrap();
    fs::write(
        root.join("src").join("billing").join("mod.rs"),
        r#"
use axum::{routing::get, Router};
async fn health() {}
pub fn app() -> Router {
    Router::new().route("/billing/health", get(health))
}
"#,
    )
    .unwrap();
}

fn run_bin(root: &std::path::Path, args: &[&str]) -> (String, String, bool) {
    let exe = env!("CARGO_BIN_EXE_ledgerful");
    let out = Command::new(exe)
        .args(args)
        .current_dir(root)
        .env("LEDGERFUL_NON_INTERACTIVE", "1")
        .output()
        .unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.success(),
    )
}

#[test]
#[allow(non_snake_case)]
fn services_gated_preview_json_populated__slow() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    write_http_fixture(root);
    git_add_and_commit(root, "initial");

    let (init_out, init_err, init_ok) = run_bin(root, &["init"]);
    assert!(init_ok, "init failed: stdout={init_out} stderr={init_err}");

    fs::write(
        root.join(".ledgerful").join("config.toml"),
        r#"
[coverage]
enabled = false

[coverage.services]
enabled = true

[[services.definitions]]
name = "billing-api"
root = "src/billing"
"#,
    )
    .unwrap();

    let (idx_out, idx_err, idx_ok) = run_bin(root, &["index", "--incremental"]);
    assert!(
        idx_ok,
        "index --incremental failed: stdout={idx_out} stderr={idx_err}"
    );

    let (pstdout, pstderr, pok) = run_bin(root, &["services", "list", "--preview", "--json"]);
    assert!(pok, "preview json failed: stderr={pstderr}");
    let pv: Value = serde_json::from_str(pstdout.trim())
        .unwrap_or_else(|e| panic!("preview JSON: {e}\n{pstdout}"));
    assert_eq!(pv["preview"], true);
    assert_eq!(pv["inferenceState"], "disabledGlobally");
    assert_eq!(pv["schemaVersion"], 1);
    let results = pv["results"]
        .as_array()
        .unwrap_or_else(|| panic!("results: {pstdout}"));
    assert!(
        !results.is_empty(),
        "preview must populate results: {pstdout}"
    );
    assert!(
        results.iter().any(|row| row["service"] == "billing-api"),
        "expected billing-api, got: {pstdout}"
    );

    let layout = ledgerful::state::layout::Layout::new(root.to_string_lossy().as_ref());
    assert!(
        !layout.cli_session_file().is_file(),
        "preview must not write cli-session.json"
    );

    let (stdout, stderr, ok) = run_bin(root, &["services", "list", "--json"]);
    assert!(ok, "services list --json failed: stderr={stderr}");
    let v: Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("services JSON: {e}\n{stdout}"));
    assert_eq!(v["emptyReason"], "disabledByConfig");
    assert_eq!(v["resultCount"], 0);
    assert_eq!(v["inferenceState"], "disabledGlobally");
    assert!(v.get("preview").is_none());
    assert_eq!(v["declared"][0]["name"], "billing-api");

    let storage = ledgerful::state::storage::StorageManager::init(
        layout.state_subdir().join("ledger.db").as_std_path(),
    )
    .unwrap();
    let assigned: i64 = storage
        .get_connection()
        .query_row(
            "SELECT COUNT(*) FROM project_files WHERE service_name IS NOT NULL",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(assigned, 0, "preview must not persist service_name");
}

#[test]
#[allow(non_snake_case)]
fn services_gated_preview_human_tokens__slow() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    write_http_fixture(root);
    git_add_and_commit(root, "initial");
    let (init_out, init_err, init_ok) = run_bin(root, &["init"]);
    assert!(init_ok, "init failed: stdout={init_out} stderr={init_err}");
    fs::write(
        root.join(".ledgerful").join("config.toml"),
        r#"
[coverage]
enabled = false

[[services.definitions]]
name = "billing-api"
root = "src/billing"
"#,
    )
    .unwrap();
    let (idx_out, idx_err, idx_ok) = run_bin(root, &["index", "--incremental"]);
    assert!(
        idx_ok,
        "index --incremental failed: stdout={idx_out} stderr={idx_err}"
    );
    let (stdout, stderr, ok) = run_bin(root, &["services", "list", "--preview"]);
    assert!(ok, "preview human failed: stderr={stderr}");
    let lower = stdout.to_lowercase();
    assert!(lower.contains("preview"), "got: {stdout}");
    assert!(lower.contains("not persisted"), "got: {stdout}");
    assert!(
        !stdout.contains('{'),
        "human preview must not dump JSON: {stdout}"
    );
}
