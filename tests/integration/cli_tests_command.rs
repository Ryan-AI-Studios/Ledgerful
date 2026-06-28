use crate::common::DirGuard;
use ledgerful::state::storage::StorageManager;
use std::fs;
use std::process::Command;
use tempfile::tempdir;

fn setup_db(storage: &StorageManager) {
    let conn = storage.get_connection();

    // Create schema
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS project_files (
            id INTEGER PRIMARY KEY,
            file_path TEXT UNIQUE NOT NULL,
            language TEXT,
            parse_status TEXT NOT NULL DEFAULT 'OK',
            last_indexed_at TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS project_symbols (
            id INTEGER PRIMARY KEY,
            file_id INTEGER NOT NULL REFERENCES project_files(id) ON DELETE CASCADE,
            qualified_name TEXT NOT NULL,
            symbol_name TEXT NOT NULL,
            symbol_kind TEXT NOT NULL,
            last_indexed_at TEXT NOT NULL,
            UNIQUE(file_id, qualified_name)
        );
        CREATE TABLE IF NOT EXISTS test_mapping (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            test_symbol_id INTEGER REFERENCES project_symbols(id) ON DELETE CASCADE,
            test_file_id INTEGER NOT NULL REFERENCES project_files(id) ON DELETE CASCADE,
            tested_symbol_id INTEGER REFERENCES project_symbols(id) ON DELETE CASCADE,
            tested_file_id INTEGER REFERENCES project_files(id) ON DELETE CASCADE,
            confidence REAL NOT NULL,
            mapping_kind TEXT NOT NULL,
            evidence TEXT,
            last_indexed_at TEXT NOT NULL,
            UNIQUE(test_symbol_id, test_file_id, tested_symbol_id, tested_file_id)
        );
        ",
    )
    .unwrap();

    conn.execute(
        "INSERT INTO project_files (id, file_path, last_indexed_at) VALUES (?1, ?2, '2026-01-01T00:00:00Z')",
        (1, "src/lib.rs"),
    ).unwrap();
    conn.execute(
        "INSERT INTO project_files (id, file_path, last_indexed_at) VALUES (?1, ?2, '2026-01-01T00:00:00Z')",
        (2, "tests/lib_test.rs"),
    ).unwrap();
    conn.execute(
        "INSERT INTO project_files (id, file_path, last_indexed_at) VALUES (?1, ?2, '2026-01-01T00:00:00Z')",
        (3, "src/orphan.rs"),
    ).unwrap();

    conn.execute(
        "INSERT INTO project_symbols (id, file_id, qualified_name, symbol_name, symbol_kind, last_indexed_at) \
         VALUES (?1, ?2, ?3, ?3, 'Function', '2026-01-01T00:00:00Z')",
        (1, 1, "tested_fn"),
    ).unwrap();
    conn.execute(
        "INSERT INTO project_symbols (id, file_id, qualified_name, symbol_name, symbol_kind, last_indexed_at) \
         VALUES (?1, ?2, ?3, ?3, 'Function', '2026-01-01T00:00:00Z')",
        (2, 2, "test_tested_fn"),
    ).unwrap();

    conn.execute(
        "INSERT INTO test_mapping (test_symbol_id, test_file_id, tested_symbol_id, tested_file_id, confidence, mapping_kind, last_indexed_at) \
         VALUES (?1, ?2, ?3, ?4, 1.0, 'MANUAL', '2026-01-01T00:00:00Z')",
        (2, 2, Some(1), Some(1)),
    ).unwrap();
}

fn setup_git_repo(root: &std::path::Path) {
    Command::new("git")
        .arg("init")
        .current_dir(root)
        .output()
        .unwrap();
    Command::new("git")
        .args(["config", "user.name", "Test"])
        .current_dir(root)
        .output()
        .unwrap();
    Command::new("git")
        .args(["config", "user.email", "test@test.com"])
        .current_dir(root)
        .output()
        .unwrap();
}

#[test]
fn test_cli_tests_mapped_file() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    // Restore CWD on drop (before `tmp` deletes the tempdir) so we don't leak an
    // invalid CWD to later tests in this process â€” the subprocess already runs with
    // `.current_dir(root)`, so the test-process CWD only needs to be transient.
    let _cwd_guard = DirGuard::new(root);
    let state_dir = root.join(".ledgerful").join("state");
    fs::create_dir_all(&state_dir).unwrap();
    let storage = StorageManager::init(&state_dir.join("ledger.db")).unwrap();
    setup_db(&storage);

    let ledgerful_bin = env!("CARGO_BIN_EXE_ledgerful");
    let output = Command::new(ledgerful_bin)
        .args(["tests", "src/lib.rs"])
        .current_dir(root)
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !stdout.contains("Tests validating") {
        panic!("STDOUT: {}\nSTDERR: {}", stdout, stderr);
    }
    assert!(stdout.contains("src/lib.rs"));
    assert!(stdout.contains("tests/lib_test.rs::test_tested_fn"));
}

