//! Integration tests for `dependencies list` (track 0153).
//!
//! Default list is live Cargo.toml + Cargo.lock (root package deps array for
//! locked versions). No Cozo seed required.

use crate::common::{DirGuard, git_add_and_commit, setup_git_repo};
use std::fs;
use std::process::Command;
use tempfile::tempdir;

const LEDGERFUL_BIN: &str = env!("CARGO_BIN_EXE_ledgerful");

fn write_cargo_project(root: &std::path::Path, toml: &str, lock: Option<&str>) {
    fs::write(root.join("Cargo.toml"), toml).unwrap();
    if let Some(lock) = lock {
        fs::write(root.join("Cargo.lock"), lock).unwrap();
    }
}

const FIXTURE_TOML: &str = r#"
[package]
name = "fixture-app"
version = "1.2.3"
edition = "2021"

[dependencies]
direct-a = "1.0"
multi-ver = "2"
shared-kind = "1"

[dev-dependencies]
shared-kind = "1"
dev-only = "0.5"

[build-dependencies]
build-only = "0.1"
"#;

/// Root pins multi-ver@2.0.0 while lock also has multi-ver@1.0.0 (H1).
const FIXTURE_LOCK: &str = r#"
version = 3

[[package]]
name = "build-only"
version = "0.1.0"
source = "registry+https://github.com/rust-lang/crates.io-index"

[[package]]
name = "dev-only"
version = "0.5.0"
source = "registry+https://github.com/rust-lang/crates.io-index"

[[package]]
name = "direct-a"
version = "1.0.0"
source = "registry+https://github.com/rust-lang/crates.io-index"

[[package]]
name = "fixture-app"
version = "1.2.3"
dependencies = [
 "build-only 0.1.0",
 "dev-only 0.5.0",
 "direct-a 1.0.0",
 "multi-ver 2.0.0",
 "shared-kind 1.2.3",
]

[[package]]
name = "multi-ver"
version = "1.0.0"
source = "registry+https://github.com/rust-lang/crates.io-index"

[[package]]
name = "multi-ver"
version = "2.0.0"
source = "registry+https://github.com/rust-lang/crates.io-index"

[[package]]
name = "shared-kind"
version = "1.2.3"
source = "registry+https://github.com/rust-lang/crates.io-index"

[[package]]
name = "transitive-only"
version = "9.9.9"
source = "registry+https://github.com/rust-lang/crates.io-index"
"#;

