use std::process::Command;

#[test]
fn test_doctor_runs_on_all_platforms() {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_ledgerful"));
    // Isolated from the host template and provider keys (0554 DoD-isolation).
    // The run uses the engine repo as cwd, so the synthetic home lives in a temp dir.
    let home = tempfile::tempdir().expect("synthetic home");
    crate::common::isolate_cli_env(&mut cmd, home.path());
    cmd.arg("doctor");
    // Engine-root doctor must not open GitHub (0205 F14). Test-local only — not ci.yml.
    cmd.env("LEDGERFUL_NO_NETWORK", "1");

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
