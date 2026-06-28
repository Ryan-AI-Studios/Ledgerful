use crate::common::setup_git_repo;
use camino::Utf8Path;
use ledgerful::state::storage::StorageManager;
use std::fs;
use std::process::Command;
use tempfile::tempdir;

#[test]
fn test_config_diff_identifies_references() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    let root_utf8 = Utf8Path::from_path(root).unwrap();

    setup_git_repo(root);
    fs::write(root.join(".env.example"), "MY_VAR=default").unwrap();
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("src/main.rs"),
        r#"fn main() { let _ = std::env::var("MY_VAR"); }"#,
    )
    .unwrap();

    let ledgerful_bin = env!("CARGO_BIN_EXE_ledgerful");

    // Init and Index
    Command::new(ledgerful_bin)
        .arg("init")
        .current_dir(root)
        .output()
        .unwrap();
    Command::new(ledgerful_bin)
        .arg("index")
        .current_dir(root)
        .output()
        .unwrap();

    // Check config diff
    let output = Command::new(ledgerful_bin)
        .args(["config", "diff"])
        .current_dir(root)
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!stdout.contains("- MY_VAR"), "Output was: {}", stdout);

    // Check database directly
    let storage = StorageManager::open_read_only(root_utf8).unwrap();
    let conn = storage.get_connection();
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM env_references WHERE var_name = 'MY_VAR'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(count > 0, "env_references table should contain MY_VAR");
}

#[test]
fn test_option_env_detected() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    let root_utf8 = Utf8Path::from_path(root).unwrap();

    setup_git_repo(root);
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("src/main.rs"),
        r#"fn main() { let _ = option_env!("OPT_VAR"); }"#,
    )
    .unwrap();

    let ledgerful_bin = env!("CARGO_BIN_EXE_ledgerful");
    Command::new(ledgerful_bin)
        .arg("init")
        .current_dir(root)
        .output()
        .unwrap();
    Command::new(ledgerful_bin)
        .arg("index")
        .current_dir(root)
        .output()
        .unwrap();

    let storage = StorageManager::open_read_only(root_utf8).unwrap();
    let conn = storage.get_connection();
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM env_references WHERE var_name = 'OPT_VAR'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(count > 0, "env_references table should contain OPT_VAR");
}

#[test]
fn test_import_meta_env_detected() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    let root_utf8 = Utf8Path::from_path(root).unwrap();

    setup_git_repo(root);
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("src/app.ts"),
        r#"const api = import.meta.env.VITE_API_URL;"#,
    )
    .unwrap();

    let ledgerful_bin = env!("CARGO_BIN_EXE_ledgerful");
    Command::new(ledgerful_bin)
        .arg("init")
        .current_dir(root)
        .output()
        .unwrap();
    Command::new(ledgerful_bin)
        .arg("index")
        .current_dir(root)
        .output()
        .unwrap();

    let storage = StorageManager::open_read_only(root_utf8).unwrap();
    let conn = storage.get_connection();
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM env_references WHERE var_name = 'VITE_API_URL'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(
        count > 0,
        "env_references table should contain VITE_API_URL"
    );
}

#[test]
fn test_orphan_cleanup_on_file_deletion() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    let root_utf8 = Utf8Path::from_path(root).unwrap();

    setup_git_repo(root);
    fs::create_dir_all(root.join("src")).unwrap();
    let file_to_delete = root.join("src/extra.rs");
    fs::write(
        &file_to_delete,
        r#"fn main() { let _ = std::env::var("DELETE_ME"); }"#,
    )
    .unwrap();

    let ledgerful_bin = env!("CARGO_BIN_EXE_ledgerful");
    Command::new(ledgerful_bin)
        .arg("init")
        .current_dir(root)
        .output()
        .unwrap();
    Command::new(ledgerful_bin)
        .arg("index")
        .current_dir(root)
        .output()
        .unwrap();

    {
        let storage = StorageManager::open_read_only(root_utf8).unwrap();
        let conn = storage.get_connection();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM env_references WHERE var_name = 'DELETE_ME'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(
            count > 0,
            "env_references should contain DELETE_ME initially"
        );
    }

    // Delete file and re-index
    fs::remove_file(file_to_delete).unwrap();
    // In YOLO mode, we simulate the git state change by NOT adding the deletion to git,
    // but Ledgerful index should still handle it if it scans the FS or if we use --force.
    // Actually, Ledgerful indexer uses project_files table.
    // We need to make sure project_files is updated or the orphan cleanup handles it.

    Command::new(ledgerful_bin)
        .arg("index")
        .current_dir(root)
        .output()
        .unwrap();

    let storage = StorageManager::open_read_only(root_utf8).unwrap();
    let conn = storage.get_connection();
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM env_references WHERE var_name = 'DELETE_ME'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        count, 0,
        "env_references should be empty after file deletion and re-indexing"
    );
}
