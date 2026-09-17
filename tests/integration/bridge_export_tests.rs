#![allow(non_snake_case)]

use std::fs;
use std::process::Command;
use tempfile::tempdir;

fn bin() -> &'static str {
    option_env!("CARGO_BIN_EXE_ledgerful").unwrap_or("target/debug/ledgerful")
}

fn git_init_commit(dir: &std::path::Path) {
    assert!(
        Command::new("git")
            .arg("init")
            .current_dir(dir)
            .output()
            .unwrap()
            .status
            .success()
    );
    assert!(
        Command::new("git")
            .args([
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.com",
                "commit",
                "--allow-empty",
                "-m",
                "init",
            ])
            .current_dir(dir)
            .output()
            .unwrap()
            .status
            .success()
    );
}

#[test]
fn test_bridge_export_subcommand_exists() {
    let output = Command::new(bin())
        .args(["bridge", "export", "--help"])
        .output()
        .expect("failed to execute process");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("export"));
    assert!(
        stdout.contains("comma") || stdout.contains("prefixes") || stdout.contains("scope"),
        "scope help should mention prefixes/comma: {stdout}"
    );
    assert!(
        !stdout.contains("as BridgeRecord NDJSON")
            || stdout.contains("NDJSON-compatible")
            || stdout.contains("JSON snapshot"),
        "help about should not claim NDJSON-only: {stdout}"
    );
}

#[test]
fn bridge_export_scope_without_hotspots__clap_error() {
    let output = Command::new(bin())
        .args(["bridge", "export", "--scope", "src/", "--stdout"])
        .output()
        .expect("run");
    assert!(
        !output.status.success(),
        "--scope without --hotspots must fail"
    );
}

#[test]
fn bridge_export_stdout_and_out_mutex() {
    let dir = tempdir().unwrap();
    git_init_commit(dir.path());
    let out = dir.path().join("x.json");
    let output = Command::new(bin())
        .args([
            "bridge",
            "export",
            "--stdout",
            "--out",
            out.to_str().unwrap(),
        ])
        .current_dir(dir.path())
        .output()
        .expect("run");
    assert!(!output.status.success(), "stdout+out must error");
    let err = String::from_utf8_lossy(&output.stderr);
    assert!(
        err.contains("--stdout cannot be combined") || err.contains("cannot be combined"),
        "stderr: {err}"
    );
}

