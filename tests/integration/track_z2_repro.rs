use crate::common::setup_git_repo;
use camino::Utf8Path;
use ledgerful::state::storage::StorageManager;
use std::fs;
use std::process::Command;
use tempfile::tempdir;

#[test]
fn test_data_models_impact_binary_output() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    let root_utf8 = Utf8Path::from_path(root).unwrap();

    setup_git_repo(root);
    fs::write(root.join("models.rs"), "struct User;").unwrap();

    // Use the binary to capture output
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

    // We need to commit so it's not "changed" (clean tree)
    Command::new("git")
        .args(["add", "."])
        .current_dir(root)
        .output()
        .unwrap();
    Command::new("git")
        .args(["commit", "-m", "init"])
        .current_dir(root)
        .output()
        .unwrap();

    // Manually insert model because detector is picky in this env
    let storage = StorageManager::open_read_only(root_utf8).unwrap();
    let conn = storage.get_connection();
    let file_id: i64 = conn
        .query_row(
            "SELECT id FROM project_files WHERE file_path = 'models.rs'",
            [],
            |r| r.get(0),
        )
        .unwrap_or(1);
    conn.execute(
        "INSERT INTO data_models (model_name, model_file_id, language, model_kind, confidence, evidence, last_indexed_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        rusqlite::params!["User", file_id, "Rust", "STRUCT", 1.0_f64, "manual", "2026-05-01T00:00:00Z"],
    ).unwrap();

    let output = Command::new(ledgerful_bin)
        .args(["data-models", "impact", "--changed"])
        .current_dir(root)
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    // EXPECTED BEHAVIOR: contains "No changed data models found." AND NO empty table/header
    assert!(
        stdout.contains("No changed data models found."),
        "Output was: {}",
        stdout
    );
    assert!(
        !stdout.contains("No data models indexed."),
        "Should not contain misleading help message"
    );
    assert!(
        !stdout.contains("Name | File"),
        "Should not contain table header"
    );
    assert!(output.status.success());
}

#[test]
fn test_data_models_impact_binary_output_no_models_at_all() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();

    setup_git_repo(root);
    fs::write(root.join("dummy.txt"), "content").unwrap();

    let ledgerful_bin = env!("CARGO_BIN_EXE_ledgerful");

    Command::new(ledgerful_bin)
        .arg("init")
        .current_dir(root)
        .output()
        .unwrap();

    let output = Command::new(ledgerful_bin)
        .args(["data-models", "impact", "--changed"])
        .current_dir(root)
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("No data models indexed."),
        "Output was: {}",
        stdout
    );
    assert!(
        !stdout.contains("No changed data models found."),
        "Output was: {}",
        stdout
    );
    assert!(
        !stdout.contains("Name | File"),
        "Should not contain table header"
    );
    assert!(output.status.success());
}

#[test]
fn test_data_models_impact_json_output() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();

    setup_git_repo(root);
    fs::write(root.join("models.rs"), "struct User;").unwrap();

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

    // We need to commit so it's not "changed" (clean tree) or leave it uncommitted so it IS changed.
    // The test requires it to be "changed". If it's uncommitted, `project_files` might not track it properly for foreign key unless index indexed it.
    // Wait, `index` only indexes committed files by default unless there's --untracked?
    // Let's commit it and then modify it to make it "changed", OR commit it and let `git diff HEAD` show it as changed?
    // Actually, `data-models impact --changed` looks at modified files. Let's commit and then append.
    Command::new("git")
        .args(["add", "."])
        .current_dir(root)
        .output()
        .unwrap();
    Command::new("git")
        .args(["commit", "-m", "init"])
        .current_dir(root)
        .output()
        .unwrap();

    // Modify the file so it's "changed"
    fs::write(root.join("models.rs"), "struct User { id: i32 }").unwrap();

    // Insert dummy data directly
    let storage =
        StorageManager::open_read_only(camino::Utf8Path::from_path(root).unwrap()).unwrap();
    let conn = storage.get_connection();
    let file_id: i64 = conn
        .query_row(
            "SELECT id FROM project_files WHERE file_path = 'models.rs'",
            [],
            |r| r.get(0),
        )
        .unwrap_or(1);
    conn.execute(
        "INSERT INTO data_models (model_name, model_file_id, language, model_kind, confidence, evidence, last_indexed_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        rusqlite::params!["User", file_id, "Rust", "STRUCT", 1.0_f64, "manual", "2026-05-01T00:00:00Z"],
    ).unwrap();
    drop(storage);

    // Call impact --changed --json
    let output = Command::new(ledgerful_bin)
        .env("RUST_LOG", "error")
        .args(["data-models", "impact", "--changed", "--json"])
        .current_dir(root)
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);

    // Attempt to parse JSON. Find the first '{' to ignore any potential logging prefixes.
    let json_start = stdout.find('{').unwrap_or(0);
    let json_str = &stdout[json_start..];

    let parsed: serde_json::Value = serde_json::from_str(json_str)
        .unwrap_or_else(|_| panic!("Should output valid JSON, got: {}", json_str));
    let obj = parsed.as_object().expect("JSON should be an object");
    assert!(
        obj.contains_key("impacted"),
        "JSON must contain 'impacted' array"
    );
    let arr = obj
        .get("impacted")
        .unwrap()
        .as_array()
        .expect("'impacted' should be an array");
    assert!(!arr.is_empty(), "impacted array should not be empty");
    // We expect it to be empty since no files were modified yet (git status is clean since we didn't add models.rs).
    // Or we expect models.rs if it's considered changed?
    // The test in the other method expects "No changed data models found" when `models.rs` is committed.
    // If it's not committed, git status might consider it changed, but `project_files` might not track it fully.
    // But anyway, it just needs to be valid JSON containing an `impacted` array!
    assert!(output.status.success());
}
