use crate::common::{DirGuard, git_add_and_commit, setup_git_repo};
use camino::Utf8Path;
use ledgerful::commands::index::{IndexArgs, execute_index};
use ledgerful::commands::init::execute_init;
use ledgerful::state::storage::StorageManager;
use std::fs;
use std::process::Command;
use tempfile::tempdir;

/// Builds a minimal git repo with an indexed `.ledgerful` state directory,
/// ready for the `ledgerful` binary to be invoked against it via
/// `current_dir`. Returns the repo root.
fn setup_indexed_repo() -> tempfile::TempDir {
    let tmp = tempdir().unwrap();
    let root = tmp.path();

    setup_git_repo(root);
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("src/lib.rs"),
        "pub fn hotspot_fn(x: i32) -> i32 {\n    if x > 0 { x + 1 } else { x - 1 }\n}\n",
    )
    .unwrap();
    git_add_and_commit(root, "initial");

    // TemporalEngine::calculate_couplings requires at least 10 commits of
    // history (see GitError::InsufficientHistory in src/git/mod.rs), and
    // persist_hotspots_and_couplings (reused by the --bootstrap path) always
    // computes couplings alongside hotspots. Touch the file across enough
    // additional commits to clear that floor with margin.
    for i in 1..=12 {
        fs::write(
            root.join("src/lib.rs"),
            format!(
                "pub fn hotspot_fn(x: i32) -> i32 {{\n    if x > {i} {{ x + 1 }} else {{ x - 1 }}\n}}\n"
            ),
        )
        .unwrap();
        git_add_and_commit(root, &format!("touch {i}"));
    }

    let _guard = DirGuard::new(root);
    execute_init(false).unwrap();
    execute_index(IndexArgs::default()).unwrap();

    tmp
}

/// Builds a minimal git repo with an indexed `.ledgerful` state directory,
/// like `setup_indexed_repo`, but deliberately kept *under* the 10-commit
/// floor that `TemporalEngine::calculate_couplings` requires (see
/// `GitError::InsufficientHistory` in src/git/mod.rs). This is the
/// "first-time user" scenario CG-F30's `--bootstrap` flag exists to help:
/// a young repo that has never had a hotspot snapshot.
fn setup_young_indexed_repo() -> tempfile::TempDir {
    let tmp = tempdir().unwrap();
    let root = tmp.path();

    setup_git_repo(root);
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("src/lib.rs"),
        "pub fn hotspot_fn(x: i32) -> i32 {\n    if x > 0 { x + 1 } else { x - 1 }\n}\n",
    )
    .unwrap();
    git_add_and_commit(root, "initial");

    // Only a handful of additional commits, well below the 10-commit floor
    // required for temporal coupling history.
    for i in 1..=3 {
        fs::write(
            root.join("src/lib.rs"),
            format!(
                "pub fn hotspot_fn(x: i32) -> i32 {{\n    if x > {i} {{ x + 1 }} else {{ x - 1 }}\n}}\n"
            ),
        )
        .unwrap();
        git_add_and_commit(root, &format!("touch {i}"));
    }

    let _guard = DirGuard::new(root);
    execute_init(false).unwrap();
    execute_index(IndexArgs::default()).unwrap();

    tmp
}

fn hotspot_history_count(root: &std::path::Path) -> i64 {
    let repo_root = Utf8Path::from_path(root).unwrap();
    let storage = StorageManager::open_read_only_sqlite_only(repo_root).unwrap();
    let conn = storage.get_connection();
    conn.query_row("SELECT COUNT(*) FROM hotspot_history", [], |row| row.get(0))
        .unwrap()
}

fn hotspot_trends_count(root: &std::path::Path) -> i64 {
    let repo_root = Utf8Path::from_path(root).unwrap();
    let storage = StorageManager::open_read_only_sqlite_only(repo_root).unwrap();
    let conn = storage.get_connection();
    conn.query_row("SELECT COUNT(*) FROM hotspot_trends", [], |row| row.get(0))
        .unwrap()
}