#[test]
#[allow(non_snake_case)]
fn test_bridge_export_file_creation__slow() {
    let dir = tempdir().unwrap();

    // Initialize a minimal git repo so bridge export can discover the project.
    let mut git_init = std::process::Command::new("git");
    git_init
        .arg("init")
        .current_dir(dir.path())
        .output()
        .unwrap();
    let mut git_commit = std::process::Command::new("git");
    git_commit
        .args([
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "--allow-empty",
            "-m",
            "init",
        ])
        .current_dir(dir.path())
        .output()
        .unwrap();

    // Initialize Ledgerful state in the temp directory.
    let binary = option_env!("CARGO_BIN_EXE_ledgerful").unwrap_or("target/debug/ledgerful");
    let init_output = Command::new(binary)
        .arg("init")
        .current_dir(dir.path())
        .output()
        .expect("failed to execute ledgerful init");
    assert!(
        init_output.status.success(),
        "ledgerful init failed: {:?}",
        init_output
    );

    // Run scan --impact to initialize the ledger database
    let scan_output = Command::new(binary)
        .args(["scan", "--impact"])
        .current_dir(dir.path())
        .output()
        .expect("failed to execute ledgerful scan");
    assert!(
        scan_output.status.success(),
        "ledgerful scan failed: {:?}",
        scan_output
    );

    let out_path = dir.path().join("export.ndjson");

    let output = Command::new(binary)
        .args(["bridge", "export", "--out", out_path.to_str().unwrap()])
        .current_dir(dir.path())
        .output()
        .expect("failed to execute process");

    assert!(
        output.status.success(),
        "bridge export failed: {:?}",
        output
    );
    assert!(out_path.exists());

    let content = fs::read_to_string(&out_path).unwrap();
    // Should be valid NDJSON if records were exported
    if !content.is_empty() {
        assert!(content.contains(r#""type":"#));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Exported bridge snapshot to"),
        "human file dest banner: {stdout}"
    );
}

#[test]
fn bridge_export_json_out__empty_stdout_and_datasets() {
    let dir = tempdir().unwrap();
    git_init_commit(dir.path());
    let init = Command::new(bin())
        .arg("init")
        .current_dir(dir.path())
        .output()
        .expect("init");
    assert!(init.status.success(), "init: {:?}", init);
    let out_path = dir.path().join("export.json");
    let output = Command::new(bin())
        .args([
            "bridge",
            "export",
            "--json",
            "--madr",
            "--out",
            out_path.to_str().unwrap(),
        ])
        .current_dir(dir.path())
        .output()
        .expect("export");
    assert!(output.status.success(), "export: {:?}", output);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.trim().is_empty(),
        "--json --out must leave stdout empty, got {stdout:?}"
    );
    let raw = fs::read_to_string(&out_path).unwrap();
    let v: serde_json::Value = serde_json::from_str(&raw).expect("json");
    assert_eq!(v["bridge_version"], "0.3");
    assert_eq!(v["record_kind"], "snapshot");
    let datasets = v["payload"]["datasets"].as_array().expect("datasets");
    assert!(
        datasets
            .iter()
            .any(|d| d["name"] == "impact" && d["count"] == 1),
        "{datasets:?}"
    );
    assert!(
        datasets.iter().any(|d| d["name"] == "madr"
            && d["emptyReason"] == "notWired"
            && d["next"] == "ledgerful ledger adr export"),
        "{datasets:?}"
    );
}

#[test]
fn bridge_export_json_alone__stdout_not_default_file() {
    let dir = tempdir().unwrap();
    git_init_commit(dir.path());
    let init = Command::new(bin())
        .arg("init")
        .current_dir(dir.path())
        .output()
        .expect("init");
    assert!(init.status.success(), "init: {:?}", init);
    let output = Command::new(bin())
        .args(["bridge", "export", "--json"])
        .current_dir(dir.path())
        .output()
        .expect("export");
    assert!(output.status.success(), "export: {:?}", output);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("bridge_version"),
        "--json alone should print the record: {stdout}"
    );
    let default_path = dir
        .path()
        .join(".ledgerful")
        .join("state")
        .join("bridge-export.json");
    assert!(
        !default_path.exists(),
        "--json alone must not write the default dest"
    );
}

#[test]
fn bridge_export_dash_out__stdout() {
    let dir = tempdir().unwrap();
    git_init_commit(dir.path());
    let init = Command::new(bin())
        .arg("init")
        .current_dir(dir.path())
        .output()
        .expect("init");
    assert!(init.status.success(), "init: {:?}", init);
    let output = Command::new(bin())
        .args(["bridge", "export", "-o", "-", "--hotspots"])
        .current_dir(dir.path())
        .output()
        .expect("export");
    assert!(output.status.success(), "export: {:?}", output);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("bridge_version"),
        "-o - must print the record, not a file named -: {stdout}"
    );
    assert!(
        !dir.path().join("-").exists(),
        "-o - must not create a file named -"
    );
}

#[test]
fn bridge_export_compact_stdout__import_round_trip() {
    let dir = tempdir().unwrap();
    git_init_commit(dir.path());
    let init = Command::new(bin())
        .arg("init")
        .current_dir(dir.path())
        .output()
        .expect("init");
    assert!(init.status.success(), "init: {:?}", init);
    let output = Command::new(bin())
        .args(["bridge", "export", "--hotspots", "--ledger", "--stdout"])
        .current_dir(dir.path())
        .output()
        .expect("export");
    assert!(output.status.success(), "export: {:?}", output);
    let stdout = output.stdout;
    let snap = dir.path().join("snap.ndjson");
    fs::write(&snap, &stdout).unwrap();
    let import = Command::new(bin())
        .args(["bridge", "import", "--input", snap.to_str().unwrap()])
        .current_dir(dir.path())
        .output()
        .expect("import");
    assert!(import.status.success(), "import round-trip: {:?}", import);
}
