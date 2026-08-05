use crate::common::{git_add_and_commit, setup_git_repo};
use camino::Utf8Path;
use ledgerful::state::layout::Layout;
use ledgerful::state::storage::StorageManager;
use std::fs;
use std::process::Command;
use tempfile::tempdir;

#[test]
fn test_f8_index_exclusions_ignored_by_scan() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();

    setup_git_repo(root);
    fs::create_dir_all(root.join(".agents").join("worktrees").join("ignored")).unwrap();
    fs::write(
        root.join(".agents")
            .join("worktrees")
            .join("ignored")
            .join("ignore_me.rs"),
        "fn ignored() {}",
    )
    .unwrap();
    fs::write(root.join("keep_me.rs"), "fn keep() {}").unwrap();

    git_add_and_commit(root, "init");

    let bin = env!("CARGO_BIN_EXE_ledgerful");
    Command::new(bin)
        .arg("init")
        .current_dir(root)
        .output()
        .unwrap();
    Command::new(bin)
        .arg("index")
        .arg("--incremental")
        .current_dir(root)
        .output()
        .unwrap();

    let storage =
        StorageManager::open_read_only(&Layout::new(Utf8Path::from_path(root).unwrap())).unwrap();
    let conn = storage.get_connection();

    // ignore_me.rs should not be in project_files
    // .agents/worktrees/ignored/ignore_me.rs should not be in project_files
    let count: i64 = conn
        .query_row(
            "SELECT count(*) FROM project_files WHERE file_path LIKE '%ignore_me.rs'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 0, "ignore_me.rs was not ignored");

    let count: i64 = conn
        .query_row(
            "SELECT count(*) FROM project_files WHERE file_path = 'keep_me.rs'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 1, "keep_me.rs should be indexed");
}

#[test]
fn test_f10_endpoints_changed_with_unindexed_vs_indexed() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();

    setup_git_repo(root);
    fs::write(
        root.join("main.rs"),
        "
use axum::{routing::get, Router};
async fn handler() {}
fn main() { let app = Router::new().route(\"/\", get(handler)); }
",
    )
    .unwrap();
    git_add_and_commit(root, "init");

    let bin = env!("CARGO_BIN_EXE_ledgerful");
    Command::new(bin)
        .arg("init")
        .current_dir(root)
        .output()
        .unwrap();

    // Without index, never-indexed message:
    let output = Command::new(bin)
        .args(["endpoints", "--changed"])
        .current_dir(root)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("No endpoints indexed."),
        "Expected no endpoints indexed message: {}",
        stdout
    );

    // Index it
    Command::new(bin)
        .arg("index")
        .current_dir(root)
        .output()
        .unwrap();

    // With index but unchanged, should show different message
    let output = Command::new(bin)
        .args(["endpoints", "--changed"])
        .current_dir(root)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("No endpoints changed in the current diff."),
        "Expected no endpoints changed message: {}",
        stdout
    );
}

/// B2.1 / 0146 P2-2: dirty working-tree path for `endpoints --changed`.
///
/// Production wires non-empty git status → RepoSnapshot → map_snapshot_to_packet
/// (symbol extract only) → match_affected_route_keys(..., blast None). Existing
/// tests only covered the clean CleanDiff short-circuit and hand-built HashSet
/// filtering. This hermetic e2e dirties a real indexed Axum registration file
/// and asserts the route still appears via the production CLI.
#[test]
fn test_endpoints_changed_dirty_handler_appears() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();

    setup_git_repo(root);
    let route_src = r#"
