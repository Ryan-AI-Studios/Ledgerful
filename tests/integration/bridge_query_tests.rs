use serde_json::Value;
use std::process::Command;

fn bin() -> &'static str {
    option_env!("CARGO_BIN_EXE_ledgerful").unwrap_or("target/debug/ledgerful")
}

#[test]
fn test_bridge_query_subcommand_exists() {
    let output = Command::new(bin())
        .args(["bridge", "query", "--help"])
        .output()
        .expect("failed to execute process");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("query"));
    assert!(
        stdout.contains("--json"),
        "0366: --json must appear on bridge query --help: {stdout}"
    );
    assert!(
        stdout.contains("<QUERY>..."),
        "0402: usage must be multi-value <QUERY>... got: {stdout}"
    );
}

#[test]
fn bridge_query_missing_query_exits_2() {
    let output = Command::new(bin())
        .args(["bridge", "query"])
        .output()
        .expect("failed to execute process");
    assert_eq!(
        output.status.code(),
        Some(2),
        "bare bridge query must keep clap exit 2, got {:?}: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn test_bridge_query_disabled_prints_hint() {
    let output = Command::new(bin())
        .env("LEDGERFUL_BRIDGE", "0")
        .args(["bridge", "query", "test query"])
        .output()
        .expect("failed to execute process");

    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stderr.contains("Bridge is disabled"),
        "expected enable hint in stderr, got: {stderr}"
    );
    assert!(
        stdout.contains("Status: disabled"),
        "0366: human disabled stdout Status: disabled, got: {stdout}"
    );
    assert!(
        !stdout.contains(stderr.trim()),
        "0366: stdout must not reprint the stderr enable hint: {stdout}"
    );
    assert!(
        stdout.contains("Next: set bridge.enabled = true"),
        "0366: human disabled stdout Next line, got: {stdout}"
    );
}

fn assert_disabled_json(stdout: &str, stderr: &str, expected_query: &str) {
    assert!(
        stderr.is_empty(),
        "0366: --json disabled stderr must be empty, got: {stderr:?}"
    );
    let trimmed = stdout.trim_start();
    assert!(
        trimmed.starts_with('{'),
        "first byte must be '{{', got: {stdout:?}"
    );
    let v: Value = serde_json::from_str(stdout.trim()).expect("json object");
    assert_eq!(v["schemaVersion"], 1);
    assert_eq!(v["kind"], "bridgeQuery");
    assert_eq!(v["status"], "disabled");
    assert_eq!(v["ok"], true);
    assert_eq!(v["query"], expected_query);
    assert!(
        !expected_query.contains("--json"),
        "query must not swallow --json: {expected_query}"
    );
    assert!(v.get("results").is_none());
    assert!(v.get("resultCount").is_none());
    assert!(v.get("source").is_none());
    assert!(v.get("providerCommand").is_none());
    assert!(
        v["next"]
            .as_str()
            .is_some_and(|n| n.contains("LEDGERFUL_BRIDGE")),
        "next must name opt-in: {v}"
    );
}

#[test]
fn test_bridge_query_json_disabled() {
    let output = Command::new(bin())
        .env("LEDGERFUL_BRIDGE", "0")
        .args(["bridge", "query", "--json", "configuration provenance"])
        .output()
        .expect("failed to execute process");

    assert!(
        output.status.success(),
        "disabled --json must exit 0: {:?}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_disabled_json(
        &String::from_utf8_lossy(&output.stdout),
        &String::from_utf8_lossy(&output.stderr),
        "configuration provenance",
    );
}

#[test]
fn bridge_query_unquoted_human_disabled() {
    let output = Command::new(bin())
        .env("LEDGERFUL_BRIDGE", "0")
        .args(["bridge", "query", "configuration", "provenance"])
        .output()
        .expect("failed to execute process");

    assert!(
        output.status.success(),
        "unquoted disabled must exit 0: {:?}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stderr.contains("Bridge is disabled"),
        "expected enable hint in stderr, got: {stderr}"
    );
    assert!(
        stdout.contains("Status: disabled"),
        "0402: unquoted human disabled stdout Status: disabled, got: {stdout}"
    );
    assert!(
        stdout.contains("Next: set bridge.enabled = true"),
        "0402: unquoted human disabled stdout Next line, got: {stdout}"
    );
}

#[test]
fn bridge_query_json_unquoted_before_tokens() {
    let output = Command::new(bin())
        .env("LEDGERFUL_BRIDGE", "0")
        .args(["bridge", "query", "--json", "configuration", "provenance"])
        .output()
        .expect("failed to execute process");

    assert!(
        output.status.success(),
        "unquoted --json before tokens must exit 0: {:?}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_disabled_json(
        &String::from_utf8_lossy(&output.stdout),
        &String::from_utf8_lossy(&output.stderr),
        "configuration provenance",
    );
}

#[test]
fn bridge_query_json_unquoted_after_tokens() {
    let output = Command::new(bin())
        .env("LEDGERFUL_BRIDGE", "0")
        .args(["bridge", "query", "configuration", "provenance", "--json"])
        .output()
        .expect("failed to execute process");

    assert!(
        output.status.success(),
        "unquoted --json after tokens must exit 0: {:?}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_disabled_json(
        &String::from_utf8_lossy(&output.stdout),
        &String::from_utf8_lossy(&output.stderr),
        "configuration provenance",
    );
}

#[test]
fn test_bridge_query_sanitization_handling() {
    let output = Command::new(bin())
        .env("LEDGERFUL_BRIDGE", "0")
        .args(["bridge", "query", "what is this?"])
        .output()
        .expect("failed to execute process");

    assert!(output.status.success());
}
