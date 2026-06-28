use serial_test::serial;
use std::fs;
use std::process::Command;
use tempfile::tempdir;

use crate::common::{DirGuard, TempEnv, non_interactive, setup_git_repo};

#[test]
#[serial(env, cwd)]
fn test_verify_empty_repo_succeeds_without_cargo() {
    let _env_non_interactive = non_interactive();
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    let _guard = DirGuard::new(root);

    let ledgerful_bin = env!("CARGO_BIN_EXE_ledgerful");
    Command::new(ledgerful_bin)
        .arg("init")
        .current_dir(root)
        .output()
        .unwrap();

    let cargo_path = root.join("cargo.bat");
    fs::write(&cargo_path, "@echo off\nexit /b 1").unwrap();
    let path_env = std::env::var("PATH").unwrap_or_default();
    let new_path = format!("{};{}", root.display(), path_env);
    let _env_path = TempEnv::set("PATH", &new_path);

    let output = Command::new(ledgerful_bin)
        .arg("verify")
        .arg("--scope")
        .arg("fast")
        .current_dir(root)
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "STDOUT: {}\nSTDERR: {}",
        stdout,
        stderr
    );
    assert!(
        stdout.contains("Verification Plan"),
        "STDOUT: {}\nSTDERR: {}",
        stdout,
        stderr
    );
    assert!(
        output.status.success(),
        "STDOUT: {}\nSTDERR: {}",
        stdout,
        stderr
    );
    assert!(
        stdout.contains("Verification Plan"),
        "STDOUT: {}\nSTDERR: {}",
        stdout,
        stderr
    );
    assert!(
        !stdout.contains("Verification Steps:"),
        "STDOUT: {}\nSTDERR: {}",
        stdout,
        stderr
    );
}

#[test]
#[serial(env, cwd)]
fn test_verify_node_fixture_invokes_only_eligible_scripts() {
    let _env_non_interactive = non_interactive();
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    let _guard = DirGuard::new(root);

    fs::write(
        root.join("package.json"),
        r#"{
            "scripts": {
                "test": "echo plain_test",
                "test:ci": "echo ci_test",
                "lint": "echo lint"
            }
        }"#,
    )
    .unwrap();

    let ledgerful_bin = env!("CARGO_BIN_EXE_ledgerful");
    Command::new(ledgerful_bin)
        .arg("init")
        .current_dir(root)
        .output()
        .unwrap();

    let output = Command::new(ledgerful_bin)
        .arg("verify")
        .arg("--dry-run")
        .current_dir(root)
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "STDOUT: {}\nSTDERR: {}",
        stdout,
        stderr
    );
    assert!(
        stdout.contains("npm run lint"),
        "STDOUT: {}\nSTDERR: {}",
        stdout,
        stderr
    );
    assert!(
        stdout.contains("npm run test:ci"),
        "STDOUT: {}\nSTDERR: {}",
        stdout,
        stderr
    );
    assert!(
        !stdout.contains("npm run test\n"),
        "STDOUT: {}\nSTDERR: {}",
        stdout,
        stderr
    );
}

#[test]
#[serial(env, cwd)]
fn test_verify_deno_non_workspace_built_in_fallback() {
    let _env_non_interactive = non_interactive();
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    let _guard = DirGuard::new(root);

    fs::write(root.join("deno.json"), "{}").unwrap();

    let ledgerful_bin = env!("CARGO_BIN_EXE_ledgerful");
    Command::new(ledgerful_bin)
        .arg("init")
        .current_dir(root)
        .output()
        .unwrap();

    let output = Command::new(ledgerful_bin)
        .arg("verify")
        .arg("--dry-run")
        .current_dir(root)
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "STDOUT: {}\nSTDERR: {}",
        stdout,
        stderr
    );
    assert!(
        stdout.contains("deno lint"),
        "STDOUT: {}\nSTDERR: {}",
        stdout,
        stderr
    );
    assert!(
        stdout.contains("deno test"),
        "STDOUT: {}\nSTDERR: {}",
        stdout,
        stderr
    );
}

#[test]
#[serial(env, cwd)]
fn test_verify_deno_workspace_no_built_in_fallback() {
    let _env_non_interactive = non_interactive();
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    let _guard = DirGuard::new(root);

    fs::write(root.join("deno.json"), r#"{ "workspace": ["member"] }"#).unwrap();

    let ledgerful_bin = env!("CARGO_BIN_EXE_ledgerful");
    Command::new(ledgerful_bin)
        .arg("init")
        .current_dir(root)
        .output()
        .unwrap();

    let output = Command::new(ledgerful_bin)
        .arg("verify")
        .arg("--dry-run")
        .current_dir(root)
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "STDOUT: {}\nSTDERR: {}",
        stdout,
        stderr
    );
    assert!(
        !stdout.contains("deno test"),
        "STDOUT: {}\nSTDERR: {}",
        stdout,
        stderr
    );
    assert!(
        !stdout.contains("deno fmt"),
        "STDOUT: {}\nSTDERR: {}",
        stdout,
        stderr
    );
}

#[test]
#[serial(env, cwd)]
fn test_verify_ambiguous_manager_evidence_runs_neutral_checks() {
    let _env_non_interactive = non_interactive();
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    let _guard = DirGuard::new(root);

    fs::write(root.join("package.json"), "{}").unwrap();
    fs::write(root.join("package-lock.json"), "{}").unwrap();
    fs::write(root.join("yarn.lock"), "{}").unwrap();

    let ledgerful_bin = env!("CARGO_BIN_EXE_ledgerful");
    Command::new(ledgerful_bin)
        .arg("init")
        .current_dir(root)
        .output()
        .unwrap();

    let output = Command::new(ledgerful_bin)
        .arg("verify")
        .arg("--dry-run")
        .current_dir(root)
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "STDOUT: {}\nSTDERR: {}",
        stdout,
        stderr
    );
    assert!(
        !stdout.contains("npm"),
        "STDOUT: {}\nSTDERR: {}",
        stdout,
        stderr
    );
    assert!(
        !stdout.contains("yarn"),
        "STDOUT: {}\nSTDERR: {}",
        stdout,
        stderr
    );
}

#[test]
#[serial(env, cwd)]
fn test_verify_explicit_config_remains_exact() {
    let _env_non_interactive = non_interactive();
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    let _guard = DirGuard::new(root);

    fs::write(root.join("Cargo.toml"), "[workspace]").unwrap();
    let ledgerful_bin = env!("CARGO_BIN_EXE_ledgerful");
    Command::new(ledgerful_bin)
        .arg("init")
        .current_dir(root)
        .output()
        .unwrap();

    fs::write(
        root.join(".ledgerful/config.toml"),
        r#"
        [verify]
        mode = "explicit"
        [[verify.steps]]
        command = "echo explicit_command"
        description = "Explicit command"
        "#,
    )
    .unwrap();

    let output = Command::new(ledgerful_bin)
        .arg("verify")
        .arg("--dry-run")
        .current_dir(root)
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "STDOUT: {}\nSTDERR: {}",
        stdout,
        stderr
    );
    assert!(
        stdout.contains("echo explicit_command"),
        "STDOUT: {}\nSTDERR: {}",
        stdout,
        stderr
    );
    assert!(
        !stdout.contains("cargo fmt"),
        "STDOUT: {}\nSTDERR: {}",
        stdout,
        stderr
    );
}