use axum::{routing::get, Router};
async fn health() {}
fn main() { let app = Router::new().route("/health", get(health)); }
"#;
    let main_path = root.join("main.rs");
    fs::write(&main_path, route_src).unwrap();
    git_add_and_commit(root, "init axum route");

    let bin = env!("CARGO_BIN_EXE_ledgerful");
    let init = Command::new(bin)
        .arg("init")
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        init.status.success(),
        "init failed: {}",
        String::from_utf8_lossy(&init.stderr)
    );

    let index = Command::new(bin)
        .arg("index")
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        index.status.success(),
        "index failed: stdout={} stderr={}",
        String::from_utf8_lossy(&index.stdout),
        String::from_utf8_lossy(&index.stderr)
    );

    // Prerequisite: routes were extracted (same clean-diff signal as f10).
    let clean = Command::new(bin)
        .args(["endpoints", "--changed"])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        clean.status.success(),
        "clean endpoints --changed failed: {}",
        String::from_utf8_lossy(&clean.stderr)
    );
    let clean_stdout = String::from_utf8_lossy(&clean.stdout);
    assert!(
        clean_stdout.contains("No endpoints changed in the current diff."),
        "expected indexed routes + clean tree before dirty step; got: {clean_stdout}"
    );

    // Snapshot latest-impact (0146: --changed must not rewrite impact cache).
    let impact_path = root
        .join(".ledgerful")
        .join("reports")
        .join("latest-impact.json");
    let impact_before = impact_path
        .exists()
        .then(|| fs::read_to_string(&impact_path).ok());

    // Dirty the registration/handler file without committing (B2.1 path).
    fs::write(
        &main_path,
        format!("{route_src}\n// dirty working tree for endpoints --changed e2e\n"),
    )
    .unwrap();

    let dirty = Command::new(bin)
        .args(["endpoints", "--changed", "--json"])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        dirty.status.success(),
        "dirty endpoints --changed failed: stdout={} stderr={}",
        String::from_utf8_lossy(&dirty.stdout),
        String::from_utf8_lossy(&dirty.stderr)
    );
    let dirty_stdout = String::from_utf8_lossy(&dirty.stdout);
    assert!(
        !dirty_stdout.contains("No endpoints changed in the current diff."),
        "dirty registration file must not yield CleanDiff empty: {dirty_stdout}"
    );
    assert!(
        !dirty_stdout.contains("noIndexedData") && !dirty_stdout.contains("No endpoints indexed"),
        "routes must stay indexed after dirty; got: {dirty_stdout}"
    );

    let v: serde_json::Value = serde_json::from_str(&dirty_stdout).unwrap_or_else(|e| {
        panic!("expected JSON from endpoints --changed --json: {e}; body={dirty_stdout}");
    });
    // Non-empty results: no emptyReason envelope (or results array with route).
    let results = v
        .get("results")
        .and_then(|r| r.as_array())
        .or_else(|| v.as_array())
        .unwrap_or_else(|| {
            panic!("expected results array in dirty --changed JSON: {dirty_stdout}");
        });
    assert!(
        !results.is_empty(),
        "dirty handler/registration file must surface matched routes; got: {dirty_stdout}"
    );

    let joined = dirty_stdout.to_lowercase();
    assert!(
        joined.contains("/health") || joined.contains("health"),
        "expected /health route or health handler in --changed output: {dirty_stdout}"
    );
    // Method may be uppercased in JSON.
    assert!(
        results.iter().any(|r| {
            let method = r.get("method").and_then(|m| m.as_str()).unwrap_or("");
            let path = r.get("path").and_then(|p| p.as_str()).unwrap_or("");
            let handler = r.get("handler").and_then(|h| h.as_str()).unwrap_or("");
            method.eq_ignore_ascii_case("GET")
                && (path.contains("/health") || handler.contains("health"))
        }),
        "expected GET /health (or health handler) among results: {dirty_stdout}"
    );

    let impact_after = impact_path
        .exists()
        .then(|| fs::read_to_string(&impact_path).ok());
    assert_eq!(
        impact_before, impact_after,
        "endpoints --changed must not create/rewrite latest-impact.json; \
         before={impact_before:?} after={impact_after:?}"
    );
}

#[test]
fn test_f10_doctor_graph_state_qualifier() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();

    setup_git_repo(root);
    fs::write(root.join("main.rs"), "fn main() {}").unwrap();
    git_add_and_commit(root, "init");

    let bin = env!("CARGO_BIN_EXE_ledgerful");
    Command::new(bin)
        .arg("init")
        .current_dir(root)
        .output()
        .unwrap();

    // Incrementally index so project_files is populated but not graph analyzed
    Command::new(bin)
        .arg("index")
        .arg("--incremental")
        .current_dir(root)
        .output()
        .unwrap();

    let output = Command::new(bin)
        .args(["doctor"])
        .current_dir(root)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Graph state: Current"),
        "Doctor output missing graph state: {}",
        stdout
    );
}

#[test]
fn test_f12_ledger_gc_dry_run() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();

    setup_git_repo(root);
    let bin = env!("CARGO_BIN_EXE_ledgerful");
    Command::new(bin)
        .arg("init")
        .current_dir(root)
        .output()
        .unwrap();

    let out = Command::new(bin)
        .args([
            "ledger",
            "start",
            "test",
            "--category",
            "REFACTOR",
            "--message",
            "test",
        ])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "ledger start failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Find the tx id from output, e.g. "Started transaction: tx-123"
    // Just hack it by backdating ALL pending transactions since there's only one
    let storage = StorageManager::init(
        root.join(".ledgerful")
            .join("state")
            .join("ledger.db")
            .as_path(),
    )
    .unwrap();
    storage
        .get_connection()
        .execute(
            "UPDATE transactions SET started_at = datetime('now', '-4 days')",
            [],
        )
        .unwrap();

    let output = Command::new(bin)
        .args(["ledger", "gc", "--dry-run", "--stale"])
        .current_dir(root)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Dry-run completed. No transactions were modified."),
        "Expected Dry-run message: {}",
        stdout
    );
    assert!(
        !stdout.contains("Successfully cleaned up"),
        "Should not contain Successfully cleaned up in dry run"
    );
}

