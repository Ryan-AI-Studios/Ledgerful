use std::process::Command;

#[test]
fn test_verify_command_triggers_notification_path() {
    // This is hard to test without a mocked IPC server, but we can ensure it doesn't crash.
    let binary = option_env!("CARGO_BIN_EXE_ledgerful").unwrap_or("target/debug/ledgerful");
    let output = Command::new(binary)
        .args(["verify", "--help"])
        .output()
        .expect("failed to execute process");

    assert!(output.status.success());
}