fn run_list(root: &std::path::Path, args: &[&str]) -> (bool, String, String) {
    let output = Command::new(LEDGERFUL_BIN)
        .args(args)
        .current_dir(root)
        .env("LEDGERFUL_NON_INTERACTIVE", "1")
        .output()
        .unwrap();
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

#[test]
fn dependencies_list_direct_locked_versions_from_root_array() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    write_cargo_project(root, FIXTURE_TOML, Some(FIXTURE_LOCK));
    fs::write(root.join("dummy.txt"), "content").unwrap();
    git_add_and_commit(root, "initial");

    let _guard = DirGuard::new(root);

    let (ok, stdout, stderr) = run_list(root, &["dependencies", "list", "--json"]);
    assert!(ok, "stderr: {stderr}");

    let v: serde_json::Value = serde_json::from_str(&stdout).expect("pure JSON");
    assert_eq!(v["schemaVersion"], 1);
    assert_eq!(v["mode"], "direct");
    assert_eq!(v["ecosystem"], "rust/cargo");
    assert_eq!(v["root"]["name"], "fixture-app");
    assert_eq!(v["root"]["version"], "1.2.3");
    assert_eq!(v["root"]["source"], "manifest");

    let packages = v["packages"].as_array().expect("packages array");
    // direct-a, multi-ver, shared-kind (normal), build-only, shared-kind (dev), dev-only
    assert_eq!(packages.len(), 6, "packages: {packages:?}");
    assert_eq!(v["directCount"], 6);
    // lock has 8 package entries (root + 7 others including both multi-ver)
    assert_eq!(v["lockPackageCount"], 8);

    let names: Vec<&str> = packages
        .iter()
        .map(|p| p["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"direct-a"));
    assert!(names.contains(&"multi-ver"));
    assert!(!names.contains(&"transitive-only"));

    let direct_a = packages
        .iter()
        .find(|p| p["name"] == "direct-a")
        .expect("direct-a");
    assert_eq!(direct_a["version"], "1.0.0");
    assert_eq!(direct_a["kind"], "normal");
}

/// H1: lock has multi-ver@1 and multi-ver@2; root deps pin 2.0.0 only.
#[test]
fn dependencies_list_h1_multi_version_root_array_only() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    write_cargo_project(root, FIXTURE_TOML, Some(FIXTURE_LOCK));
    fs::write(root.join("dummy.txt"), "content").unwrap();
    git_add_and_commit(root, "initial");

    let _guard = DirGuard::new(root);

    let (ok, stdout, stderr) = run_list(root, &["dependencies", "list", "--json"]);
    assert!(ok, "stderr: {stderr}");

    let v: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let multi: Vec<&serde_json::Value> = v["packages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|p| p["name"] == "multi-ver")
        .collect();
    assert_eq!(
        multi.len(),
        1,
        "expected only multi-ver@2.0.0, got {multi:?}"
    );
    assert_eq!(multi[0]["version"], "2.0.0");
}

/// M2: shared-kind in [dependencies] and [dev-dependencies] → two rows.
#[test]
fn dependencies_list_dual_kind_two_rows() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    write_cargo_project(root, FIXTURE_TOML, Some(FIXTURE_LOCK));
    fs::write(root.join("dummy.txt"), "content").unwrap();
    git_add_and_commit(root, "initial");

    let _guard = DirGuard::new(root);

    let (ok, stdout, stderr) = run_list(root, &["dependencies", "list", "--json"]);
    assert!(ok, "stderr: {stderr}");

    let v: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let shared: Vec<&serde_json::Value> = v["packages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|p| p["name"] == "shared-kind")
        .collect();
    assert_eq!(shared.len(), 2, "expected normal+dev rows, got {shared:?}");
    let kinds: Vec<&str> = shared.iter().map(|p| p["kind"].as_str().unwrap()).collect();
    assert!(kinds.contains(&"normal"));
    assert!(kinds.contains(&"dev"));
    for p in &shared {
        assert_eq!(p["version"], "1.2.3");
    }
}

/// Cosmetic #3: root version always from live manifest (no Cozo involved).
#[test]
fn dependencies_list_root_version_from_manifest() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    write_cargo_project(root, FIXTURE_TOML, Some(FIXTURE_LOCK));
    fs::write(root.join("dummy.txt"), "content").unwrap();
    git_add_and_commit(root, "initial");

    let _guard = DirGuard::new(root);

    // Even with no init / empty Cozo, list succeeds with live root version.
    let (ok, stdout, stderr) = run_list(root, &["dependencies", "list", "--json"]);
    assert!(ok, "stderr: {stderr}");
    let v: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(v["root"]["version"], "1.2.3");
    assert_eq!(v["root"]["source"], "manifest");
}

#[test]
fn dependencies_list_all_json_full_lock() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    write_cargo_project(root, FIXTURE_TOML, Some(FIXTURE_LOCK));
    fs::write(root.join("dummy.txt"), "content").unwrap();
    git_add_and_commit(root, "initial");

    let _guard = DirGuard::new(root);

    let (ok, stdout, stderr) = run_list(root, &["dependencies", "list", "--all", "--json"]);
    assert!(ok, "stderr: {stderr}");

    let v: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(v["schemaVersion"], 1);
    assert_eq!(v["mode"], "all");
    let packages = v["packages"].as_array().unwrap();
    assert_eq!(
        packages.len(),
        v["lockPackageCount"].as_u64().unwrap() as usize
    );
    assert_eq!(packages.len(), 8);

    // multi-ver appears twice in --all (both lock versions)
    let multi: Vec<_> = packages
        .iter()
        .filter(|p| p["name"] == "multi-ver")
        .collect();
    assert_eq!(multi.len(), 2);

    // path/workspace style: fixture-app has no source
    let root_pkg = packages
        .iter()
        .find(|p| p["name"] == "fixture-app")
        .expect("root in lock");
    assert!(root_pkg["source"].is_null() || root_pkg.get("source").is_none());
    assert_eq!(root_pkg["relation"], "root");

    let multi_v1 = packages
        .iter()
        .find(|p| p["name"] == "multi-ver" && p["version"] == "1.0.0")
        .expect("multi-ver@1.0.0");
    assert_eq!(multi_v1["relation"], "transitive");
    let multi_v2 = packages
        .iter()
        .find(|p| p["name"] == "multi-ver" && p["version"] == "2.0.0")
        .expect("multi-ver@2.0.0");
    assert_eq!(multi_v2["relation"], "direct");

    let n = packages
        .iter()
        .filter(|p| p["relation"] == "direct")
        .count();
    let m = packages
        .iter()
        .filter(|p| p["relation"] == "transitive")
        .count();
    let roots = packages.iter().filter(|p| p["relation"] == "root").count();
    assert_eq!(n + m + roots, packages.len());
}

