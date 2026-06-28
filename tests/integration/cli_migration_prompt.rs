use camino::Utf8Path;
use rusqlite::Connection;
use rusqlite_migration::Migrations;
use std::process::Command;
use tempfile::tempdir;

use crate::common::setup_git_repo;

#[test]
fn test_cli_migration_prompt_non_interactive() {
    let tmp = tempdir().unwrap();
    let root = Utf8Path::from_path(tmp.path()).unwrap();
    setup_git_repo(tmp.path());

    let cg_dir = root.join(".ledgerful");
    std::fs::create_dir_all(&cg_dir).unwrap();
    let state_dir = cg_dir.join("state");
    std::fs::create_dir_all(&state_dir).unwrap();

    let db_path = state_dir.join("ledger.db");

    let mut conn = Connection::open(&db_path).unwrap();
    let first_batch = Migrations::new(ledgerful::state::migrations::m1_to_m10::m1_to_m10());
    first_batch.to_latest(&mut conn).unwrap();
    // Drop the connection so the file lock is released
    drop(conn);

    let ledgerful_bin = env!("CARGO_BIN_EXE_ledgerful");

    let output = Command::new(ledgerful_bin)
        .args(["scan", "--impact"])
        .env("NON_INTERACTIVE", "1")
        .current_dir(root)
        .output()
        .unwrap();

    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        stderr.contains("[INFO] Ledgerful database auto-migrated from v10 to v"),
        "Expected auto-migrated notice in stderr, got: {}",
        stderr
    );

    assert!(
        !stdout.contains("Proceed? [Y/n]"),
        "Stdout should not contain interactive prompt"
    );
    assert!(
        !stderr.contains("Proceed? [Y/n]"),
        "Stderr should not contain interactive prompt"
    );
}
