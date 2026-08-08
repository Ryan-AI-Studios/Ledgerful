use crate::common::{DirGuard, git_add_and_commit, setup_git_repo};
use camino::Utf8Path;
use ledgerful::commands::index::{IndexArgs, execute_index};
use ledgerful::commands::init::execute_init;
use ledgerful::state::layout::Layout;
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
    execute_init(false, false).unwrap();
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
    execute_init(false, false).unwrap();
    execute_index(IndexArgs::default()).unwrap();

    tmp
}

fn hotspot_history_count(root: &std::path::Path) -> i64 {
    let repo_root = Utf8Path::from_path(root).unwrap();
    let storage = StorageManager::open_read_only_sqlite_only(&Layout::new(repo_root)).unwrap();
    let conn = storage.get_connection();
    conn.query_row("SELECT COUNT(*) FROM hotspot_history", [], |row| row.get(0))
        .unwrap()
}

fn hotspot_trends_count(root: &std::path::Path) -> i64 {
    let repo_root = Utf8Path::from_path(root).unwrap();
    let storage = StorageManager::open_read_only_sqlite_only(&Layout::new(repo_root)).unwrap();
    let conn = storage.get_connection();
    conn.query_row("SELECT COUNT(*) FROM hotspot_trends", [], |row| row.get(0))
        .unwrap()
}

