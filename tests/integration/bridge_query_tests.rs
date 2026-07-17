use std::process::Command;

#[test]
fn test_bridge_query_subcommand_exists() {
    let binary = option_env!("CARGO_BIN_EXE_ledgerful").unwrap_or("target/debug/ledgerful");
    let output = Command::new(binary)
        .args(["bridge", "query", "--help"])
        .output()
        .expect("failed to execute process");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("query"));
}

#[test]
fn test_bridge_query_disabled_prints_hint() {
    let binary = option_env!("CARGO_BIN_EXE_ledgerful").unwrap_or("target/debug/ledgerful");
    let output = Command::new(binary)
        .env("LEDGERFUL_BRIDGE", "0")
        .args(["bridge", "query", "test query"])
        .output()
        .expect("failed to execute process");

    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Bridge is disabled"),
        "expected enable hint in stderr, got: {stderr}"
    );
}

#[test]
fn test_bridge_query_sanitization_handling() {
    let binary = option_env!("CARGO_BIN_EXE_ledgerful").unwrap_or("target/debug/ledgerful");
    let output = Command::new(binary)
        .env("LEDGERFUL_BRIDGE", "1")
        .args(["bridge", "query", "what is this?"])
        .output()
        .expect("failed to execute process");

    assert!(output.status.success());
}

#[test]
#[ignore = "requires local embedding server to be running (http://127.0.0.1:8083)"]
fn test_bridge_query_fail_open_on_missing_binary() {
    let binary = option_env!("CARGO_BIN_EXE_ledgerful").unwrap_or("target/debug/ledgerful");
    let output = Command::new(binary)
        .env("LEDGERFUL_BRIDGE", "1")
        .args(["bridge", "query", "unlikely-to-find-anything-12345"])
        .output()
        .expect("failed to execute process");

    assert!(output.status.success());
}
