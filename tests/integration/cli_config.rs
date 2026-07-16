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

#[derive(Clone, Copy)]
struct ConfigDiffCase {
    name: &'static str,
    /// Source-file fixture: either a `src/main.rs` snippet referencing an
    /// internal var, or a `.env.example` with the given contents.
    fixture: ConfigDiffFixture,
    /// Extra command-line args beyond `config diff`.
    extra_args: &'static [&'static str],
    /// Var names that must appear in `missing_declarations`.
    expect_in_missing: &'static [&'static str],
    /// Var names that must NOT appear in `missing_declarations`.
    expect_not_in_missing: &'static [&'static str],
    /// Var names that must appear in `internal_env_vars`.
    expect_in_internal: &'static [&'static str],
    /// Var names that must NOT appear in `internal_env_vars`.
    expect_not_in_internal: &'static [&'static str],
    /// If true, the JSON must not contain an `internal_env_vars` key at all.
    expect_internal_absent: bool,
    /// Var names that must appear in `unused_declarations`.
    expect_in_unused: &'static [&'static str],
    /// Var names that must NOT appear in `unused_declarations`.
    expect_not_in_unused: &'static [&'static str],
    /// Extra JSON assertions: (var_name, expected note, expected file_paths len).
    internal_note_checks: &'static [(&'static str, &'static str, usize)],
    /// Extra human-output assertions.
    human_contains: &'static [&'static str],
    /// For declared-but-unreferenced tail checks: var names that must not
    /// appear after the "Declared but not referenced in code:" heading.
    human_unused_tail_must_not_contain: &'static [&'static str],
}

