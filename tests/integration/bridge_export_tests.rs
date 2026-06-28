use std::fs;
use std::process::Command;
use tempfile::tempdir;

#[test]
fn test_bridge_export_subcommand_exists() {
    let binary = option_env!("CARGO_BIN_EXE_ledgerful").unwrap_or("target/debug/ledgerful");
    let output = Command::new(binary)
        .args(["bridge", "export", "--help"])
        .output()
        .expect("failed to execute process");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("export"));
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

    let content = fs::read_to_string(out_path).unwrap();
    // Should be valid NDJSON if records were exported
    if !content.is_empty() {
        assert!(content.contains(r#""type":"#));
    }
}
