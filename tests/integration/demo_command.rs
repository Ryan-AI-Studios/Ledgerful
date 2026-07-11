#![allow(non_snake_case)]

//! Integration tests for `ledgerful demo` (Track 0039 Phase 2).
//!
//! These tests exercise the demo command end-to-end via the CLI binary,
//! verifying:
//! - The hook actually fires (non-zero ledger entries in the export).
//! - Production `~/.ledgerful/keys/` is untouched.
//! - The DEMO marker appears in the export manifest.
//! - The `--keep` flag retains the demo repo.
//! - Default behavior cleans up the demo repo.
//! - Global git config is not modified.
//! - The demo completes within the wall-clock budget.

use crate::common::non_interactive;
use serial_test::serial;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;
use tempfile::tempdir;

/// Extract every file in a zip archive into a sorted map.
fn extract_zip_members(zip_bytes: &[u8]) -> BTreeMap<String, Vec<u8>> {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(zip_bytes))
        .expect("zip archive should be readable");
    let mut out = BTreeMap::new();
    for i in 0..archive.len() {
        let mut file = archive.by_index(i).expect("zip entry should be readable");
        let mut buf = Vec::new();
        std::io::Read::read_to_end(&mut file, &mut buf)
            .expect("zip entry bytes should be readable");
        out.insert(file.name().to_string(), buf);
    }
    out
}

/// Run `ledgerful demo` with the given args, returning the (stdout, stderr, success).
fn run_demo(args: &[&str], cwd: &std::path::Path) -> (String, String, bool) {
    let binary = env!("CARGO_BIN_EXE_ledgerful");
    let output = Command::new(binary)
        .args(args)
        .current_dir(cwd)
        .env("LEDGERFUL_NON_INTERACTIVE", "1")
        .output()
        .expect("ledgerful demo binary should run");
    (
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
        output.status.success(),
    )
}

/// Hash the production keys directory contents (file names + sizes) so we can
/// detect whether the demo touched it.
fn production_keys_fingerprint() -> Vec<(String, u64)> {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    let keys_dir = home.join(".ledgerful").join("keys");
    if !keys_dir.exists() {
        return Vec::new();
    }
    let mut entries: Vec<(String, u64)> = std::fs::read_dir(&keys_dir)
        .unwrap_or_else(|_| panic!("failed to read keys dir: {keys_dir:?}"))
        .filter_map(|e| e.ok())
        .map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            let size = e.metadata().map(|m| m.len()).unwrap_or(0);
            (name, size)
        })
        .collect();
    entries.sort();
    entries
}

#[test]
#[serial(cwd, env)]
fn demo__hook_fires_and_produces_signed_entries() {
    let _ni = non_interactive();
    let tmp = tempdir().unwrap();

    // Snapshot production keys before.
    let keys_before = production_keys_fingerprint();

    let (_stdout, _stderr, success) =
        run_demo(&["demo", "--keep", "--output", "demo-repo"], tmp.path());
    assert!(success, "demo --keep should succeed");

    // Production keys untouched.
    let keys_after = production_keys_fingerprint();
    assert_eq!(
        keys_before, keys_after,
        "production keys dir must not be modified by the demo"
    );

    // The export must exist and contain non-zero entries.
    let export_path = tmp
        .path()
        .join("demo-repo")
        .join("ledgerful-DEMO-evidence.zip");
    assert!(
        export_path.exists(),
        "DEMO evidence zip must exist at {export_path:?}"
    );

    let zip_bytes = std::fs::read(&export_path).unwrap();
    let members = extract_zip_members(&zip_bytes);
    assert!(
        members.contains_key("manifest.json"),
        "manifest.json must be in the export"
    );
    assert!(
        members.contains_key("ledger.csv"),
        "ledger.csv must be in the export"
    );

    let ledger_csv = String::from_utf8(members.get("ledger.csv").unwrap().clone()).unwrap();
    let data_lines: Vec<&str> = ledger_csv
        .lines()
        .filter(|l| !l.is_empty())
        .skip(1) // header
        .collect();
    assert!(
        !data_lines.is_empty(),
        "ledger.csv must contain non-zero entries (hook actually fired); got:\n{ledger_csv}"
    );
    // DoD 3: DEMO marker in entry summaries
    assert!(
        data_lines.iter().any(|line| line.contains("[DEMO]")),
        "ledger.csv data rows must contain [DEMO] in summaries; got:\n{ledger_csv}"
    );
}

#[test]
#[serial(cwd, env)]
fn demo__manifest_contains_demo_marker() {
    let _ni = non_interactive();
    let tmp = tempdir().unwrap();

    let (_stdout, _stderr, success) = run_demo(
        &["demo", "--keep", "--output", "demo-marker-test"],
        tmp.path(),
    );
    assert!(success, "demo should succeed");

    let export_path = tmp
        .path()
        .join("demo-marker-test")
        .join("ledgerful-DEMO-evidence.zip");
    let zip_bytes = std::fs::read(&export_path).unwrap();
    let members = extract_zip_members(&zip_bytes);

    let manifest: serde_json::Value = serde_json::from_slice(members.get("manifest.json").unwrap())
        .expect("manifest.json should parse");
    let demo = manifest
        .get("demo")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    assert!(demo, "manifest.json must contain \"demo\": true");

    assert!(
        members.contains_key("index.md"),
        "index.md must be present in the demo export"
    );
}