#[test]
fn dependencies_list_default_json_schema_envelope() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    write_cargo_project(root, FIXTURE_TOML, Some(FIXTURE_LOCK));
    fs::write(root.join("dummy.txt"), "content").unwrap();
    git_add_and_commit(root, "initial");

    let _guard = DirGuard::new(root);

    let (ok, stdout, stderr) = run_list(root, &["dependencies", "list", "--json"]);
    assert!(ok, "stderr: {stderr}");
    // Pure parse — no progress banners on stdout
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).expect("pure JSON");
    assert_eq!(v["schemaVersion"], 1);
    assert_eq!(v["mode"], "direct");
    assert!(v.get("truncated").is_none());
    for p in v["packages"].as_array().unwrap() {
        assert!(
            p.get("relation").is_none() || p["relation"].is_null(),
            "direct mode omits relation: {p}"
        );
    }
}

#[test]
fn dependencies_list_no_lock_honest_versions() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    write_cargo_project(root, FIXTURE_TOML, None);
    fs::write(root.join("dummy.txt"), "content").unwrap();
    git_add_and_commit(root, "initial");

    let _guard = DirGuard::new(root);

    let (ok, stdout, stderr) = run_list(root, &["dependencies", "list", "--json"]);
    assert!(ok, "stderr: {stderr}");

    let v: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(v["mode"], "direct");
    assert_eq!(v["lockPackageCount"], 0);
    let packages = v["packages"].as_array().unwrap();
    assert!(!packages.is_empty());
    for p in packages {
        assert!(
            p["version"].is_null() || p.get("version").is_none(),
            "expected null locked version without lock, got {p}"
        );
    }

    let (ok_h, stdout_h, _) = run_list(root, &["dependencies", "list"]);
    assert!(ok_h);
    assert!(
        stdout_h.contains("no Cargo.lock") || stdout_h.contains("Cargo.lock not found"),
        "human note missing: {stdout_h}"
    );

    let (ok_all, stdout_all, _) = run_list(root, &["dependencies", "list", "--all"]);
    assert!(ok_all);
    assert!(
        stdout_all.contains("Note: Cargo.lock not found — --all is empty."),
        "no-lock --all footnote missing: {stdout_all}"
    );
}

/// L2: non-git tempdir with Cargo.toml + lock still succeeds.
#[test]
fn dependencies_list_nongit_tempdir_succeeds() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    // Intentionally no setup_git_repo
    write_cargo_project(root, FIXTURE_TOML, Some(FIXTURE_LOCK));

    let (ok, stdout, stderr) = run_list(root, &["dependencies", "list", "--json"]);
    assert!(ok, "non-git list should succeed; stderr: {stderr}");
    let v: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(v["schemaVersion"], 1);
    assert_eq!(v["mode"], "direct");
    assert_eq!(v["root"]["name"], "fixture-app");
    assert!(v["directCount"].as_u64().unwrap() >= 1);
}

