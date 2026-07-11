use crate::common::{DirGuard, TempEnv, git_add_and_commit, setup_git_repo};
use ledgerful::commands::config::{
    execute_config_schema, execute_config_verify, execute_config_view,
};
use ledgerful::commands::init::execute_init;
use serde_json::Value;
use std::fs;
use std::process::Command;
use tempfile::tempdir;

#[test]
fn test_config_verify_default() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();

    setup_git_repo(root);
    fs::write(root.join("dummy.txt"), "content").unwrap();
    git_add_and_commit(root, "initial");

    let _guard = DirGuard::new(root);
    execute_init(false, false).unwrap();

    // execute_config_verify uses Layout::new(cwd), so cwd must be the repo root
    let result = execute_config_verify(false, None, false);
    assert!(result.is_ok());
}

#[test]
fn test_config_view_shows_values() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();

    setup_git_repo(root);
    fs::write(root.join("dummy.txt"), "content").unwrap();
    git_add_and_commit(root, "initial");

    let _guard = DirGuard::new(root);
    execute_init(false, false).unwrap();

    let result = execute_config_view(false, None, None);
    assert!(result.is_ok());
}

#[test]
fn test_config_schema_output() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();

    setup_git_repo(root);
    fs::write(root.join("dummy.txt"), "content").unwrap();
    git_add_and_commit(root, "initial");

    let _guard = DirGuard::new(root);
    execute_init(false, false).unwrap();

    let result = execute_config_schema(false);
    assert!(result.is_ok());
}

/// CG-F35 (requirement #4, #7): `config diff` must separate "referenced in
/// production code but undeclared" from "referenced only from test/example
/// files but undeclared" rather than mixing both into one undifferentiated
/// list. The test/example-only signal must stay visible (not hidden behind
/// a flag) in both human and JSON output, just clearly separated.
#[test]
fn test_config_diff_separates_test_only_from_production_missing_declarations() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();

    setup_git_repo(root);
    fs::create_dir_all(root.join("src")).unwrap();
    fs::create_dir_all(root.join("tests")).unwrap();
    fs::write(
        root.join("src/main.rs"),
        r#"fn main() { let _ = std::env::var("PROD_ONLY_VAR"); }"#,
    )
    .unwrap();
    fs::write(
        root.join("tests/some_test.rs"),
        r#"fn helper() { let _ = std::env::var("TEST_ONLY_VAR"); }"#,
    )
    .unwrap();
    git_add_and_commit(root, "initial");

    let exe = env!("CARGO_BIN_EXE_ledgerful");
    Command::new(exe)
        .arg("init")
        .current_dir(root)
        .output()
        .unwrap();
    let index_out = Command::new(exe)
        .args(["index", "--incremental"])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        index_out.status.success(),
        "index failed: {}",
        String::from_utf8_lossy(&index_out.stderr)
    );

    // Human output: production gap stays in the primary section,
    // test-only gap appears in its own clearly-labeled section, not
    // suppressed.
    let human_out = Command::new(exe)
        .args(["config", "diff"])
        .current_dir(root)
        .output()
        .unwrap();
    let human_stdout = String::from_utf8_lossy(&human_out.stdout);
    assert!(
        human_stdout.contains("PROD_ONLY_VAR"),
        "expected production-only var in output: {human_stdout}"
    );
    assert!(
        human_stdout.contains("TEST_ONLY_VAR"),
        "expected test-only var to remain visible (not hidden), got: {human_stdout}"
    );
    assert!(
        human_stdout.contains("Test/example-only references"),
        "expected a clearly-labeled lower-priority section, got: {human_stdout}"
    );

    // JSON output: both sections present and machine-readable, with no
    // need for a special flag to see test-only entries.
    let json_out = Command::new(exe)
        .args(["config", "diff", "--json"])
        .current_dir(root)
        .output()
        .unwrap();
    let v: Value = serde_json::from_slice(&json_out.stdout).unwrap();

    let prod_vars: Vec<&str> = v["missing_declarations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["var_name"].as_str().unwrap())
        .collect();
    assert!(
        prod_vars.contains(&"PROD_ONLY_VAR"),
        "expected PROD_ONLY_VAR in missing_declarations: {v}"
    );
    assert!(
        !prod_vars.contains(&"TEST_ONLY_VAR"),
        "TEST_ONLY_VAR must not be mixed into the production section: {v}"
    );

    let test_only_vars: Vec<&str> = v["missing_declarations_test_or_example_only"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["var_name"].as_str().unwrap())
        .collect();
    assert!(
        test_only_vars.contains(&"TEST_ONLY_VAR"),
        "expected TEST_ONLY_VAR reachable in JSON without a special flag: {v}"
    );
}