#[test]
fn test_trend_no_history_non_bootstrap_human_shows_exact_command() {
    let tmp = setup_indexed_repo();
    let root = tmp.path();

    let ledgerful_bin = env!("CARGO_BIN_EXE_ledgerful");
    let output = Command::new(ledgerful_bin)
        .args(["hotspots", "trend"])
        .current_dir(root)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "CLI command failed: {:?}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        stdout.contains("No trend history yet for this repository."),
        "expected explicit no-history explanation, got: {stdout}"
    );
    assert!(
        stdout.contains("ledgerful hotspots trend --bootstrap"),
        "expected the exact bootstrap command to be printed, got: {stdout}"
    );

    // Read-only contract: history must remain untouched.
    assert_eq!(
        hotspot_history_count(root),
        0,
        "plain `hotspots trend` must not mutate hotspot_history"
    );
}

#[test]
fn test_trend_no_history_non_bootstrap_json_shape_and_read_only() {
    let tmp = setup_indexed_repo();
    let root = tmp.path();

    let ledgerful_bin = env!("CARGO_BIN_EXE_ledgerful");
    let output = Command::new(ledgerful_bin)
        .args(["hotspots", "trend", "--json"])
        .current_dir(root)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "CLI command failed: {:?}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let json: serde_json::Value = serde_json::from_str(&stdout).unwrap();

    assert_eq!(json["history_available"], serde_json::json!(false));
    assert_eq!(
        json["bootstrap_hint"],
        serde_json::json!("ledgerful hotspots trend --bootstrap")
    );
    assert!(json["entries"].as_array().unwrap().is_empty());

    // Read-only contract: history must remain untouched.
    assert_eq!(
        hotspot_history_count(root),
        0,
        "plain `hotspots trend --json` must not mutate hotspot_history"
    );
}

#[test]
fn test_trend_bootstrap_on_empty_history_creates_one_snapshot_and_reports_available() {
    let tmp = setup_indexed_repo();
    let root = tmp.path();

    assert_eq!(
        hotspot_history_count(root),
        0,
        "precondition: empty history"
    );

    let ledgerful_bin = env!("CARGO_BIN_EXE_ledgerful");
    let output = Command::new(ledgerful_bin)
        .args(["hotspots", "trend", "--bootstrap", "--json"])
        .current_dir(root)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "CLI command failed: {:?}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let json: serde_json::Value = serde_json::from_str(&stdout).unwrap();

    assert_eq!(json["history_available"], serde_json::json!(true));
    assert_eq!(json["bootstrap_hint"], serde_json::Value::Null);
    assert!(
        !json["entries"].as_array().unwrap().is_empty(),
        "expected the freshly bootstrapped snapshot to be visible in entries, got: {stdout}"
    );

    let rows_after = hotspot_trends_count(root);
    assert!(
        rows_after > 0,
        "expected --bootstrap to persist rows into hotspot_trends"
    );

    // Human-readable variant should also explain this is a first snapshot.
    let output_human = Command::new(ledgerful_bin)
        .args(["hotspots", "trend", "--bootstrap"])
        .current_dir(root)
        .output()
        .unwrap();
    // Second human run: history already exists at this point, so it should be
    // reported as a no-op rather than creating a duplicate snapshot. Checked
    // fully in the dedicated idempotency test below; here we just confirm this
    // run's own success and non-empty output.
    assert!(
        output_human.status.success(),
        "CLI human command failed: {:?}",
        String::from_utf8_lossy(&output_human.stderr)
    );
    let stdout_human = String::from_utf8_lossy(&output_human.stdout);
    assert!(
        stdout_human.contains('\u{253C}'),
        "expected a premium table border in human output, got: {stdout_human}"
    );
    assert!(
        stdout_human.contains("Score"),
        "expected tabular 'Score' header in human output, got: {stdout_human}"
    );
}

