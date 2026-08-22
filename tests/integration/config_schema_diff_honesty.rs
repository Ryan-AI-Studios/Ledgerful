//! 0216 — config schema required/source_kind honesty + diff OS/git ignore
//! + inline `#[cfg(test)]` module env literals are not production gaps.

use crate::common::{git_add_and_commit, setup_git_repo};
use serde_json::Value;
use std::fs;
use std::path::Path;
use tempfile::tempdir;

fn cargo_root() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn run_cli(dir: &Path, args: &[&str]) -> (String, String, i32) {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_ledgerful"))
        .args(args)
        .current_dir(dir)
        .env("LEDGERFUL_NON_INTERACTIVE", "1")
        .output()
        .expect("failed to run ledgerful");
    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
        output.status.code().unwrap_or(-1),
    )
}

fn missing_names(v: &Value) -> Vec<&str> {
    v["missing_declarations"]
        .as_array()
        .unwrap_or_else(|| panic!("missing_declarations must be an array: {v}"))
        .iter()
        .map(|entry| entry["var_name"].as_str().unwrap_or(""))
        .collect()
}

#[test]
fn config_schema_required_source_kind_and_diff_os_git_honesty() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    fs::create_dir_all(root.join("src")).unwrap();
    fs::copy(cargo_root().join(".env.example"), root.join(".env.example")).unwrap();
    fs::write(
        root.join("src").join("lib.rs"),
        r#"
pub fn prod() {
    let _ = std::env::var("APPDATA");
    let _ = std::env::var("COLUMNS");
    let _ = std::env::var("GIT_BINARY");
    let _ = std::env::var("GEMINI_FAST_MODEL");
}

#[cfg(test)]
mod tests {
    let _ = std::env::var("DATABASE_URL");
    let _ = env!("API_TOKEN");
}
"#,
    )
    .unwrap();
    git_add_and_commit(root, "initial");

    let (stdout, stderr, code) = run_cli(root, &["init"]);
    assert_eq!(code, 0, "init failed; stdout={stdout} stderr={stderr}");

    let (stdout, stderr, code) = run_cli(root, &["index", "--incremental"]);
    assert_eq!(
        code, 0,
        "index --incremental failed; stdout={stdout} stderr={stderr}"
    );

    let (stdout, stderr, code) = run_cli(root, &["config", "schema", "--json"]);
    assert_eq!(code, 0, "config schema --json; stderr={stderr}");
    let schema: Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("expected populated schema JSON object: {e}\n{stdout}"));
    let rows = schema["results"]
        .as_array()
        .unwrap_or_else(|| panic!("populated schema must expose results[], got: {stdout}"));
    let quiet = rows
        .iter()
        .find(|r| r["varName"] == "LEDGERFUL_QUIET")
        .unwrap_or_else(|| panic!("LEDGERFUL_QUIET row missing: {stdout}"));
    assert_eq!(
        quiet["required"], false,
        "LEDGERFUL_QUIET required must be false: {stdout}"
    );
    assert_eq!(
        quiet["sourceKind"], "dotenvExample",
        "sourceKind must round-trip dotenvExample: {stdout}"
    );

    let (human_schema, stderr, code) = run_cli(root, &["config", "schema"]);
    assert_eq!(code, 0, "config schema; stderr={stderr}");
    assert!(
        human_schema.contains("LEDGERFUL_QUIET"),
        "human schema must list LEDGERFUL_QUIET, got: {human_schema}"
    );
    assert!(
        human_schema.contains("DOTENV_EXAMPLE"),
        "human schema Source must print DOTENV_EXAMPLE, got: {human_schema}"
    );
    assert!(
        !human_schema.contains("YES"),
        "human schema must not print Req YES for dotenv rows, got: {human_schema}"
    );
    // Source-column token, not substring of LEDGERFUL_CONFIG_HOME / LEDGERFUL_DEFAULT_CONFIG.
    assert!(
        !human_schema
            .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .any(|tok| tok == "CONFIG"),
        "human schema must not print Source CONFIG, got: {human_schema}"
    );

    let (stdout, stderr, code) = run_cli(root, &["config", "diff", "--json"]);
    assert_eq!(code, 0, "config diff --json; stderr={stderr}");
    let diff: Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("expected config diff JSON: {e}\n{stdout}"));
    let missing = missing_names(&diff);
    for name in [
        "APPDATA",
        "COLUMNS",
        "GIT_BINARY",
        "GIT_EXEC_PATH",
        "GIT_SSH_COMMAND",
        "XDG_CONFIG_HOME",
        "DATABASE_URL",
        "API_TOKEN",
    ] {
        assert!(
            !missing.contains(&name),
            "{name} must not be a production missing declaration: {stdout}"
        );
    }
    assert!(
        missing.contains(&"GEMINI_FAST_MODEL"),
        "GEMINI_FAST_MODEL must stay a missing product declaration: {stdout}"
    );

    let (human, stderr, code) = run_cli(root, &["config", "diff"]);
    assert_eq!(code, 0, "config diff; stderr={stderr}");
    for name in ["APPDATA", "COLUMNS", "GIT_BINARY"] {
        assert!(
            !human.contains(name),
            "human config diff must not list {name}, got: {human}"
        );
    }
}
