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
