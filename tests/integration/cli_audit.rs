use crate::common::{DirGuard, git_add_and_commit, setup_git_repo};
use ledgerful::commands::init::execute_init;
use ledgerful::commands::ledger_audit::execute_ledger_audit;
use std::fs;
use std::path::Path;
use std::process::Command;
use tempfile::tempdir;

#[test]
fn audit_returns_entity_list() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();

    setup_git_repo(root);
    fs::write(root.join("dummy.txt"), "content").unwrap();
    git_add_and_commit(root, "initial");

    let _guard = DirGuard::new(root);
    execute_init(false, false).unwrap();

    // Audit with limit 5, no entity filter, not json, no unaudited
    let result = execute_ledger_audit(None, false, 5, 0, false, None);
    assert!(result.is_ok());
}

#[test]
fn audit_entity_related_returns_related_entities() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();

    setup_git_repo(root);

    // Create related files
    fs::create_dir_all(root.join("src/cli")).unwrap();
    fs::write(root.join("src/cli/dispatch.rs"), "fn dispatch() {}").unwrap();
    fs::write(root.join("src/cli/helpers.rs"), "fn helper() {}").unwrap();
    git_add_and_commit(root, "initial");

    let _guard = DirGuard::new(root);
    execute_init(false, false).unwrap();

    let db_path = root.join(".ledgerful/state/ledger.db");
    let mut storage = ledgerful::state::storage::StorageManager::init(&db_path).unwrap();
    let mut manager = ledgerful::ledger::TransactionManager::new(
        &mut storage,
        root.to_path_buf(),
        ledgerful::config::model::Config::default(),
    );

    // Transaction for dispatch.rs
    let tx1 = manager
        .start_change(ledgerful::ledger::TransactionRequest {
            category: ledgerful::ledger::Category::Feature,
            entity: "src/cli/dispatch.rs".to_string(),
            ..Default::default()
        })
        .unwrap();
    manager
        .commit_change(
            tx1,
            ledgerful::ledger::CommitRequest {
                summary: "Dispatch change".to_string(),
                reason: "reason".to_string(),
                ..Default::default()
            },
            false,
        )
        .unwrap();

    // Transaction for helpers.rs
    let tx2 = manager
        .start_change(ledgerful::ledger::TransactionRequest {
            category: ledgerful::ledger::Category::Bugfix,
            entity: "src/cli/helpers.rs".to_string(),
            ..Default::default()
        })
        .unwrap();
    manager
        .commit_change(
            tx2,
            ledgerful::ledger::CommitRequest {
                summary: "Helpers change".to_string(),
                reason: "reason".to_string(),
                ..Default::default()
            },
            false,
        )
        .unwrap();

    drop(manager);
    drop(storage);

    // Audit dispatch.rs
    let result = execute_ledger_audit(
        Some("src/cli/dispatch.rs".to_string()),
        false,
        5,
        0,
        false,
        None,
    );
    assert!(result.is_ok());
}

fn run_bin(root: &Path, args: &[&str]) -> (String, String, i32) {
    let output = Command::new(env!("CARGO_BIN_EXE_ledgerful"))
        .args(args)
        .current_dir(root)
        .env("LEDGERFUL_NON_INTERACTIVE", "1")
        .output()
        .expect("failed to spawn ledgerful");
    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
        output.status.code().unwrap_or(-1),
    )
}