#[derive(Clone, Copy)]
enum ConfigDiffFixture {
    MainRsInternal(&'static str),
    EnvExample(&'static str),
    EnvExampleWithSchema {
        env_example: &'static str,
        schema_json: &'static str,
    },
}

/// TA19 / TA21: consolidated `config diff` internal-env-var behavior.
/// The two blocked dimensions (DEBUG stderr + test-vs-prod separation) stay as
/// standalone tests above.
#[rstest::rstest]
#[case::default_internal_filtered(ConfigDiffCase {
    name: "default_internal_filtered",
    fixture: ConfigDiffFixture::MainRsInternal(
        r#"fn main() { let _ = std::env::var("LEDGERFUL_TEST_VAR"); }"#,
    ),
    extra_args: &["--json"],
    expect_in_missing: &[],
    expect_not_in_missing: &["LEDGERFUL_TEST_VAR"],
    expect_in_internal: &["LEDGERFUL_TEST_VAR"],
    expect_not_in_internal: &[],
    expect_internal_absent: false,
    expect_in_unused: &[],
    expect_not_in_unused: &[],
    internal_note_checks: &[],
    human_contains: &[],
    human_unused_tail_must_not_contain: &[],
})]
#[case::show_internal_lists_internal(ConfigDiffCase {
    name: "show_internal_lists_internal",
    fixture: ConfigDiffFixture::MainRsInternal(
        r#"fn main() { let _ = std::env::var("LEDGERFUL_TEST_VAR"); }"#,
    ),
    extra_args: &["--show-internal", "--json"],
    expect_in_missing: &["LEDGERFUL_TEST_VAR"],
    expect_not_in_missing: &[],
    expect_in_internal: &[],
    expect_not_in_internal: &[],
    expect_internal_absent: true,
    expect_in_unused: &[],
    expect_not_in_unused: &[],
    internal_note_checks: &[],
    human_contains: &[],
    human_unused_tail_must_not_contain: &[],
})]
#[case::default_internal_unused_filtered(ConfigDiffCase {
    name: "default_internal_unused_filtered",
    fixture: ConfigDiffFixture::EnvExample(
        "OLLAMA_CLOUD_API_KEY=\nOPENROUTER_API_KEY=\nUSED_NORMAL_VAR=\n",
    ),
    extra_args: &["--json"],
    expect_in_missing: &[],
    expect_not_in_missing: &[],
    expect_in_internal: &["OLLAMA_CLOUD_API_KEY", "OPENROUTER_API_KEY"],
    expect_not_in_internal: &[],
    expect_internal_absent: false,
    expect_in_unused: &["USED_NORMAL_VAR"],
    expect_not_in_unused: &["OLLAMA_CLOUD_API_KEY", "OPENROUTER_API_KEY"],
    internal_note_checks: &[("OLLAMA_CLOUD_API_KEY", "declared but not directly referenced", 0)],
    human_contains: &[
        "Internal env vars",
        "OLLAMA_CLOUD_API_KEY",
        "declared but not directly referenced",
    ],
    human_unused_tail_must_not_contain: &["OLLAMA_CLOUD_API_KEY"],
})]
#[case::show_internal_lists_internal_unused(ConfigDiffCase {
    name: "show_internal_lists_internal_unused",
    fixture: ConfigDiffFixture::EnvExample(
        "OLLAMA_CLOUD_API_KEY=\nOPENROUTER_API_KEY=\nUSED_NORMAL_VAR=\n",
    ),
    extra_args: &["--show-internal", "--json"],
    expect_in_missing: &[],
    expect_not_in_missing: &[],
    expect_in_internal: &[],
    expect_not_in_internal: &[],
    expect_internal_absent: true,
    expect_in_unused: &["OLLAMA_CLOUD_API_KEY", "OPENROUTER_API_KEY", "USED_NORMAL_VAR"],
    expect_not_in_unused: &[],
    internal_note_checks: &[],
    human_contains: &[],
    human_unused_tail_must_not_contain: &[],
})]
#[case::explicit_schema_keeps_internal_in_unused(ConfigDiffCase {
    name: "explicit_schema_keeps_internal_in_unused",
    fixture: ConfigDiffFixture::EnvExampleWithSchema {
        env_example: "EXPLICIT_LEDGERFUL_VAR=\nOLLAMA_CLOUD_API_KEY=\n",
        schema_json: r#"{"EXPLICIT_LEDGERFUL_VAR": "explicit"}"#,
    },
    extra_args: &["--json"],
    expect_in_missing: &[],
    expect_not_in_missing: &[],
    expect_in_internal: &["OLLAMA_CLOUD_API_KEY"],
    expect_not_in_internal: &["EXPLICIT_LEDGERFUL_VAR"],
    expect_internal_absent: false,
    expect_in_unused: &["EXPLICIT_LEDGERFUL_VAR"],
    expect_not_in_unused: &[],
    internal_note_checks: &[],
    human_contains: &[],
    human_unused_tail_must_not_contain: &[],
})]
fn config_diff_internal_var_handling(#[case] case: ConfigDiffCase) {
    let tmp = tempdir().unwrap();
    let root = tmp.path();

    setup_git_repo(root);
    match case.fixture {
        ConfigDiffFixture::MainRsInternal(src) => {
            fs::create_dir_all(root.join("src")).unwrap();
            fs::write(root.join("src/main.rs"), src).unwrap();
        }
        ConfigDiffFixture::EnvExample(contents) => {
            fs::write(root.join(".env.example"), contents).unwrap();
        }
        ConfigDiffFixture::EnvExampleWithSchema {
            env_example,
            schema_json: _,
        } => {
            fs::write(root.join(".env.example"), env_example).unwrap();
        }
    }
    git_add_and_commit(root, "initial");

    let exe = env!("CARGO_BIN_EXE_ledgerful");
    Command::new(exe)
        .arg("init")
        .current_dir(root)
        .output()
        .unwrap();

    if let ConfigDiffFixture::EnvExampleWithSchema { schema_json, .. } = case.fixture {
        fs::write(root.join(".ledgerful/schema.json"), schema_json).unwrap();
    }

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

    let mut args = vec!["config", "diff"];
    args.extend(case.extra_args);
    let out = Command::new(exe)
        .args(&args)
        .current_dir(root)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "config diff {:?} failed: {}",
        case.extra_args,
        String::from_utf8_lossy(&out.stderr)
    );

    let v: Value = serde_json::from_slice(&out.stdout).unwrap();

    let missing_vars: Vec<&str> = v["missing_declarations"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .map(|entry| entry["var_name"].as_str().unwrap())
                .collect()
        })
        .unwrap_or_default();
    for var in case.expect_in_missing {
        assert!(
            missing_vars.contains(var),
            "case {}: expected {} in missing_declarations: {}",
            case.name,
            var,
            stdout
        );
    }
    for var in case.expect_not_in_missing {
        assert!(
            !missing_vars.contains(var),
            "case {}: {} must not be in missing_declarations: {}",
            case.name,
            var,
            stdout
        );
    }

    if case.expect_internal_absent {
        assert!(
            v.get("internal_env_vars").is_none(),
            "case {}: internal_env_vars key must be absent: {}",
            case.name,
            stdout
        );
    }

    let internal_entries: Vec<&Value> = v["internal_env_vars"]
        .as_array()
        .map(|arr| arr.iter().collect())
        .unwrap_or_default();
    let internal_names: Vec<&str> = internal_entries
        .iter()
        .map(|entry| entry["var_name"].as_str().unwrap())
        .collect();
    for var in case.expect_in_internal {
        assert!(
            internal_names.contains(var),
            "case {}: expected {} in internal_env_vars: {}",
            case.name,
            var,
            stdout
        );
    }
    for var in case.expect_not_in_internal {
        assert!(
            !internal_names.contains(var),
            "case {}: {} must not be in internal_env_vars: {}",
            case.name,
            var,
            stdout
        );
    }
    for (var, expected_note, expected_paths_len) in case.internal_note_checks {
        let entry = internal_entries
            .iter()
            .find(|e| e["var_name"].as_str() == Some(var))
            .unwrap_or_else(|| panic!("case {}: missing internal entry for {}", case.name, var));
        assert_eq!(
            entry["file_paths"].as_array().unwrap().len(),
            *expected_paths_len,
            "case {}: {} file_paths length mismatch: {}",
            case.name,
            var,
            stdout
        );
        assert_eq!(
            entry["note"].as_str(),
            Some(*expected_note),
            "case {}: {} note mismatch: {}",
            case.name,
            var,
            stdout
        );
    }

    let unused_vars: Vec<&str> = v["unused_declarations"]
        .as_array()
        .map(|arr| arr.iter().map(|entry| entry.as_str().unwrap()).collect())
        .unwrap_or_default();
    for var in case.expect_in_unused {
        assert!(
            unused_vars.contains(var),
            "case {}: expected {} in unused_declarations: {}",
            case.name,
            var,
            stdout
        );
    }
    for var in case.expect_not_in_unused {
        assert!(
            !unused_vars.contains(var),
            "case {}: {} must not be in unused_declarations: {}",
            case.name,
            var,
            stdout
        );
    }

    if !case.human_contains.is_empty() || !case.human_unused_tail_must_not_contain.is_empty() {
        let human_out = Command::new(exe)
            .args(["config", "diff"])
            .current_dir(root)
            .output()
            .unwrap();
        let human_stdout = String::from_utf8_lossy(&human_out.stdout);
        for text in case.human_contains {
            assert!(
                human_stdout.contains(*text),
                "case {}: human output missing {:?}: {}",
                case.name,
                text,
                human_stdout
            );
        }
        let unused_tail = human_stdout
            .split("Declared but not referenced in code:")
            .nth(1)
            .unwrap_or("");
        for var in case.human_unused_tail_must_not_contain {
            assert!(
                !unused_tail.contains(*var),
                "case {}: {} must not be in declared-but-unreferenced tail: {}",
                case.name,
                var,
                human_stdout
            );
        }
    }
}