#[test]
fn test_trend_bootstrap_is_idempotent_after_history_exists() {
    let tmp = setup_indexed_repo();
    let root = tmp.path();

    let ledgerful_bin = env!("CARGO_BIN_EXE_ledgerful");

    // First --bootstrap: should create the initial snapshot.
    let first = Command::new(ledgerful_bin)
        .args(["hotspots", "trend", "--bootstrap", "--json"])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        first.status.success(),
        "first bootstrap failed: {:?}",
        String::from_utf8_lossy(&first.stderr)
    );
    let rows_after_first = hotspot_trends_count(root);
    assert!(
        rows_after_first > 0,
        "expected first --bootstrap to persist hotspot_trends rows"
    );

    // Second --bootstrap: history already exists, must be a no-op.
    let second = Command::new(ledgerful_bin)
        .args(["hotspots", "trend", "--bootstrap"])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        second.status.success(),
        "second bootstrap failed: {:?}",
        String::from_utf8_lossy(&second.stderr)
    );
    let stdout_second = String::from_utf8_lossy(&second.stdout);
    assert!(
        stdout_second.contains("History already exists")
            && stdout_second.contains("--bootstrap was skipped"),
        "expected the second bootstrap run to report itself as skipped, got: {stdout_second}"
    );

    let rows_after_second = hotspot_trends_count(root);
    assert_eq!(
        rows_after_first, rows_after_second,
        "a second --bootstrap run must not create duplicate hotspot_trends rows"
    );
}

#[test]
fn test_trend_bootstrap_succeeds_on_young_repo_with_insufficient_coupling_history() {
    let tmp = setup_young_indexed_repo();
    let root = tmp.path();

    assert_eq!(
        hotspot_history_count(root),
        0,
        "precondition: empty history"
    );

    let ledgerful_bin = env!("CARGO_BIN_EXE_ledgerful");
    let output = Command::new(ledgerful_bin)
        .args(["hotspots", "trend", "--bootstrap", "--json"])
        .current_dir(root)
        .output()
        .unwrap();

    // This is the regression check for CG-F30 Fix 1: persist_hotspots_and_couplings
    // used to hard-fail with GitError::InsufficientHistory (and roll back the
    // entire snapshot, including the already-inserted hotspot rows) whenever the
    // repo had fewer than 10 commits. That broke --bootstrap for exactly the
    // first-time-user, young-repo scenario it exists to help. It must now
    // succeed and persist hotspot rows, only skipping temporal coupling history.
    assert!(
        output.status.success(),
        "expected `hotspots trend --bootstrap` to succeed on a young repo (fewer than \
         10 commits) instead of erroring out on insufficient temporal-coupling history: {:?}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let json: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(json["history_available"], serde_json::json!(true));
    assert!(
        !json["entries"].as_array().unwrap().is_empty(),
        "expected the freshly bootstrapped hotspot snapshot to be visible in entries, got: {stdout}"
    );

    let rows_after = hotspot_trends_count(root);
    assert!(
        rows_after > 0,
        "expected --bootstrap to persist hotspot_trends rows even though temporal \
         coupling history was skipped"
    );

    // Human-readable run should truthfully disclose that coupling history was
    // skipped rather than silently omitting it (operator-surface-policy:
    // truthful over optimistic).
    let tmp_human = setup_young_indexed_repo();
    let root_human = tmp_human.path();
    let output_human = Command::new(ledgerful_bin)
        .args(["hotspots", "trend", "--bootstrap"])
        .current_dir(root_human)
        .output()
        .unwrap();
    assert!(
        output_human.status.success(),
        "CLI human command failed: {:?}",
        String::from_utf8_lossy(&output_human.stderr)
    );
    let stdout_human = String::from_utf8_lossy(&output_human.stdout);
    assert!(
        stdout_human.contains("Bootstrapped hotspot trend history from historical commits."),
        "expected bootstrap completion message, got: {stdout_human}"
    );
    assert!(
        stdout_human.contains('\u{253C}'),
        "expected a premium table border in human output, got: {stdout_human}"
    );
    assert!(
        stdout_human.contains("Score"),
        "expected tabular 'Score' header in human output, got: {stdout_human}"
    );
}