/// TA19 R1/R2: `config diff -v` must not enable debug-level tracing output.
/// The global `-v` flag still controls tracing for every other command, but
/// for `config diff` it must be ignored so the report stays readable.
#[test]
#[serial_test::serial(env)]
fn config_diff_global_verbose_does_not_emit_debug_tracing() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();

    setup_git_repo(root);
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("src/main.rs"),
        r#"fn main() { let _ = std::env::var("LEDGERFUL_TEST_VAR"); }"#,
    )
    .unwrap();
    git_add_and_commit(root, "initial");

    let exe = env!("CARGO_BIN_EXE_ledgerful");
    Command::new(exe)
        .arg("init")
        .current_dir(root)
        .output()
        .unwrap();
    let index_out = Command::new(exe)
        .args(["index", "--incremental"])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        index_out.status.success(),
        "index failed: {}",
        String::from_utf8_lossy(&index_out.stderr)
    );

    // Remove RUST_LOG so we are testing the `-v` flag in isolation.
    let _rust_log_guard = TempEnv::remove("RUST_LOG");
    let out = Command::new(exe)
        .args(["config", "diff", "-v"])
        .current_dir(root)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "config diff -v failed: {stderr}");
    assert!(
        !stderr.lines().any(|line| line.starts_with("DEBUG")),
        "expected no DEBUG tracing lines on stderr, got: {stderr}"
    );
    assert!(
        stdout.contains("LEDGERFUL_TEST_VAR"),
        "global -v should still act as the internal-env-var filter for config diff: {stdout}"
    );
}

/// TA19 R2/R3: `config diff --show-internal` exposes internal env vars in
/// the primary missing-declarations list instead of filtering them out.
#[test]
fn config_diff_show_internal_lists_internal_env_vars() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();

    setup_git_repo(root);
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("src/main.rs"),
        r#"fn main() { let _ = std::env::var("LEDGERFUL_TEST_VAR"); }"#,
    )
    .unwrap();
    git_add_and_commit(root, "initial");

    let exe = env!("CARGO_BIN_EXE_ledgerful");
    Command::new(exe)
        .arg("init")
        .current_dir(root)
        .output()
        .unwrap();
    let index_out = Command::new(exe)
        .args(["index", "--incremental"])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        index_out.status.success(),
        "index failed: {}",
        String::from_utf8_lossy(&index_out.stderr)
    );

    let out = Command::new(exe)
        .args(["config", "diff", "--show-internal", "--json"])
        .current_dir(root)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "config diff --show-internal --json failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();

    let missing_vars: Vec<&str> = v["missing_declarations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["var_name"].as_str().unwrap())
        .collect();
    assert!(
        missing_vars.contains(&"LEDGERFUL_TEST_VAR"),
        "expected LEDGERFUL_TEST_VAR in missing_declarations with --show-internal: {stdout}"
    );
    assert!(
        v.get("internal_env_vars").is_none(),
        "internal_env_vars key must not appear when --show-internal is set: {stdout}"
    );
}

/// TA19: `config diff` without flags keeps internal env vars out of the main
/// missing-declarations list and surfaces them in a dedicated section.
#[test]
fn config_diff_default_filters_internal_env_vars() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();

    setup_git_repo(root);
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("src/main.rs"),
        r#"fn main() { let _ = std::env::var("LEDGERFUL_TEST_VAR"); }"#,
    )
    .unwrap();
    git_add_and_commit(root, "initial");

    let exe = env!("CARGO_BIN_EXE_ledgerful");
    Command::new(exe)
        .arg("init")
        .current_dir(root)
        .output()
        .unwrap();
    let index_out = Command::new(exe)
        .args(["index", "--incremental"])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        index_out.status.success(),
        "index failed: {}",
        String::from_utf8_lossy(&index_out.stderr)
    );

    let out = Command::new(exe)
        .args(["config", "diff", "--json"])
        .current_dir(root)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "config diff --json failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();

    let missing_vars: Vec<&str> = v["missing_declarations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["var_name"].as_str().unwrap())
        .collect();
    assert!(
        !missing_vars.contains(&"LEDGERFUL_TEST_VAR"),
        "LEDGERFUL_TEST_VAR must not appear in missing_declarations by default: {stdout}"
    );

    let internal_vars: Vec<&str> = v["internal_env_vars"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["var_name"].as_str().unwrap())
        .collect();
    assert!(
        internal_vars.contains(&"LEDGERFUL_TEST_VAR"),
        "expected LEDGERFUL_TEST_VAR in internal_env_vars section by default: {stdout}"
    );
}

