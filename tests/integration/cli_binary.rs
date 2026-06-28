use std::process::Command;

#[test]
fn binary_shows_ledgerful_help() {
    let binary = env!("CARGO_BIN_EXE_ledgerful");
    let output = Command::new(binary)
        .arg("--help")
        .output()
        .expect("binary should run");

    assert!(output.status.success(), "{binary} --help should succeed");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Ledgerful"),
        "{binary} --help should mention Ledgerful"
    );
    assert!(
        stdout.contains("reset"),
        "{binary} --help should list reset command"
    );
}

#[test]
fn binary_reports_package_version() {
    let binary = env!("CARGO_BIN_EXE_ledgerful");
    let output = Command::new(binary)
        .arg("--version")
        .output()
        .expect("binary should run");

    assert!(output.status.success(), "{binary} --version should succeed");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.trim().starts_with("ledgerful "),
        "{binary} --version should report package identity, got: {stdout}"
    );
}