fn seed_changed_files_audit_repo() -> tempfile::TempDir {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    fs::create_dir_all(root.join("src/commands")).unwrap();
    fs::write(root.join("src/commands/configure.rs"), "fn configure() {}").unwrap();
    fs::write(root.join("src/commands/other.rs"), "fn other() {}").unwrap();
    git_add_and_commit(root, "initial");

    let (init_out, init_err, init_code) = run_bin(root, &["init"]);
    assert_eq!(
        init_code, 0,
        "ledgerful init failed: stderr={init_err} stdout={init_out}"
    );

    let db_path = root.join(".ledgerful").join("state").join("ledger.db");
    let mut storage = ledgerful::state::storage::StorageManager::init(&db_path).unwrap();
    let mut manager = ledgerful::ledger::TransactionManager::new(
        &mut storage,
        root.to_path_buf(),
        ledgerful::config::model::Config::default(),
    );
    let file_path = "src/commands/configure.rs";
    let snapshot_id = {
        let conn = manager.get_connection();
        conn.execute(
            "INSERT INTO snapshots (timestamp, head_hash, branch_name, is_clean, packet_json)
             VALUES ('2026-09-12T00:00:00Z', 'deadbeef', 'main', 1, '{}')",
            [],
        )
        .unwrap();
        conn.last_insert_rowid()
    };
    manager
        .get_connection()
        .execute(
            "INSERT INTO changed_files (snapshot_id, path, status, is_staged) VALUES (?1, ?2, 'MODIFIED', 1)",
            (snapshot_id, file_path),
        )
        .unwrap();

    let track_tx = manager
        .start_change(ledgerful::ledger::TransactionRequest {
            category: ledgerful::ledger::Category::Bugfix,
            entity: "0319-fixture-track".to_string(),
            ..Default::default()
        })
        .unwrap();
    manager
        .commit_change(
            track_tx,
            ledgerful::ledger::CommitRequest {
                summary: "fixture track commit".to_string(),
                reason: "Co-authored-by: Cursor <cursoragent@cursor.com>".to_string(),
                risk: Some("HIGH".to_string()),
                snapshot_id: Some(snapshot_id),
                ..Default::default()
            },
            false,
        )
        .unwrap();

    let neighbor_tx = manager
        .start_change(ledgerful::ledger::TransactionRequest {
            category: ledgerful::ledger::Category::Docs,
            entity: "src/commands/other.rs".to_string(),
            ..Default::default()
        })
        .unwrap();
    manager
        .commit_change(
            neighbor_tx,
            ledgerful::ledger::CommitRequest {
                summary: "neighbor".to_string(),
                reason: "Store a substantive why.".to_string(),
                risk: Some("TRIVIAL".to_string()),
                ..Default::default()
            },
            false,
        )
        .unwrap();

    drop(manager);
    storage.shutdown().unwrap();
    tmp
}

#[test]
fn audit_human_trailer_reason_prefixed() {
    let tmp = seed_changed_files_audit_repo();
    let args = ["ledger", "audit", "src/commands/configure.rs"];
    let (stdout, stderr, code) = run_bin(tmp.path(), &args);
    assert_eq!(
        code, 0,
        "human audit failed (code {code}): stderr={stderr} stdout={stdout}"
    );
    assert!(
        stdout.contains("Reason:") && stdout.contains("[trailer]"),
        "human stdout must print Reason: with [trailer]: {stdout}"
    );
    assert!(
        stdout.contains("Risk:") && stdout.contains("(from category BUGFIX)"),
        "human stdout must print Risk: with category source: {stdout}"
    );
}

#[test]
fn audit_exact_includes_changed_files_track_entity() {
    let tmp = seed_changed_files_audit_repo();
    let args = [
        "ledger",
        "audit",
        "src/commands/configure.rs",
        "--json",
        "--limit",
        "20",
    ];
    let (stdout, stderr, code) = run_bin(tmp.path(), &args);
    assert_eq!(
        code, 0,
        "json audit failed (code {code}): stderr={stderr} stdout={stdout}"
    );
    let value: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("audit JSON must parse");
    let exact = value["exact"]
        .as_array()
        .unwrap_or_else(|| panic!("expected exact array: {stdout}"));
    let track = exact
        .iter()
        .find(|e| e["entity"] == "0319-fixture-track")
        .unwrap_or_else(|| panic!("track-slug TX missing from exact: {stdout}"));
    assert_eq!(track["matchBasis"], "changed_files");
    assert_eq!(track["reasonKind"], "trailer");
    assert_eq!(track["riskSource"], "category");
    assert!(
        track.get("match_basis").is_none(),
        "audit JSON must be camelCase: {track}"
    );

    let related = value["related"]
        .as_array()
        .unwrap_or_else(|| panic!("expected related array: {stdout}"));
    let neighbor = related
        .iter()
        .find(|e| e["entity"] == "src/commands/other.rs")
        .unwrap_or_else(|| panic!("directory neighbor missing from related: {stdout}"));
    assert_eq!(neighbor["matchBasis"], "directory");
    assert!(
        exact.iter().all(|e| e["entity"] != "src/commands/other.rs"),
        "directory neighbor must not be exact: {stdout}"
    );
}