/// TA21 R1: internal env vars declared in `.env.example` but not referenced
/// must be removed from the default "Declared but not referenced" section.
#[test]
fn config_diff_default_filters_internal_unused_from_declared_not_referenced() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();

    setup_git_repo(root);
    fs::write(
        root.join(".env.example"),
        "OLLAMA_CLOUD_API_KEY=\nOPENROUTER_API_KEY=\nUSED_NORMAL_VAR=\n",
    )
    .unwrap();
    git_add_and_commit(root, "initial");

    let exe = env!("CARGO_BIN_EXE_ledgerful");
    Command::new(exe)
        .arg("init")
        .current_dir(root)
        .output()
        .unwrap();
    let index_out = Command::new(exe)
        .args(["index", "--incremental"])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        index_out.status.success(),
        "index failed: {}",
        String::from_utf8_lossy(&index_out.stderr)
    );

    let out = Command::new(exe)
        .args(["config", "diff", "--json"])
        .current_dir(root)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "config diff --json failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();

    let unused_vars: Vec<&str> = v["unused_declarations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry.as_str().unwrap())
        .collect();
    assert!(
        !unused_vars.contains(&"OLLAMA_CLOUD_API_KEY"),
        "OLLAMA_CLOUD_API_KEY must not appear in unused_declarations by default: {stdout}"
    );
    assert!(
        !unused_vars.contains(&"OPENROUTER_API_KEY"),
        "OPENROUTER_API_KEY must not appear in unused_declarations by default: {stdout}"
    );
    assert!(
        unused_vars.contains(&"USED_NORMAL_VAR"),
        "USED_NORMAL_VAR should remain in unused_declarations: {stdout}"
    );

    let internal_entries: Vec<&Value> = v["internal_env_vars"].as_array().unwrap().iter().collect();
    let internal_names: Vec<&str> = internal_entries
        .iter()
        .map(|entry| entry["var_name"].as_str().unwrap())
        .collect();
    assert!(
        internal_names.contains(&"OLLAMA_CLOUD_API_KEY"),
        "expected OLLAMA_CLOUD_API_KEY in internal_env_vars by default: {stdout}"
    );
    assert!(
        internal_names.contains(&"OPENROUTER_API_KEY"),
        "expected OPENROUTER_API_KEY in internal_env_vars by default: {stdout}"
    );

    let ollama_entry = internal_entries
        .iter()
        .find(|entry| entry["var_name"].as_str() == Some("OLLAMA_CLOUD_API_KEY"))
        .unwrap();
    assert_eq!(
        ollama_entry["file_paths"].as_array().unwrap().len(),
        0,
        "declared-but-unreferenced internal var should have empty file_paths: {stdout}"
    );
    assert_eq!(
        ollama_entry["note"].as_str(),
        Some("declared but not directly referenced"),
        "declared-but-unreferenced internal var should carry the note: {stdout}"
    );

    let human_out = Command::new(exe)
        .args(["config", "diff"])
        .current_dir(root)
        .output()
        .unwrap();
    let human_stdout = String::from_utf8_lossy(&human_out.stdout);
    assert!(
        human_stdout.contains("Internal env vars"),
        "human output must show Internal env vars section: {human_stdout}"
    );
    assert!(
        human_stdout.contains("OLLAMA_CLOUD_API_KEY"),
        "human output must list OLLAMA_CLOUD_API_KEY in the internal section: {human_stdout}"
    );
    assert!(
        human_stdout.contains("declared but not directly referenced"),
        "human output must annotate declared-but-unreferenced internal vars: {human_stdout}"
    );
    // The var must not appear in the "Declared but not referenced" tail.
    let unused_tail = human_stdout
        .split("Declared but not referenced in code:")
        .nth(1)
        .unwrap_or("");
    assert!(
        !unused_tail.contains("OLLAMA_CLOUD_API_KEY"),
        "OLLAMA_CLOUD_API_KEY must not be in the declared-but-unreferenced tail: {human_stdout}"
    );
}

