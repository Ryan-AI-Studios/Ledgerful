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
    let mapped: serde_json::Value = serde_json::from_str(stdout2.trim())
        .unwrap_or_else(|e| panic!("mapped tests --json must parse: {e}; {stdout2}"));
    assert_eq!(
        mapped["schemaVersion"], 1,
        "mapped tests schemaVersion: {stdout2}"
    );
    assert!(
        mapped["resultCount"].as_u64().unwrap_or(0) >= 1,
        "mapped tests resultCount: {stdout2}"
    );
    assert!(
        mapped["mappings"].as_array().is_some(),
        "mapped tests must expose mappings[]: {stdout2}"
    );
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

    // 2. Bare `tests` is a usage error (exit 2); guidance on stderr, stdout empty (0278).
    let out_none = Command::new(ledgerful_bin)
        .args(["tests"])
        .current_dir(root)
        .output()
        .unwrap();
    let stdout_none = String::from_utf8_lossy(&out_none.stdout);
    let stderr_none = String::from_utf8_lossy(&out_none.stderr);
    assert!(
        !out_none.status.success(),
        "bare tests must refuse, got {:?}",
        out_none.status
    );
    assert_eq!(
        out_none.status.code(),
        Some(2),
        "expected exit 2, got {:?}; stdout={stdout_none}; stderr={stderr_none}",
        out_none.status
    );
    assert!(
        stdout_none.trim().is_empty(),
        "bare tests must keep stdout empty: {stdout_none}"
    );
    assert!(
        !stdout_none.contains("vendor/")
            && !stdout_none.contains("sqlite3")
            && !stdout_none.contains("schemaVersion"),
        "bare tests stdout must not list vendor/sqlite3 or emit schemaVersion: {stdout_none}"
    );
    assert!(
        stderr_none.contains("No entity specified"),
        "expected missing-entity message on stderr, got: {stderr_none}"
    );
    assert!(
        stderr_none.contains("Usage:"),
        "expected usage on stderr, got: {stderr_none}"
    );
    assert!(
        stderr_none.contains("[ENTITY]"),
        "refuse-state usage must match clap optional positional, got: {stderr_none}"
    );
    assert!(
        !stderr_none.contains("[OPTIONS] <ENTITY>"),
        "refuse-state must not claim a required <ENTITY> while clap parse is optional: {stderr_none}"
    );
    assert_no_miette_chrome(&stderr_none);

    let help = Command::new(ledgerful_bin)
        .args(["tests", "--help"])
        .current_dir(root)
        .output()
        .unwrap();
    let help_out = format!(
        "{}{}",
        String::from_utf8_lossy(&help.stdout),
        String::from_utf8_lossy(&help.stderr)
    );
    assert!(
        help.status.success(),
        "tests --help should succeed: {help_out}"
    );
    assert!(
        help_out.contains("[ENTITY]"),
        "tests --help must show optional [ENTITY] positional: {help_out}"
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
        stderr_both.contains("the argument '[ENTITY]' cannot be used with '--entity <ENTITY>'"),
        "tests conflict token must follow pos_entity value_name=ENTITY, got: {stderr_both}"
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

    // 8. Empty-state help must not cite the dead mint path `src/commands/doctor.rs` (0156 B3).
    let refuse_text = format!("{stdout_none}{stderr_none}");
    assert!(
        !refuse_text.contains("src/commands/doctor.rs"),
        "empty-state help must not cite non-indexed doctor.rs: {refuse_text}"
    );
    assert!(
        stderr_none.contains("src/commands/doctor/mod.rs"),
        "0156 examples must keep doctor/mod.rs: {stderr_none}"
    );
}

