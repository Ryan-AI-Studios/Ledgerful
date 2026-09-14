//! 0343: `index --incremental` writes `deploy_manifests` so deploy-only
//! catalog / surfaces readers are reachable.

use crate::common::{git_add_and_commit, setup_git_repo};
use serde_json::Value;
use std::fs;
use std::path::Path;
use std::process::Command;
use tempfile::tempdir;

fn ledgerful_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ledgerful")
}

fn write_coverage_config(root: &Path, coverage_enabled: bool, deploy_enabled: bool) {
    let dir = root.join(".ledgerful");
    fs::create_dir_all(&dir).unwrap();
    let content = format!(
        "[coverage]\nenabled = {coverage_enabled}\n\n[coverage.deploy]\nenabled = {deploy_enabled}\n"
    );
    fs::write(dir.join("config.toml"), content).unwrap();
}

fn seed_and_init(root: &Path) {
    setup_git_repo(root);
    fs::write(root.join("README.md"), "# temp\n").unwrap();
    git_add_and_commit(root, "initial");
    let init = Command::new(ledgerful_bin())
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
}

fn index_incremental(root: &Path) {
    let out = Command::new(ledgerful_bin())
        .args(["index", "--incremental"])
        .current_dir(root)
        .env("LEDGERFUL_NON_INTERACTIVE", "1")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "index --incremental failed: stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn configure_deploy_only_dockerfile_emits_coverage_global() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    seed_and_init(root);
    fs::write(root.join("Dockerfile"), "FROM alpine:3.20\n").unwrap();
    index_incremental(root);

    let out = Command::new(ledgerful_bin())
        .args(["configure", "--json"])
        .current_dir(root)
        .env("LEDGERFUL_NON_INTERACTIVE", "1")
        .env("LEDGERFUL_SESSION_ID", "t-0343-configure-first")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "configure --json failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let v: Value = serde_json::from_str(&stdout).unwrap_or_else(|_| {
        panic!("configure --json was not JSON: {stdout}");
    });
    let items = v["items"]
        .as_array()
        .unwrap_or_else(|| panic!("items array missing: {stdout}"));
    let global = items
        .iter()
        .find(|item| item["id"] == "coverage.global")
        .unwrap_or_else(|| panic!("coverage.global missing: {stdout}"));
    assert_eq!(global["status"], "gated", "stdout={stdout}");
    assert_eq!(global["applyArg"], "coverage.global", "stdout={stdout}");
}

#[test]
fn surfaces_deploy_ready_after_index_when_coverage_on() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    seed_and_init(root);
    write_coverage_config(root, true, true);
    fs::write(root.join("Dockerfile"), "FROM alpine:3.20\n").unwrap();
    index_incremental(root);

    let out = Command::new(ledgerful_bin())
        .args(["surfaces", "--json"])
        .current_dir(root)
        .env("LEDGERFUL_NON_INTERACTIVE", "1")
        .env("LEDGERFUL_SESSION_ID", "t-0343-surfaces")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "surfaces --json failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let v: Value = serde_json::from_str(&stdout).unwrap_or_else(|_| {
        panic!("surfaces --json was not JSON: {stdout}");
    });
    let surfaces = v["surfaces"]
        .as_array()
        .unwrap_or_else(|| panic!("surfaces array missing: {stdout}"));
    let deploy = surfaces
        .iter()
        .find(|item| item["id"] == "deploy")
        .unwrap_or_else(|| panic!("deploy surface missing: {stdout}"));
    assert_eq!(deploy["status"], "ready", "stdout={stdout}");
    assert_eq!(deploy["next"], "ledgerful deploy", "stdout={stdout}");
}
