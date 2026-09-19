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
        stdout.contains("trailing") || stdout.contains("directory"),
        "scope help should mention trailing / for directory scoping: {stdout}"
    );
    assert!(
        !stdout.contains("as BridgeRecord NDJSON")
            || stdout.contains("NDJSON-compatible")
            || stdout.contains("JSON snapshot"),
        "help about should not claim NDJSON-only: {stdout}"
    );
    assert!(
        stdout.contains("--timeout"),
        "export --help must list --timeout: {stdout}"
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

fn git_commit_files(dir: &std::path::Path, files: &[(&str, &str)], message: &str) {
    for (rel, body) in files {
        let path = dir.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&path, body).unwrap();
    }
    assert!(
        Command::new("git")
            .args(["add", "-A"])
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
                "-m",
                message,
            ])
            .current_dir(dir)
            .output()
            .unwrap()
            .status
            .success()
    );
}

#[test]
fn bridge_export_hotspots_populated__count_and_metadata() {
    let dir = tempdir().unwrap();
    git_init_commit(dir.path());
    git_commit_files(dir.path(), &[("src/lib.rs", "fn lib() {}\n")], "files");
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
            "--hotspots",
            "--ledger",
            "--json",
            "--out",
            out_path.to_str().unwrap(),
        ])
        .current_dir(dir.path())
        .output()
        .expect("export");
    assert!(output.status.success(), "export: {:?}", output);
    let v: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&out_path).unwrap()).expect("json");
    let datasets = v["payload"]["datasets"].as_array().expect("datasets");
    let hotspots = datasets
        .iter()
        .find(|d| d["name"] == "hotspots")
        .expect("hotspots");
    let count = hotspots["count"].as_u64().expect("count");
    assert!(count > 0, "{hotspots}");
    assert!(
        hotspots["commitsWalked"].as_u64().unwrap_or(0) > 0,
        "{hotspots}"
    );
    assert!(hotspots.get("emptyReason").is_none() || hotspots["emptyReason"].is_null());
    assert_eq!(v["payload"]["metadata"]["hotspot_count"], count.to_string());
    let ledger = datasets
        .iter()
        .find(|d| d["name"] == "ledger")
        .expect("ledger");
    assert_eq!(
        v["payload"]["metadata"]["ledger_count"],
        ledger["count"].as_u64().expect("ledger count").to_string()
    );
}

#[test]
fn bridge_export_human_dataset_line__prefers_empty_reason() {
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
            "--hotspots",
            "--out",
            out_path.to_str().unwrap(),
        ])
        .current_dir(dir.path())
        .output()
        .expect("export");
    assert!(output.status.success(), "export: {:?}", output);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Dataset: hotspots 0 (noMatches)"),
        "human Dataset line must prefer emptyReason over source: {stdout}"
    );
    assert!(
        !stdout.contains("Dataset: hotspots 0 (live)"),
        "must not hide noMatches behind source=live: {stdout}"
    );
}

#[test]
fn bridge_export_hotspots_empty__no_matches() {
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
            "--hotspots",
            "--json",
            "--out",
            out_path.to_str().unwrap(),
        ])
        .current_dir(dir.path())
        .output()
        .expect("export");
    assert!(output.status.success(), "export: {:?}", output);
    let v: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&out_path).unwrap()).expect("json");
    let datasets = v["payload"]["datasets"].as_array().expect("datasets");
    let hotspots = datasets
        .iter()
        .find(|d| d["name"] == "hotspots")
        .expect("hotspots");
    assert_eq!(hotspots["emptyReason"], "noMatches");
    assert_eq!(hotspots["count"], 0);
    assert_eq!(v["payload"]["metadata"]["hotspot_count"], "0");
}

#[test]
fn bridge_export_scope_multi_prefix__cli_reaches_dir_filters() {
    let dir = tempdir().unwrap();
    git_init_commit(dir.path());
    git_commit_files(
        dir.path(),
        &[
            ("src/lib.rs", "fn src() {}\n"),
            ("docs/note.md", "# note\n"),
            ("tests/noise.rs", "fn noise() {}\n"),
        ],
        "scoped",
    );
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
            "--hotspots",
            "--scope",
            "src/,docs/",
            "--json",
            "--out",
            out_path.to_str().unwrap(),
        ])
        .current_dir(dir.path())
        .output()
        .expect("export");
    assert!(output.status.success(), "export: {:?}", output);
    let v: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&out_path).unwrap()).expect("json");
    let datasets = v["payload"]["datasets"].as_array().expect("datasets");
    let hotspots = datasets
        .iter()
        .find(|d| d["name"] == "hotspots")
        .expect("hotspots");
    assert_eq!(hotspots["filter"], "src/,docs/");
    let rows = v["payload"]["hotspots"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let paths: Vec<String> = rows
        .iter()
        .map(|h| h["path"].as_str().unwrap_or("").replace('\\', "/"))
        .collect();
    assert!(
        paths
            .iter()
            .all(|p| p.starts_with("src/") || p.starts_with("docs/")),
        "scoped export must not emit tests/: {paths:?}"
    );
    assert!(
        paths.iter().any(|p| p.starts_with("src/")) || paths.iter().any(|p| p.starts_with("docs/")),
        "scoped files must appear: {paths:?}"
    );
}

#[test]
fn bridge_export_bridge_off__exit_zero() {
    let dir = tempdir().unwrap();
    git_init_commit(dir.path());
    let init = Command::new(bin())
        .arg("init")
        .current_dir(dir.path())
        .output()
        .expect("init");
    assert!(init.status.success(), "init: {:?}", init);
    let output = Command::new(bin())
        .env("LEDGERFUL_BRIDGE", "0")
        .args(["bridge", "export", "--hotspots", "--json", "--stdout"])
        .current_dir(dir.path())
        .output()
        .expect("export");
    assert!(
        output.status.success(),
        "export when bridge off must exit 0: {:?}",
        output
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("bridge_version"), "{stdout}");
}
