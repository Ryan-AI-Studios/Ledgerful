use std::process::Command;

#[test]
fn test_doctor_runs_on_all_platforms() {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_ledgerful"));
    cmd.arg("doctor");

    let output = cmd.output().expect("Failed to execute process");
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        output.status.success(),
        "Command failed with status: {}\nStdout: {}\nStderr: {}",
        output.status,
        stdout,
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains("LEDGERFUL_PLATFORM:"),
        "Output must contain platform telemetry"
    );
    assert!(
        stdout.contains("target_triple="),
        "Output must contain target_triple"
    );
    assert!(stdout.contains("family="), "Output must contain family");
}