/// 0156: module-style alias `…/pkg.rs` → `…/pkg/mod.rs` is Mapped, not "not recognized".
#[test]
fn test_cli_tests_dir_module_alias() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    let _cwd_guard = DirGuard::new(root);
    let state_dir = root.join(".ledgerful").join("state");
    fs::create_dir_all(&state_dir).unwrap();
    let storage = StorageManager::init(&state_dir.join("ledger.db")).unwrap();
    // Baseline mapping keeps the table non-empty (M7); add dir-module fixture.
    setup_db(&storage);

    let conn = storage.get_connection();
    conn.execute(
        "INSERT INTO project_files (id, file_path, last_indexed_at) VALUES (10, 'src/pkg/mod.rs', '2026-01-01T00:00:00Z')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO project_files (id, file_path, last_indexed_at) VALUES (11, 'tests/pkg_test.rs', '2026-01-01T00:00:00Z')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO project_symbols (id, file_id, qualified_name, symbol_name, symbol_kind, last_indexed_at) \
         VALUES (10, 10, 'pkg_fn', 'pkg_fn', 'Function', '2026-01-01T00:00:00Z')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO project_symbols (id, file_id, qualified_name, symbol_name, symbol_kind, last_indexed_at) \
         VALUES (11, 11, 'test_pkg_fn', 'test_pkg_fn', 'Function', '2026-01-01T00:00:00Z')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO test_mapping (test_symbol_id, test_file_id, tested_symbol_id, tested_file_id, confidence, mapping_kind, last_indexed_at) \
         VALUES (11, 11, 10, 10, 1.0, 'MANUAL', '2026-01-01T00:00:00Z')",
        [],
    )
    .unwrap();

    let ledgerful_bin = env!("CARGO_BIN_EXE_ledgerful");
    let output = Command::new(ledgerful_bin)
        .args(["tests", "src/pkg.rs"])
        .current_dir(root)
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stdout.contains("not a recognized indexed file path"),
        "alias must not report EntityNotIndexed: STDOUT={stdout}\nSTDERR={stderr}"
    );
    assert!(
        stdout.contains("Tests validating"),
        "expected Mapped: STDOUT={stdout}\nSTDERR={stderr}"
    );
    // M4: header prefers resolved stored path.
    assert!(
        stdout.contains("src/pkg/mod.rs"),
        "expected resolved path in header: {stdout}"
    );
    assert!(stdout.contains("tests/pkg_test.rs::test_pkg_fn"));
}

/// 0156: multi-match suffix is Ambiguous with candidates; no index remediation (M5).
#[test]
fn test_cli_tests_ambiguous_suffix() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    let _cwd_guard = DirGuard::new(root);
    let state_dir = root.join(".ledgerful").join("state");
    fs::create_dir_all(&state_dir).unwrap();
    let storage = StorageManager::init(&state_dir.join("ledger.db")).unwrap();
    setup_db(&storage);

    let conn = storage.get_connection();
    conn.execute(
        "INSERT INTO project_files (id, file_path, last_indexed_at) VALUES (10, 'src/a/mod.rs', '2026-01-01T00:00:00Z')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO project_files (id, file_path, last_indexed_at) VALUES (11, 'src/b/mod.rs', '2026-01-01T00:00:00Z')",
        [],
    )
    .unwrap();

    let ledgerful_bin = env!("CARGO_BIN_EXE_ledgerful");
    let output = Command::new(ledgerful_bin)
        .args(["tests", "mod.rs"])
        .current_dir(root)
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("indexed paths match"),
        "expected Ambiguous message: {stdout}"
    );
    assert!(stdout.contains("src/a/mod.rs"));
    assert!(stdout.contains("src/b/mod.rs"));
    assert!(stdout.contains("Provide a more specific path"));
    assert!(
        !stdout.contains("index --incremental"),
        "Ambiguous must not suggest re-index (M5): {stdout}"
    );
    assert!(!stdout.contains("not a recognized indexed file path"));
}

fn assert_no_miette_chrome(stderr: &str) {
    for ch in ['│', '╭', '╰', '╮', '╯', '┌', '└', '┐', '┘', '─', '━', '┃'] {
        assert!(
            !stderr.contains(ch),
            "stderr must not include miette/box-drawing chrome {ch:?}: {stderr}"
        );
    }
}

