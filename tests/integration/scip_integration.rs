use std::process::Command;

#[test]
fn test_scip_cli_wiring() {
    let binary_path = env!("CARGO_BIN_EXE_ledgerful");
    let tmp = tempfile::tempdir().unwrap();

    // Initialize a minimal git repo
    std::process::Command::new("git")
        .arg("init")
        .current_dir(tmp.path())
        .output()
        .unwrap();

    // Create the state directory so SQLite/CozoDB can create the database file
    std::fs::create_dir_all(tmp.path().join(".ledgerful/state")).unwrap();

    // Running with a non-existent SCIP file should fail gracefully
    let output = Command::new(binary_path)
        .args(["index", "--scip", "non_existent.scip"])
        .current_dir(tmp.path())
        .output()
        .expect("Failed to execute ledgerful index");

    // It should fail with an error about the file not existing
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    println!("STDOUT: {}", stdout);
    println!("STDERR: {}", stderr);
    assert!(!output.status.success());
    assert!(stderr.contains("non_existent.scip") || stdout.contains("non_existent.scip"));
}
