use crate::common::setup_git_repo;
use camino::Utf8Path;
use ledgerful::state::layout::Layout;
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

    // Manually insert model because detector is picky in this env.
    // Must use write open — true SQLITE_OPEN_READ_ONLY rejects INSERT.
    let layout = Layout::new(root_utf8);
    let storage = StorageManager::init_with_layout(&layout).unwrap();
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
    let _ = storage.shutdown();

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

    // Insert dummy data directly (write open — SQLITE_OPEN_READ_ONLY rejects INSERT).
    let layout = Layout::new(camino::Utf8Path::from_path(root).unwrap());
    let storage = StorageManager::init_with_layout(&layout).unwrap();
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
    let _ = storage.shutdown();

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

/// Seed a clean git-indexed fixture with stacked identical `data_models` rows
/// (legacy multi-pass residue) for DoD-3 e2e uniqueness coverage (0155).
///
/// Returns `(tmp, root_path, ledgerful_bin)`. Raw table holds 3 rows for
/// identities that must emit as 2 unique models (User×2 + Account×1).
fn setup_stacked_data_models_fixture() -> (tempfile::TempDir, std::path::PathBuf, &'static str) {
    let tmp = tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    let root_utf8 = Utf8Path::from_path(&root).unwrap();

    setup_git_repo(&root);
    fs::write(root.join("models.rs"), "struct User;\nstruct Account;").unwrap();

    let ledgerful_bin = env!("CARGO_BIN_EXE_ledgerful");

    let init = Command::new(ledgerful_bin)
        .arg("init")
        .current_dir(&root)
        .output()
        .unwrap();
    assert!(
        init.status.success(),
        "init failed: {}",
        String::from_utf8_lossy(&init.stderr)
    );

    let index = Command::new(ledgerful_bin)
        .arg("index")
        .current_dir(&root)
        .output()
        .unwrap();
    assert!(
        index.status.success(),
        "index failed: {}",
        String::from_utf8_lossy(&index.stderr)
    );

    // Clean tree so impact without --changed is not dominated by git noise.
    Command::new("git")
        .args(["add", "."])
        .current_dir(&root)
        .output()
        .unwrap();
    Command::new("git")
        .args(["commit", "-m", "init"])
        .current_dir(&root)
        .output()
        .unwrap();

    let layout = Layout::new(root_utf8);
    let storage = StorageManager::init_with_layout(&layout).unwrap();
    let conn = storage.get_connection();
    let file_id: i64 = conn
        .query_row(
            "SELECT id FROM project_files WHERE file_path = 'models.rs'",
            [],
            |r| r.get(0),
        )
        .unwrap_or(1);

    // Controlled stacked fixture: wipe detector residue, then insert known rows.
    conn.execute("DELETE FROM data_models", []).unwrap();
    // Two identical User identities (stacked residue), different last_indexed_at.
    conn.execute(
        "INSERT INTO data_models (model_name, model_file_id, language, model_kind, confidence, evidence, last_indexed_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        rusqlite::params![
            "User",
            file_id,
            "Rust",
            "STRUCT",
            0.9_f64,
            "manual-stack-1",
            "2026-05-01T00:00:00Z"
        ],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO data_models (model_name, model_file_id, language, model_kind, confidence, evidence, last_indexed_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        rusqlite::params![
            "User",
            file_id,
            "Rust",
            "STRUCT",
            0.95_f64,
            "manual-stack-2",
            "2026-05-03T00:00:00Z"
        ],
    )
    .unwrap();
    // Second distinct model once.
    conn.execute(
        "INSERT INTO data_models (model_name, model_file_id, language, model_kind, confidence, evidence, last_indexed_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        rusqlite::params![
            "Account",
            file_id,
            "Rust",
            "STRUCT",
            0.9_f64,
            "manual-account",
            "2026-05-01T00:00:00Z"
        ],
    )
    .unwrap();

    let raw_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM data_models", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        raw_count, 3,
        "fixture must leave stacked rows in the table (User×2 + Account×1)"
    );
    let _ = storage.shutdown();

    (tmp, root, ledgerful_bin)
}