/// 0278: `--json` without an entity is a usage error, not a mappings envelope.
#[test]
fn test_cli_tests_json_missing_entity_is_usage_error() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    let _cwd_guard = DirGuard::new(root);
    let state_dir = root.join(".ledgerful").join("state");
    fs::create_dir_all(&state_dir).unwrap();
    let storage = StorageManager::init(&state_dir.join("ledger.db")).unwrap();
    setup_db(&storage);

    let ledgerful_bin = env!("CARGO_BIN_EXE_ledgerful");
    let output = Command::new(ledgerful_bin)
        .args(["tests", "--json"])
        .current_dir(root)
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "tests --json without entity must refuse, got {:?}; stdout={stdout}; stderr={stderr}",
        output.status
    );
    assert_eq!(
        output.status.code(),
        Some(2),
        "expected exit 2, got {:?}; stdout={stdout}; stderr={stderr}",
        output.status
    );
    assert!(
        stdout.trim().is_empty(),
        "missing-entity --json must keep stdout empty: {stdout}"
    );
    assert!(
        !stdout.contains("schemaVersion"),
        "missing-entity --json must not emit a schemaVersion envelope: {stdout}"
    );
    let parsed = serde_json::from_str::<serde_json::Value>(stdout.trim());
    assert!(
        parsed
            .as_ref()
            .ok()
            .and_then(|v| v.get("schemaVersion"))
            .is_none(),
        "stdout must not be a parseable schemaVersion object: {stdout}"
    );
    assert!(
        stderr.contains("No entity specified"),
        "stderr must include missing-entity text, got: {stderr}"
    );
    assert!(
        stderr.contains("Usage:"),
        "stderr must include usage, got: {stderr}"
    );
    assert_no_miette_chrome(&stderr);
}

/// 0278: empty index is the same refuse path (exit 2, stderr, no envelope).
#[test]
fn test_cli_tests_empty_graph_is_usage_error() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    let _cwd_guard = DirGuard::new(root);
    let state_dir = root.join(".ledgerful").join("state");
    fs::create_dir_all(&state_dir).unwrap();
    StorageManager::init(&state_dir.join("ledger.db")).unwrap();

    let ledgerful_bin = env!("CARGO_BIN_EXE_ledgerful");
    let output = Command::new(ledgerful_bin)
        .args(["tests"])
        .current_dir(root)
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "empty-graph tests must refuse, got {:?}; stdout={stdout}; stderr={stderr}",
        output.status
    );
    assert_eq!(
        output.status.code(),
        Some(2),
        "expected exit 2, got {:?}; stdout={stdout}; stderr={stderr}",
        output.status
    );
    assert!(
        stdout.trim().is_empty(),
        "empty-graph tests must keep stdout empty: {stdout}"
    );
    assert!(
        !stdout.contains("schemaVersion"),
        "empty-graph must not emit a JSON envelope: {stdout}"
    );
    assert!(
        stderr.contains("Knowledge graph is empty"),
        "expected empty-graph message on stderr, got: {stderr}"
    );
    assert_no_miette_chrome(&stderr);

    let json = Command::new(ledgerful_bin)
        .args(["tests", "--json"])
        .current_dir(root)
        .output()
        .unwrap();
    let json_stdout = String::from_utf8_lossy(&json.stdout);
    let json_stderr = String::from_utf8_lossy(&json.stderr);
    assert!(
        !json.status.success(),
        "empty-graph --json must refuse, got {:?}",
        json.status
    );
    assert_eq!(
        json.status.code(),
        Some(2),
        "empty-graph --json expected exit 2, got {:?}; stdout={json_stdout}; stderr={json_stderr}",
        json.status
    );
    assert!(
        json_stdout.trim().is_empty() && !json_stdout.contains("schemaVersion"),
        "empty-graph --json must not emit an envelope: {json_stdout}"
    );
    assert!(
        json_stderr.contains("Knowledge graph is empty"),
        "expected empty-graph message on stderr, got: {json_stderr}"
    );
}