#[test]
#[serial(cwd, env)]
fn demo__cleanup_by_default_removes_dir() {
    let _ni = non_interactive();
    let tmp = tempdir().unwrap();

    let (_stdout, _stderr, success) =
        run_demo(&["demo", "--output", "demo-cleanup-test"], tmp.path());
    assert!(success, "demo should succeed");

    let demo_dir = tmp.path().join("demo-cleanup-test");
    assert!(
        !demo_dir.exists(),
        "demo dir must be removed by default; --keep is required to retain it"
    );
}

#[test]
#[serial(cwd, env)]
fn demo__keep_retains_dir() {
    let _ni = non_interactive();
    let tmp = tempdir().unwrap();

    let (_stdout, _stderr, success) = run_demo(
        &["demo", "--keep", "--output", "demo-keep-test"],
        tmp.path(),
    );
    assert!(success, "demo --keep should succeed");

    let demo_dir = tmp.path().join("demo-keep-test");
    assert!(demo_dir.exists(), "demo dir must exist with --keep");
    assert!(
        demo_dir.join(".ledgerful").exists(),
        "demo dir must have .ledgerful state"
    );
    assert!(
        demo_dir.join("ledgerful-DEMO-evidence.zip").exists(),
        "demo dir must have the evidence export"
    );
    assert!(
        demo_dir.join(".ledgerful").join("DEMO_MARKER").exists(),
        "demo dir must have a DEMO_MARKER file for dashboard self-identification"
    );
}

#[test]
#[serial(cwd, env)]
fn demo__refuses_non_empty_without_force() {
    let _ni = non_interactive();
    let tmp = tempdir().unwrap();
    let target = tmp.path().join("existing-dir");
    std::fs::create_dir_all(&target).unwrap();
    std::fs::write(target.join("file.txt"), "x").unwrap();

    let (_stdout, stderr, success) = run_demo(&["demo", "--output", "existing-dir"], tmp.path());
    assert!(!success, "demo must refuse non-empty dir without --force");
    assert!(
        stderr.contains("already exists"),
        "error must mention already exists; got: {stderr}"
    );
}

#[test]
#[serial(cwd, env)]
fn demo__force_overwrites_non_empty() {
    let _ni = non_interactive();
    let tmp = tempdir().unwrap();
    let target = tmp.path().join("existing-dir");
    std::fs::create_dir_all(&target).unwrap();
    std::fs::write(target.join("file.txt"), "x").unwrap();

    let (_stdout, _stderr, success) = run_demo(
        &["demo", "--keep", "--force", "--output", "existing-dir"],
        tmp.path(),
    );
    assert!(success, "demo --force should overwrite non-empty dir");
    assert!(
        !target.join("file.txt").exists(),
        "old file must be removed by --force"
    );
}

#[test]
#[serial(cwd, env)]
fn demo__global_git_config_unchanged() {
    let _ni = non_interactive();
    let tmp = tempdir().unwrap();

    // Snapshot global git config before.
    let global_before = Command::new("git")
        .args(["config", "--global", "--list"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default();

    let (_stdout, _stderr, _success) =
        run_demo(&["demo", "--keep", "--output", "demo-git-test"], tmp.path());

    // Snapshot global git config after.
    let global_after = Command::new("git")
        .args(["config", "--global", "--list"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default();

    assert_eq!(
        global_before, global_after,
        "global git config must not be modified by the demo"
    );
}

#[test]
#[serial(cwd, env)]
fn demo__stdout_contains_demo_marker_and_export_path() {
    let _ni = non_interactive();
    let tmp = tempdir().unwrap();

    let (stdout, _stderr, success) = run_demo(
        &["demo", "--keep", "--output", "demo-output-test"],
        tmp.path(),
    );
    assert!(success, "demo should succeed");

    assert!(
        stdout.contains("[DEMO]"),
        "stdout must contain [DEMO] marker; got:\n{stdout}"
    );
    assert!(
        stdout.contains("ledgerful-DEMO-evidence.zip"),
        "stdout must mention the export filename; got:\n{stdout}"
    );
    assert!(
        stdout.contains("observe"),
        "stdout must mention observe mode; got:\n{stdout}"
    );
    assert!(
        stdout.contains("verify"),
        "stdout must contain a verify instruction; got:\n{stdout}"
    );
}

#[test]
#[serial(cwd, env)]
fn demo__completes_within_wall_clock_budget() {
    let _ni = non_interactive();
    let tmp = tempdir().unwrap();

    let start = std::time::Instant::now();
    let (_stdout, _stderr, success) = run_demo(
        &["demo", "--keep", "--output", "demo-budget-test"],
        tmp.path(),
    );
    let elapsed = start.elapsed();

    assert!(success, "demo should succeed");
    assert!(
        elapsed.as_secs() <= 90,
        "demo must complete within 90s ceiling; took {:?}",
        elapsed
    );
}
