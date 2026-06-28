use camino::Utf8Path;
use ledgerful::commands::index::{IndexArgs, execute_index};
use std::fs;
use tempfile::tempdir;

use crate::common::{DirGuard, setup_git_repo};

#[test]
fn test_ta34_repair_metadata() {
    let tmp = tempdir().unwrap();
    let root = Utf8Path::from_path(tmp.path()).unwrap();
    setup_git_repo(tmp.path());

    let _guard = DirGuard::from_utf8(root);
    let state_dir = root.join(".ledgerful").join("state");
    fs::create_dir_all(&state_dir).unwrap();

    // Initial index
    let res = execute_index(IndexArgs {
        incremental: true,
        ..Default::default()
    });
    res.unwrap();

    // Delete last_indexed_at
    {
        let state_dir = root.join(".ledgerful").join("state");
        let conn = rusqlite::Connection::open(state_dir.join("ledger.db")).unwrap();
        conn.execute(
            "DELETE FROM index_metadata WHERE key = 'last_indexed_at'",
            [],
        )
        .unwrap();
    }

    // Repair metadata
    let repair_res = execute_index(IndexArgs {
        repair_metadata: true,
        yes: true,
        ..Default::default()
    });
    assert!(repair_res.is_ok(), "Repair failed: {:?}", repair_res.err());
}

#[test]
fn test_ta34_empty_reason_discovery() {
    let tmp = tempdir().unwrap();
    let root = Utf8Path::from_path(tmp.path()).unwrap();
    setup_git_repo(tmp.path());

    let _guard = DirGuard::from_utf8(root);
    let state_dir = root.join(".ledgerful").join("state");
    fs::create_dir_all(&state_dir).unwrap();

    // Initial index
    let res = execute_index(IndexArgs {
        incremental: true,
        ..Default::default()
    });
    res.unwrap();

    let state_dir = root.join(".ledgerful").join("state");
    let conn = rusqlite::Connection::open(state_dir.join("ledger.db")).unwrap();
    let empty_reason: Option<String> = conn
        .query_row(
            "SELECT value FROM index_metadata WHERE key = 'empty_reason'",
            [],
            |row| row.get(0),
        )
        .ok();

    assert!(empty_reason.is_some(), "empty_reason must be populated");
    let reason = empty_reason.unwrap();
    assert!(reason.contains("RepositoryEmpty") || reason.contains("NoSupportedFiles"));
}