/// 0278: picker is mapped product files, never vendor sqlite by symbol count,
/// never in-crate `*/tests.rs`.
#[test]
fn test_cli_tests_bare_picker_excludes_vendor_and_incrate_tests_rs() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    let _cwd_guard = DirGuard::new(root);
    let state_dir = root.join(".ledgerful").join("state");
    fs::create_dir_all(&state_dir).unwrap();
    let storage = StorageManager::init(&state_dir.join("ledger.db")).unwrap();
    setup_db(&storage);

    let conn = storage.get_connection();
    conn.execute(
        "INSERT INTO project_files (id, file_path, last_indexed_at) VALUES (50, 'vendor/sqlite3-src/source/sqlite3.c', '2026-01-01T00:00:00Z')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO project_files (id, file_path, last_indexed_at) VALUES (51, 'tests/sqlite_vendor_test.rs', '2026-01-01T00:00:00Z')",
        [],
    )
    .unwrap();
    for i in 0..80 {
        conn.execute(
            "INSERT INTO project_symbols (id, file_id, qualified_name, symbol_name, symbol_kind, last_indexed_at) \
             VALUES (?1, 50, ?2, ?2, 'Function', '2026-01-01T00:00:00Z')",
            (100 + i, format!("sqlite_sym_{i}")),
        )
        .unwrap();
    }
    conn.execute(
        "INSERT INTO project_symbols (id, file_id, qualified_name, symbol_name, symbol_kind, last_indexed_at) \
         VALUES (51, 51, 'test_sqlite_vendor', 'test_sqlite_vendor', 'Function', '2026-01-01T00:00:00Z')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO test_mapping (test_symbol_id, test_file_id, tested_symbol_id, tested_file_id, confidence, mapping_kind, last_indexed_at) \
         VALUES (51, 51, 100, 50, 1.0, 'MANUAL', '2026-01-01T00:00:00Z')",
        [],
    )
    .unwrap();

    conn.execute(
        "INSERT INTO project_files (id, file_path, last_indexed_at) VALUES (60, 'src/verify/plan/tests.rs', '2026-01-01T00:00:00Z')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO project_files (id, file_path, last_indexed_at) VALUES (61, 'tests/plan_tests.rs', '2026-01-01T00:00:00Z')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO project_symbols (id, file_id, qualified_name, symbol_name, symbol_kind, last_indexed_at) \
         VALUES (60, 60, 'plan_under_test', 'plan_under_test', 'Function', '2026-01-01T00:00:00Z')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO project_symbols (id, file_id, qualified_name, symbol_name, symbol_kind, last_indexed_at) \
         VALUES (61, 61, 'test_plan_under_test', 'test_plan_under_test', 'Function', '2026-01-01T00:00:00Z')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO test_mapping (test_symbol_id, test_file_id, tested_symbol_id, tested_file_id, confidence, mapping_kind, last_indexed_at) \
         VALUES (61, 61, 60, 60, 1.0, 'MANUAL', '2026-01-01T00:00:00Z')",
        [],
    )
    .unwrap();

    let ledgerful_bin = env!("CARGO_BIN_EXE_ledgerful");
    let output = Command::new(ledgerful_bin)
        .args(["tests"])
        .current_dir(root)
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let combined = format!("{stdout}{stderr}");
    assert!(
        !output.status.success(),
        "bare tests must refuse, got {:?}; stdout={stdout}; stderr={stderr}",
        output.status
    );
    assert_eq!(output.status.code(), Some(2));
    assert!(
        stdout.trim().is_empty(),
        "bare tests must keep stdout empty: {stdout}"
    );
    assert!(
        !combined.contains("vendor/") && !combined.contains("sqlite3"),
        "picker must not list vendor sqlite: {combined}"
    );
    assert!(
        !combined.contains("src/verify/plan/tests.rs"),
        "picker must not list in-crate tests.rs: {combined}"
    );
    assert!(
        stderr.contains("src/lib.rs"),
        "picker may list mapped product src/lib.rs, got: {stderr}"
    );
    assert!(
        stderr.contains("Files with indexed test mappings (top 10):"),
        "human picker must use mapping-count header, got: {stderr}"
    );
    assert!(
        stderr.contains(" mappings"),
        "picker counts must be mappings, not symbols: {stderr}"
    );
    assert!(!stderr.contains("Available entities (top 10 by symbol count)"));
    assert!(!stderr.contains(" symbols"));
    assert_no_miette_chrome(&stderr);
}