#[test]
fn test_f13_data_models_pruning() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();

    setup_git_repo(root);
    // Use a valid ORM marker so it actually gets indexed as a data model
    fs::write(
        root.join("models.rs"),
        "#[derive(sqlx::FromRow)]\nstruct MyModel;",
    )
    .unwrap();
    git_add_and_commit(root, "init");

    let bin = env!("CARGO_BIN_EXE_ledgerful");
    Command::new(bin)
        .arg("init")
        .current_dir(root)
        .output()
        .unwrap();
    Command::new(bin)
        .arg("index")
        .arg("--incremental")
        .current_dir(root)
        .output()
        .unwrap();

    let storage =
        StorageManager::open_read_only(&Layout::new(Utf8Path::from_path(root).unwrap())).unwrap();
    let count: i64 = storage
        .get_connection()
        .query_row(
            "SELECT count(*) FROM data_models WHERE model_name = 'MyModel'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 1, "MyModel should be indexed");

    // Remove struct and re-index
    fs::write(root.join("models.rs"), "// removed").unwrap();
    git_add_and_commit(root, "remove");
    Command::new(bin)
        .arg("index")
        .arg("--incremental")
        .current_dir(root)
        .output()
        .unwrap();

    let storage =
        StorageManager::open_read_only(&Layout::new(Utf8Path::from_path(root).unwrap())).unwrap();
    let count: i64 = storage
        .get_connection()
        .query_row(
            "SELECT count(*) FROM data_models WHERE model_name = 'MyModel'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 0, "MyModel should be pruned");
}

#[test]
fn test_f13_serialize_only_struct_excluded() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();

    setup_git_repo(root);
    fs::write(root.join("models.rs"), "#[derive(Serialize)]\nstruct JustSerialize;\n#[derive(sqlx::FromRow)]\nstruct ValidDbModel;").unwrap();
    git_add_and_commit(root, "init");

    let bin = env!("CARGO_BIN_EXE_ledgerful");
    Command::new(bin)
        .arg("init")
        .current_dir(root)
        .output()
        .unwrap();
    Command::new(bin)
        .arg("index")
        .arg("--incremental")
        .current_dir(root)
        .output()
        .unwrap();

    let storage =
        StorageManager::open_read_only(&Layout::new(Utf8Path::from_path(root).unwrap())).unwrap();
    let conn = storage.get_connection();

    let valid_count: i64 = conn
        .query_row(
            "SELECT count(*) FROM data_models WHERE model_name = 'ValidDbModel'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(valid_count, 1, "ValidDbModel should be indexed");

    let invalid_count: i64 = conn
        .query_row(
            "SELECT count(*) FROM data_models WHERE model_name = 'JustSerialize'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        invalid_count, 0,
        "JustSerialize should NOT be indexed because it is only Serialize"
    );
}

#[test]
fn test_f14_config_diff_env() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();

    setup_git_repo(root);
    let bin = env!("CARGO_BIN_EXE_ledgerful");
    Command::new(bin)
        .arg("init")
        .current_dir(root)
        .output()
        .unwrap();

    // Create a fixture file that references an excluded variable and a non-excluded variable
    fs::write(
        root.join("env_usage.rs"),
        r#"
        fn get_env() {
            let _ = std::env::var("LOCALAPPDATA");
            let _ = std::env::var("MY_CUSTOM_VAR");
        }
    "#,
    )
    .unwrap();
    git_add_and_commit(root, "fixture");
    Command::new(bin)
        .arg("index")
        .arg("--incremental")
        .current_dir(root)
        .output()
        .unwrap();

    // Set some env vars that should be ignored
    let mut cmd = Command::new(bin);
    cmd.args(["config", "diff"])
        .current_dir(root)
        .env("LOCALAPPDATA", "C:\\Users\\test\\AppData\\Local")
        .env("USERPROFILE", "C:\\Users\\test");

    let output = cmd.output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        !stdout.contains("LOCALAPPDATA"),
        "LOCALAPPDATA should be ignored, but stdout was: {}",
        stdout
    );
    assert!(
        !stdout.contains("USERPROFILE"),
        "USERPROFILE should be ignored, but stdout was: {}",
        stdout
    );
    assert!(
        stdout.contains("MY_CUSTOM_VAR"),
        "MY_CUSTOM_VAR should NOT be ignored, but stdout was: {}",
        stdout
    );
}