#[test]
fn dependencies_list_human_header_not_kg() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    write_cargo_project(root, FIXTURE_TOML, Some(FIXTURE_LOCK));
    fs::write(root.join("dummy.txt"), "content").unwrap();
    git_add_and_commit(root, "initial");

    let _guard = DirGuard::new(root);

    let (ok, stdout, stderr) = run_list(root, &["dependencies", "list"]);
    assert!(ok, "stderr: {stderr}");
    assert!(
        stdout.contains("Cargo.toml") || stdout.contains("direct"),
        "expected direct/manifest header: {stdout}"
    );
    assert!(
        !stdout.contains("Knowledge Graph"),
        "must not claim KG: {stdout}"
    );
    assert!(stdout.contains("direct-a"));
    assert!(stdout.contains("Direct:"));
    assert!(stdout.contains("fixture-app") || stdout.contains("1.2.3"));
}

#[test]
fn dependencies_list_missing_toml_fails() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    fs::write(root.join("dummy.txt"), "content").unwrap();
    git_add_and_commit(root, "initial");

    let _guard = DirGuard::new(root);

    let (ok, _stdout, stderr) = run_list(root, &["dependencies", "list", "--json"]);
    assert!(!ok, "missing Cargo.toml must fail");
    assert!(
        stderr.contains("Cargo.toml") || _stdout.contains("Cargo.toml"),
        "clear error expected; stderr={stderr}"
    );
}

#[test]
fn dependencies_list_all_human_relation_and_footer() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    write_cargo_project(root, FIXTURE_TOML, Some(FIXTURE_LOCK));
    fs::write(root.join("dummy.txt"), "content").unwrap();
    git_add_and_commit(root, "initial");

    let _guard = DirGuard::new(root);

    let (ok, stdout, stderr) = run_list(root, &["dependencies", "list", "--all"]);
    assert!(ok, "stderr: {stderr}");
    assert!(
        stdout.contains("Relation"),
        "expected Relation column: {stdout}"
    );
    assert!(
        stdout.contains("Package") && stdout.contains("Version") && stdout.contains("Ecosystem"),
        "expected pinned header: {stdout}"
    );
    assert!(stdout.contains("root"), "expected root relation: {stdout}");
    assert!(
        stdout.contains("direct") && stdout.contains("transitive"),
        "expected relation values: {stdout}"
    );
    assert!(
        stdout.contains("Direct:") && stdout.contains("Transitive (lock-only):"),
        "expected lock-row footer: {stdout}"
    );
    assert!(
        !stdout.contains("Knowledge Graph"),
        "must not claim KG: {stdout}"
    );
}

#[test]
fn dependencies_audit_empty_human_source_and_next() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    write_cargo_project(root, FIXTURE_TOML, Some(FIXTURE_LOCK));
    fs::write(root.join("empty-osv.json"), r#"{"results":[]}"#).unwrap();
    fs::write(root.join("dummy.txt"), "content").unwrap();
    git_add_and_commit(root, "initial");

    let _guard = DirGuard::new(root);

    let (init_ok, init_out, init_err) = run_list(root, &["init"]);
    assert!(
        init_ok,
        "ledgerful init failed: stderr={init_err} stdout={init_out}"
    );

    let (ok, stdout, stderr) = run_list(
        root,
        &["dependencies", "audit", "--input", "empty-osv.json"],
    );
    assert!(ok, "stderr: {stderr}");
    assert!(
        stdout.contains("Source: --input empty-osv.json (osv-scanner JSON)"),
        "expected interpolated source: {stdout}"
    );
    assert!(
        stdout.contains("does not scan"),
        "expected honesty: {stdout}"
    );
    assert!(
        stdout.contains("Findings: 0"),
        "expected Findings: {stdout}"
    );
    assert!(
        stdout.contains("Next: osv-scanner scan --format json -L Cargo.lock > empty-osv.json"),
        "expected official next: {stdout}"
    );
    assert!(
        stdout.contains("then ledgerful dependencies audit --input empty-osv.json"),
        "expected re-import next: {stdout}"
    );
}