#[test]
fn test_cli_tests_mapped_symbol() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    // Restore CWD on drop (before `tmp` deletes the tempdir) so we don't leak an
    // invalid CWD to later tests in this process â€” the subprocess already runs with
    // `.current_dir(root)`, so the test-process CWD only needs to be transient.
    let _cwd_guard = DirGuard::new(root);
    let state_dir = root.join(".ledgerful").join("state");
    fs::create_dir_all(&state_dir).unwrap();
    let storage = StorageManager::init(&state_dir.join("ledger.db")).unwrap();
    setup_db(&storage);

    let ledgerful_bin = env!("CARGO_BIN_EXE_ledgerful");
    let output = Command::new(ledgerful_bin)
        .args(["tests", "tested_fn"])
        .current_dir(root)
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Tests validating"));
    assert!(stdout.contains("tested_fn"));
    assert!(stdout.contains("tests/lib_test.rs::test_tested_fn"));
}

#[test]
fn test_cli_tests_unmapped_entity() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    // Restore CWD on drop (before `tmp` deletes the tempdir) so we don't leak an
    // invalid CWD to later tests in this process â€” the subprocess already runs with
    // `.current_dir(root)`, so the test-process CWD only needs to be transient.
    let _cwd_guard = DirGuard::new(root);
    let state_dir = root.join(".ledgerful").join("state");
    fs::create_dir_all(&state_dir).unwrap();
    let storage = StorageManager::init(&state_dir.join("ledger.db")).unwrap();
    setup_db(&storage);

    let ledgerful_bin = env!("CARGO_BIN_EXE_ledgerful");
    let output = Command::new(ledgerful_bin)
        .args(["tests", "src/orphan.rs"])
        .current_dir(root)
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("'src/orphan.rs' is indexed, but no tests currently map to it."));
}

#[test]
fn test_cli_tests_not_indexed_entity() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    // Restore CWD on drop (before `tmp` deletes the tempdir) so we don't leak an
    // invalid CWD to later tests in this process â€” the subprocess already runs with
    // `.current_dir(root)`, so the test-process CWD only needs to be transient.
    let _cwd_guard = DirGuard::new(root);
    let state_dir = root.join(".ledgerful").join("state");
    fs::create_dir_all(&state_dir).unwrap();
    let storage = StorageManager::init(&state_dir.join("ledger.db")).unwrap();
    setup_db(&storage);

    let ledgerful_bin = env!("CARGO_BIN_EXE_ledgerful");
    let output = Command::new(ledgerful_bin)
        .args(["tests", "unknown_fn"])
        .current_dir(root)
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("'unknown_fn' is not a recognized indexed file path or symbol name."));
}