/// 0155 DoD-3: `data-models list --json` must emit unique rows through the real
/// CLI binary when the DB holds stacked identical model identities.
#[test]
fn data_models_list_json_dedupes_stacked_rows() {
    let (_tmp, root, ledgerful_bin) = setup_stacked_data_models_fixture();

    let output = Command::new(ledgerful_bin)
        .env("RUST_LOG", "error")
        .args(["data-models", "list", "--json"])
        .current_dir(&root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "list --json failed: status={:?} stderr={}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("list --json must be valid JSON ({e}): {stdout}"));
    assert!(
        parsed.is_object(),
        "list --json must be an object envelope, got: {stdout}"
    );
    let arr = parsed["models"]
        .as_array()
        .unwrap_or_else(|| panic!("list --json must expose models[], got: {stdout}"));

    assert_eq!(
        arr.len(),
        2,
        "stacked User rows must collapse; expected unique count 2 (User, Account), got: {arr:?}"
    );

    let mut names: Vec<&str> = arr
        .iter()
        .map(|v| v["name"].as_str().unwrap_or(""))
        .collect();
    names.sort();
    assert_eq!(names, vec!["Account", "User"]);

    // Every name appears once.
    let mut seen = std::collections::HashSet::new();
    for item in arr {
        let name = item["name"].as_str().expect("name present");
        assert!(
            seen.insert(name.to_string()),
            "duplicate name in list --json: {name}"
        );
        let file_path = item["file_path"]
            .as_str()
            .unwrap_or_else(|| panic!("file_path required on list row: {item}"));
        assert!(!file_path.is_empty(), "file_path must be non-empty: {item}");
        assert!(
            !file_path.contains('\\'),
            "file_path must use / separators, got: {file_path}"
        );
    }

    // Optional human path: same Name must not appear twice for same File.
    let human = Command::new(ledgerful_bin)
        .env("RUST_LOG", "error")
        .args(["data-models", "list"])
        .current_dir(&root)
        .output()
        .unwrap();
    assert!(human.status.success());
    let human_out = String::from_utf8_lossy(&human.stdout);
    let user_name_hits = human_out.matches("User").count();
    // Title/header may not include "User"; body should list User once.
    // Premium table has one Name cell per model; stacked residue must not double.
    assert!(
        user_name_hits <= 1,
        "human list must not list Name 'User' twice; stdout:\n{human_out}"
    );
}

/// 0155 DoD-3: `data-models impact --json` (all models, no --changed) must emit
/// unique (name, file_path) pairs when the DB holds stacked identical rows.
#[test]
fn data_models_impact_json_dedupes_stacked_rows() {
    let (_tmp, root, ledgerful_bin) = setup_stacked_data_models_fixture();

    let output = Command::new(ledgerful_bin)
        .env("RUST_LOG", "error")
        .args(["data-models", "impact", "--json"])
        .current_dir(&root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "impact --json failed: status={:?} stderr={}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let json_start = stdout.find('{').unwrap_or(0);
    let json_str = &stdout[json_start..];
    let parsed: serde_json::Value = serde_json::from_str(json_str)
        .unwrap_or_else(|e| panic!("impact --json must be valid JSON ({e}): {json_str}"));
    let obj = parsed
        .as_object()
        .unwrap_or_else(|| panic!("impact --json must be an object, got: {json_str}"));
    let arr = obj
        .get("impacted")
        .and_then(|v| v.as_array())
        .unwrap_or_else(|| panic!("impact --json must contain 'impacted' array: {json_str}"));

    assert_eq!(
        arr.len(),
        2,
        "stacked User rows must collapse; expected unique count 2, got: {arr:?}"
    );

    let mut pairs: Vec<(String, String)> = arr
        .iter()
        .map(|item| {
            let name = item["name"]
                .as_str()
                .unwrap_or_else(|| panic!("name required: {item}"))
                .to_string();
            let file_path = item["file_path"]
                .as_str()
                .unwrap_or_else(|| panic!("file_path required: {item}"))
                .to_string();
            assert!(
                !file_path.contains('\\'),
                "file_path must use / separators, got: {file_path}"
            );
            (name, file_path)
        })
        .collect();
    pairs.sort();

    let mut uniq = pairs.clone();
    uniq.dedup();
    assert_eq!(
        pairs, uniq,
        "impacted must have unique (name, file_path) pairs"
    );

    let names: Vec<&str> = pairs.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, vec!["Account", "User"]);
}