/// TA21 R1: `--show-internal` keeps internal unused vars in the
/// "Declared but not referenced" section.
#[test]
fn config_diff_show_internal_lists_internal_unused_in_declared_not_referenced() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();

    setup_git_repo(root);
    fs::write(
        root.join(".env.example"),
        "OLLAMA_CLOUD_API_KEY=\nOPENROUTER_API_KEY=\nUSED_NORMAL_VAR=\n",
    )
    .unwrap();
    git_add_and_commit(root, "initial");

    let exe = env!("CARGO_BIN_EXE_ledgerful");
    Command::new(exe)
        .arg("init")
        .current_dir(root)
        .output()
        .unwrap();
    let index_out = Command::new(exe)
        .args(["index", "--incremental"])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        index_out.status.success(),
        "index failed: {}",
        String::from_utf8_lossy(&index_out.stderr)
    );

    let out = Command::new(exe)
        .args(["config", "diff", "--show-internal", "--json"])
        .current_dir(root)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "config diff --show-internal --json failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();

    let unused_vars: Vec<&str> = v["unused_declarations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry.as_str().unwrap())
        .collect();
    assert!(
        unused_vars.contains(&"OLLAMA_CLOUD_API_KEY"),
        "expected OLLAMA_CLOUD_API_KEY in unused_declarations with --show-internal: {stdout}"
    );
    assert!(
        unused_vars.contains(&"OPENROUTER_API_KEY"),
        "expected OPENROUTER_API_KEY in unused_declarations with --show-internal: {stdout}"
    );
    assert!(
        v.get("internal_env_vars").is_none(),
        "internal_env_vars key must not appear when --show-internal is set: {stdout}"
    );
}

/// TA21 R1 explicit-declaration exception: an internal env var that is
/// formally declared in `.ledgerful/schema.json` must stay in the default
/// "Declared but not referenced" section so the user gets type-enforcement
/// feedback.
#[test]
fn config_diff_default_does_not_filter_explicitly_declared_internal_unused() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();

    setup_git_repo(root);
    fs::write(
        root.join(".env.example"),
        "EXPLICIT_LEDGERFUL_VAR=\nOLLAMA_CLOUD_API_KEY=\n",
    )
    .unwrap();
    git_add_and_commit(root, "initial");

    let exe = env!("CARGO_BIN_EXE_ledgerful");
    Command::new(exe)
        .arg("init")
        .current_dir(root)
        .output()
        .unwrap();

    fs::write(
        root.join(".ledgerful/schema.json"),
        r#"{"EXPLICIT_LEDGERFUL_VAR": "explicit"}"#,
    )
    .unwrap();

    let index_out = Command::new(exe)
        .args(["index", "--incremental"])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        index_out.status.success(),
        "index failed: {}",
        String::from_utf8_lossy(&index_out.stderr)
    );

    let out = Command::new(exe)
        .args(["config", "diff", "--json"])
        .current_dir(root)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "config diff --json failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();

    let unused_vars: Vec<&str> = v["unused_declarations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry.as_str().unwrap())
        .collect();
    assert!(
        unused_vars.contains(&"EXPLICIT_LEDGERFUL_VAR"),
        "explicitly-declared internal var must remain in unused_declarations: {stdout}"
    );

    let internal_names: Vec<&str> = v["internal_env_vars"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["var_name"].as_str().unwrap())
        .collect();
    assert!(
        !internal_names.contains(&"EXPLICIT_LEDGERFUL_VAR"),
        "explicitly-declared internal var must not be moved to internal_env_vars: {stdout}"
    );
    assert!(
        internal_names.contains(&"OLLAMA_CLOUD_API_KEY"),
        "implicitly-declared internal var should still be filtered to internal_env_vars: {stdout}"
    );
}