#[test]
fn test_cli_tests_json_output() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    // Restore CWD on drop (before `tmp` deletes the tempdir) so we don't leak an
    // invalid CWD to later tests in this process â€” the subprocess already runs with
    // `.current_dir(root)`, so the test-process CWD only needs to be transient.
    let _cwd_guard = DirGuard::new(root);
    let state_dir = root.join(".ledgerful").join("state");
    fs::create_dir_all(&state_dir).unwrap();
    let storage = StorageManager::init(&state_dir.join("ledger.db")).unwrap();
    setup_db(&storage);

    let ledgerful_bin = env!("CARGO_BIN_EXE_ledgerful");
    let output = Command::new(ledgerful_bin)
        .args(["tests", "src/orphan.rs", "--json"])
        .current_dir(root)
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains(r#""emptyReason": "noMatches""#));

    let output2 = Command::new(ledgerful_bin)
        .args(["tests", "tested_fn", "--json"])
        .current_dir(root)
        .output()
        .unwrap();
    let stdout2 = String::from_utf8_lossy(&output2.stdout);
    assert!(stdout2.contains(r#""tests/lib_test.rs::test_tested_fn""#));
}

#[test]
fn test_cli_tests_ergonomics_and_exclusivity() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    // Restore CWD on drop (before `tmp` deletes the tempdir) so we don't leak an
    // invalid CWD to later tests in this process â€” the subprocess already runs with
    // `.current_dir(root)`, so the test-process CWD only needs to be transient.
    let _cwd_guard = DirGuard::new(root);
    let state_dir = root.join(".ledgerful").join("state");
    fs::create_dir_all(&state_dir).unwrap();
    let storage = StorageManager::init(&state_dir.join("ledger.db")).unwrap();
    setup_db(&storage);

    let ledgerful_bin = env!("CARGO_BIN_EXE_ledgerful");

    // 1. Verify --entity src/lib.rs works and matches positional
    let out_flag = Command::new(ledgerful_bin)
        .args(["tests", "--entity", "src/lib.rs"])
        .current_dir(root)
        .output()
        .unwrap();
    let stdout_flag = String::from_utf8_lossy(&out_flag.stdout);
    assert!(stdout_flag.contains("Tests validating"));
    assert!(stdout_flag.contains("src/lib.rs"));
    assert!(stdout_flag.contains("tests/lib_test.rs::test_tested_fn"));

    // 2. Running without arguments shows empty-state help and exits 0 (TA16)
    let out_none = Command::new(ledgerful_bin)
        .args(["tests"])
        .current_dir(root)
        .output()
        .unwrap();
    let stdout_none = String::from_utf8_lossy(&out_none.stdout);
    assert!(
        out_none.status.success(),
        "expected exit 0, got {:?}",
        out_none.status
    );
    assert!(
        stdout_none.contains("No entity specified.")
            || stdout_none.contains("Knowledge graph is empty."),
        "expected empty-state message, got: {stdout_none}"
    );

    // 3. Running both positional and --entity fails with clap's own conflict error
    //    (clap rejects this at parse time via `conflicts_with`, before the handler runs).
    let out_both = Command::new(ledgerful_bin)
        .args(["tests", "src/lib.rs", "--entity", "src/lib.rs"])
        .current_dir(root)
        .output()
        .unwrap();
    let stderr_both = String::from_utf8_lossy(&out_both.stderr);
    assert!(!out_both.status.success());
    assert!(
        stderr_both.contains("the argument '[POS_ENTITY]' cannot be used with '--entity <ENTITY>'")
    );

    // 4. Running audit with both positional and --entity fails with clap's own conflict error
    let out_audit_both = Command::new(ledgerful_bin)
        .args(["audit", "src/lib.rs", "--entity", "src/lib.rs"])
        .current_dir(root)
        .output()
        .unwrap();
    let stderr_audit_both = String::from_utf8_lossy(&out_audit_both.stderr);
    assert!(!out_audit_both.status.success());
    assert!(
        stderr_audit_both
            .contains("the argument '[POS_ENTITY]' cannot be used with '--entity <ENTITY>'")
    );

    // 5. Running ledger audit with both positional and --entity fails with clap's own conflict error
    let out_ledger_audit_both = Command::new(ledgerful_bin)
        .args(["ledger", "audit", "src/lib.rs", "--entity", "src/lib.rs"])
        .current_dir(root)
        .output()
        .unwrap();
    let stderr_ledger_audit_both = String::from_utf8_lossy(&out_ledger_audit_both.stderr);
    assert!(!out_ledger_audit_both.status.success());
    assert!(
        stderr_ledger_audit_both
            .contains("the argument '[POS_ENTITY]' cannot be used with '--entity <ENTITY>'")
    );

    // 6. `audit --entity src/lib.rs` (no conflict) still parses and executes successfully.
    let out_audit_flag = Command::new(ledgerful_bin)
        .args(["audit", "--entity", "src/lib.rs"])
        .current_dir(root)
        .output()
        .unwrap();
    let stdout_audit_flag = String::from_utf8_lossy(&out_audit_flag.stdout);
    let stderr_audit_flag = String::from_utf8_lossy(&out_audit_flag.stderr);
    if !out_audit_flag.status.success() {
        panic!(
            "STDOUT: {}\nSTDERR: {}",
            stdout_audit_flag, stderr_audit_flag
        );
    }
    assert!(stdout_audit_flag.contains("Audit History for"));
    assert!(stdout_audit_flag.contains("src/lib.rs"));
    assert!(!stderr_audit_flag.contains("cannot be used with"));
    assert!(!stderr_audit_flag.contains("An entity must be specified"));

    // 7. `ledger audit --entity src/lib.rs` (no conflict) still parses and executes successfully.
    let out_ledger_audit_flag = Command::new(ledgerful_bin)
        .args(["ledger", "audit", "--entity", "src/lib.rs"])
        .current_dir(root)
        .output()
        .unwrap();
    let stdout_ledger_audit_flag = String::from_utf8_lossy(&out_ledger_audit_flag.stdout);
    let stderr_ledger_audit_flag = String::from_utf8_lossy(&out_ledger_audit_flag.stderr);
    if !out_ledger_audit_flag.status.success() {
        panic!(
            "STDOUT: {}\nSTDERR: {}",
            stdout_ledger_audit_flag, stderr_ledger_audit_flag
        );
    }
    assert!(stdout_ledger_audit_flag.contains("Audit History for"));
    assert!(stdout_ledger_audit_flag.contains("src/lib.rs"));
    assert!(!stderr_ledger_audit_flag.contains("cannot be used with"));
    assert!(!stderr_ledger_audit_flag.contains("An entity must be specified"));
}