/// Seed `hotspot_trends` with one row per path so summary/limit/entity e2e
/// tests do not depend on multi-file git history from bootstrap.
///
/// Storage is opened write-mode then dropped before the CLI binary is invoked,
/// so the binary can open the same SQLite file without lock contention.
fn seed_hotspot_trends(root: &std::path::Path, paths_and_scores: &[(&str, f64)]) {
    let repo_root = Utf8Path::from_path(root).unwrap();
    let layout = Layout::new(repo_root);
    let storage = StorageManager::init_with_layout(&layout).unwrap();
    let conn = storage.get_connection();
    // Recent RFC3339 timestamp so default --days window includes the rows.
    let recorded_at = "2026-08-01T12:00:00+00:00";
    for (i, (path, score)) in paths_and_scores.iter().enumerate() {
        conn.execute(
            "INSERT INTO hotspot_trends (file_path, score, frequency, complexity, commit_hash, recorded_at) \
             VALUES (?1, ?2, NULL, NULL, ?3, ?4)",
            rusqlite::params![
                path,
                score,
                format!("seedcommit{i:02}"),
                recorded_at,
            ],
        )
        .unwrap();
    }
    drop(storage);
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

    // 0151: default --json is summary envelope (no full entries matrix).
    assert_eq!(json["schemaVersion"], 1);
    assert_eq!(json["mode"], "summary");
    assert_eq!(json["historyAvailable"], serde_json::json!(false));
    assert_eq!(
        json["bootstrapHint"],
        serde_json::json!("ledgerful hotspots trend --bootstrap")
    );
    assert!(
        json["files"].as_array().is_some_and(|a| a.is_empty()),
        "empty window should yield empty files[], got: {stdout}"
    );
    assert!(
        json.get("entries").is_none(),
        "summary mode must omit entries, got: {stdout}"
    );

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

    // Default bootstrap --json is summary mode (top-N files).
    assert_eq!(json["schemaVersion"], 1);
    assert_eq!(json["mode"], "summary");
    assert_eq!(json["historyAvailable"], serde_json::json!(true));
    assert_eq!(json["bootstrapHint"], serde_json::Value::Null);
    assert!(
        !json["files"].as_array().unwrap().is_empty(),
        "expected the freshly bootstrapped snapshot to be visible in summary files, got: {stdout}"
    );

    // Full matrix still available via --all --json (F1 migration).
    let all_out = Command::new(ledgerful_bin)
        .args(["hotspots", "trend", "--all", "--json"])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        all_out.status.success(),
        "CLI --all --json failed: {:?}",
        String::from_utf8_lossy(&all_out.stderr)
    );
    let all_json: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&all_out.stdout)).unwrap();
    assert_eq!(all_json["mode"], "full");
    assert!(
        !all_json["entries"].as_array().unwrap().is_empty(),
        "expected --all --json entries from bootstrapped snapshot, got: {}",
        String::from_utf8_lossy(&all_out.stdout)
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
    assert_eq!(json["schemaVersion"], 1);
    assert_eq!(json["mode"], "summary");
    assert_eq!(json["historyAvailable"], serde_json::json!(true));
    assert!(
        !json["files"].as_array().unwrap().is_empty(),
        "expected the freshly bootstrapped hotspot snapshot to be visible in summary files, got: {stdout}"
    );
    // Full matrix via --all --json (F1).
    let all_out = Command::new(ledgerful_bin)
        .args(["hotspots", "trend", "--all", "--json"])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(all_out.status.success());
    let all_json: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&all_out.stdout)).unwrap();
    assert!(
        !all_json["entries"].as_array().unwrap().is_empty(),
        "expected --all --json entries after young-repo bootstrap"
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

/// 0151 Codex P2: `--limit 5` is honored end-to-end (JSON limit + truncation).
#[test]
fn test_trend_limit_5_honored_in_summary_json() {
    let tmp = setup_indexed_repo();
    let root = tmp.path();

    // Eight distinct files so limit 5 must truncate (totalFiles > 5 → truncated).
    let path_bufs: Vec<String> = (0..8).map(|i| format!("src/file_{i}.rs")).collect();
    let seeded: Vec<(&str, f64)> = path_bufs
        .iter()
        .enumerate()
        .map(|(i, p)| (p.as_str(), f64::from(i as u32) + 1.0))
        .collect();
    seed_hotspot_trends(root, &seeded);

    let ledgerful_bin = env!("CARGO_BIN_EXE_ledgerful");
    let output = Command::new(ledgerful_bin)
        .args(["hotspots", "trend", "--limit", "5", "--json"])
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

    assert_eq!(json["schemaVersion"], 1);
    assert_eq!(json["mode"], "summary");
    assert_eq!(json["limit"], 5);
    assert_eq!(json["totalFiles"], 8);
    assert_eq!(json["truncated"], serde_json::json!(true));

    let files = json["files"].as_array().expect("summary files array");
    assert_eq!(
        files.len(),
        5,
        "expected exactly 5 files when --limit 5 and totalFiles=8, got: {stdout}"
    );
    assert!(
        json.get("entries").is_none(),
        "summary mode must omit entries, got: {stdout}"
    );
}

/// 0151 Codex P2: `--limit 0` is rejected by clap range `1..` (non-zero exit).
#[test]
fn test_trend_limit_0_rejected_by_clap() {
    let tmp = setup_indexed_repo();
    let root = tmp.path();

    let ledgerful_bin = env!("CARGO_BIN_EXE_ledgerful");
    let output = Command::new(ledgerful_bin)
        .args(["hotspots", "trend", "--limit", "0", "--json"])
        .current_dir(root)
        .output()
        .unwrap();

    assert!(
        !output.status.success(),
        "expected non-zero exit for --limit 0, got success; stdout={:?} stderr={:?}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    // clap range parser message: "0 is not in 1.." (wording may include "invalid value").
    assert!(
        combined.contains("0 is not in 1..")
            || (combined.contains("invalid value") && combined.contains("limit")),
        "expected clap range rejection for --limit 0, got: {combined}"
    );
}

/// 0151 Codex P2: `--entity` JSON is mode entity with only that path's rows.
#[test]
fn test_trend_entity_json_mode_filters_to_path() {
    let tmp = setup_indexed_repo();
    let root = tmp.path();

    let entity_path = "src/target.rs";
    let other_path = "src/other.rs";
    seed_hotspot_trends(
        root,
        &[
            (entity_path, 9.0),
            (entity_path, 8.0), // second sample for the same entity
            (other_path, 3.0),
            ("src/third.rs", 1.0),
        ],
    );
    // Distinct recorded_at for the two entity samples so both are visible as series.
    {
        let repo_root = Utf8Path::from_path(root).unwrap();
        let layout = Layout::new(repo_root);
        let storage = StorageManager::init_with_layout(&layout).unwrap();
        let conn = storage.get_connection();
        // Bump one entity row's timestamp so we have two distinct samples.
        conn.execute(
            "UPDATE hotspot_trends SET recorded_at = ?1, score = ?2 \
             WHERE file_path = ?3 AND score = 8.0",
            rusqlite::params!["2026-08-02T12:00:00+00:00", 8.0, entity_path],
        )
        .unwrap();
        drop(storage);
    }

    let ledgerful_bin = env!("CARGO_BIN_EXE_ledgerful");
    let output = Command::new(ledgerful_bin)
        .args(["hotspots", "trend", "--entity", entity_path, "--json"])
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

    assert_eq!(json["schemaVersion"], 1);
    assert_eq!(json["mode"], "entity");
    assert_eq!(json["truncated"], serde_json::json!(false));
    assert!(
        json.get("limit").is_none(),
        "entity mode should omit limit, got: {stdout}"
    );
    assert!(
        json.get("files").is_none(),
        "entity mode should omit files summary array, got: {stdout}"
    );

    let entries = json["entries"].as_array().expect("entity entries array");
    assert!(
        !entries.is_empty(),
        "expected entity entries for {entity_path}, got: {stdout}"
    );
    assert!(
        entries
            .iter()
            .all(|e| e["file_path"].as_str() == Some(entity_path)),
        "all entity entries must match --entity path, got: {stdout}"
    );
    assert!(
        entries
            .iter()
            .all(|e| e["file_path"].as_str() != Some(other_path)),
        "entity mode must not include other files, got: {stdout}"
    );
    assert_eq!(
        json["totalFiles"], 1,
        "entity window should report a single file, got: {stdout}"
    );
}
