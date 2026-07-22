use crate::common::{DirGuard, TempEnv, git_add_and_commit_no_verify, setup_git_repo};
use camino::Utf8Path;
use ledgerful::commands::hook_post_commit::{
    execute_hook_post_commit, execute_hook_post_commit_for_layout,
};
use ledgerful::commands::index::{IndexArgs, execute_index};
use ledgerful::commands::init::execute_init;
use ledgerful::ledger::crypto::get_or_create_keys_in;
use ledgerful::state::layout::Layout;
use ledgerful::state::storage::StorageManager;
use serial_test::serial;

use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::Duration;
use tempfile::tempdir;

/// Bounded poll helper used because the production hook records trends in a
/// background thread. Matches `tests/integration/common/sync.rs`.
fn wait_for_condition<F: Fn() -> bool>(
    check: F,
    timeout: Duration,
    interval: Duration,
) -> Result<(), ()> {
    let start = std::time::Instant::now();
    while start.elapsed() < timeout {
        if check() {
            return Ok(());
        }
        std::thread::sleep(interval);
    }
    Err(())
}

fn keys_dir(root: &Path) -> std::path::PathBuf {
    root.join(".ledgerful").join("keys")
}

/// Builds a minimal git repo with an indexed `.ledgerful` state directory,
/// ready for the post-commit hook to be invoked. Returns the repo root.
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
    git_add_and_commit_no_verify(root, "initial");

    // Enough commits for temporal coupling history to succeed, mirroring
    // cli_hotspots::setup_indexed_repo.
    for i in 1..=12 {
        fs::write(
            root.join("src/lib.rs"),
            format!(
                "pub fn hotspot_fn(x: i32) -> i32 {{\n    if x > {i} {{ x + 1 }} else {{ x - 1 }}\n}}\n"
            ),
        )
        .unwrap();
        git_add_and_commit_no_verify(root, &format!("touch {i}"));
    }

    // Pre-generate keys directly in the temp repo's key dir so the
    // commit hook path uses the injected keys dir instead of real home.
    let kdir = keys_dir(root);
    fs::create_dir_all(&kdir).unwrap();
    let _ = get_or_create_keys_in(&kdir);

    // Point HOME/USERPROFILE at the temp repo root so that the production
    // env-resolving path finds the same keys we just generated. The guard
    // is needed because the hook code (and CLI subprocesses) still resolve
    // the keys dir from env, not from DI.
    let _home_guard_home = TempEnv::set("HOME", root.to_str().unwrap());
    let _home_guard_profile = TempEnv::set("USERPROFILE", root.to_str().unwrap());

    let _guard = DirGuard::new(root);
    execute_init(false, false).unwrap();
    execute_index(IndexArgs::default()).unwrap();

    // Remove the auto-installed post-commit hook so tests can drive the hook
    // explicitly. The ledger gate pre-commit/pre-push hooks are also removed
    // to avoid them firing during test commits.
    let hooks_dir = root.join(".git").join("hooks");
    for hook in ["post-commit", "pre-commit", "pre-push", "commit-msg"] {
        let _ = fs::remove_file(hooks_dir.join(hook));
    }

    // The current working directory must remain in the temp repo so that the
    // in-process hook entry point `execute_hook_post_commit` (used by the
    // non-blocking test) resolves the repository correctly.
    let _ = std::env::set_current_dir(root);

    tmp
}

fn hotspot_trends_count(root: &std::path::Path) -> i64 {
    let repo_root = Utf8Path::from_path(root).unwrap();
    let storage = StorageManager::open_read_only_sqlite_only(repo_root).unwrap();
    let conn = storage.get_connection();
    conn.query_row("SELECT COUNT(*) FROM hotspot_trends", [], |row| row.get(0))
        .unwrap()
}

fn hotspot_trends_for_commit(root: &std::path::Path, commit_hash: &str) -> i64 {
    let repo_root = Utf8Path::from_path(root).unwrap();
    let storage = StorageManager::open_read_only_sqlite_only(repo_root).unwrap();
    let conn = storage.get_connection();
    conn.query_row(
        "SELECT COUNT(*) FROM hotspot_trends WHERE commit_hash = ?1",
        [commit_hash],
        |row| row.get(0),
    )
    .unwrap()
}

/// Like `setup_indexed_repo` but configured so the commit-msg and post-commit
/// hooks can be driven directly in-process via the `ledgerful internal hook-*`
/// CLI entry points.  This avoids relying on git auto-executing shell hooks in
/// the test runner, while still exercising the real hook code paths.
///
/// The returned guard owns the temp directory *and* the environment guards so
/// the HOME/USERPROFILE redirection and cwd stay in effect for the lifetime of
/// the test.
struct HookedRepo {
    tmp: tempfile::TempDir,
    _home_guard_home: crate::common::TempEnv,
    _home_guard_profile: crate::common::TempEnv,
    _dir_guard: DirGuard,
}

impl HookedRepo {
    fn path(&self) -> &std::path::Path {
        self.tmp.path()
    }
}

fn setup_repo_with_hooks() -> HookedRepo {
    let tmp = tempdir().unwrap();
    let root = tmp.path();

    setup_git_repo(root);
    fs::write(root.join(".gitignore"), ".ledgerful/\n").unwrap();
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("src/lib.rs"),
        "pub fn hotspot_fn(x: i32) -> i32 {\n    if x > 0 { x + 1 } else { x - 1 }\n}\n",
    )
    .unwrap();
    git_add_and_commit_no_verify(root, "initial");

    // Pre-generate keys directly in the temp repo's key dir so the
    // commit hook path uses the injected keys dir instead of real home.
    let kdir = keys_dir(root);
    fs::create_dir_all(&kdir).unwrap();
    let _ = get_or_create_keys_in(&kdir);

    // Point HOME/USERPROFILE at the temp repo root so that the production
    // env-resolving path finds the same keys we just generated.
    let home_guard_home = crate::common::TempEnv::set("HOME", root.to_str().unwrap());
    let home_guard_profile = crate::common::TempEnv::set("USERPROFILE", root.to_str().unwrap());

    let dir_guard = DirGuard::new(root);
    execute_init(false, false).unwrap();
    execute_index(IndexArgs::default()).unwrap();

    // Enable rename detection so `git show --numstat -z` emits renames as a
    // single record with old\0new paths.
    let _ = Command::new("git")
        .args(["config", "diff.renames", "true"])
        .current_dir(root)
        .output();

    // Remove all auto-installed hooks: we drive the hook commands explicitly
    // so we do not depend on git executing shell scripts in this environment.
    let hooks_dir = root.join(".git").join("hooks");
    for hook in ["commit-msg", "post-commit", "pre-commit", "pre-push"] {
        let _ = fs::remove_file(hooks_dir.join(hook));
    }

    HookedRepo {
        tmp,
        _home_guard_home: home_guard_home,
        _home_guard_profile: home_guard_profile,
        _dir_guard: dir_guard,
    }
}

fn ledgerful_bin() -> &'static str {
    std::env!("CARGO_BIN_EXE_ledgerful")
}

/// Stage all changes, run the commit-msg hook to create the pending sidecar
/// (and staged snapshot), then commit with `--no-verify` so git does not try
/// to auto-run hooks, and finally run the post-commit hook directly.
fn commit_through_hooks(root: &std::path::Path, msg: &str) -> std::process::Output {
    Command::new("git")
        .args(["add", "-A"])
        .current_dir(root)
        .output()
        .unwrap();

    let msg_file = root.join(".git").join("COMMIT_EDITMSG");
    // Match the trailing newline that `git log -1 --format=%B` will return
    // so the sidecar hash matches the committed message hash.
    fs::write(&msg_file, format!("{msg}\n")).unwrap();

    let hook_output = Command::new(ledgerful_bin())
        .args(["internal", "hook-commit-msg", msg_file.to_str().unwrap()])
        .current_dir(root)
        .env("LEDGERFUL_NON_INTERACTIVE", "1")
        .output()
        .unwrap();
    assert!(
        hook_output.status.success(),
        "hook-commit-msg failed: {}\nstdout: {}",
        String::from_utf8_lossy(&hook_output.stderr),
        String::from_utf8_lossy(&hook_output.stdout)
    );

    let sidecar_path = root
        .join(".ledgerful")
        .join("state")
        .join("pending_hook_tx");
    assert!(
        sidecar_path.exists(),
        "pending_hook_tx sidecar should exist after hook-commit-msg; stdout: {}, stderr: {}",
        String::from_utf8_lossy(&hook_output.stdout),
        String::from_utf8_lossy(&hook_output.stderr)
    );

    // Use -F so the committed message bytes exactly match the message-file
    // bytes that the commit-msg hook hashed.
    let commit_output = Command::new("git")
        .args(["commit", "--no-verify", "-F", msg_file.to_str().unwrap()])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        commit_output.status.success(),
        "git commit failed: {}\nstdout: {}",
        String::from_utf8_lossy(&commit_output.stderr),
        String::from_utf8_lossy(&commit_output.stdout)
    );

    let post_output = Command::new(ledgerful_bin())
        .args(["internal", "hook-post-commit"])
        .current_dir(root)
        .env("LEDGERFUL_NON_INTERACTIVE", "1")
        .output()
        .unwrap();
    assert!(
        post_output.status.success(),
        "hook-post-commit failed: {}\nstdout: {}",
        String::from_utf8_lossy(&post_output.stderr),
        String::from_utf8_lossy(&post_output.stdout)
    );
    assert!(
        !sidecar_path.exists(),
        "pending_hook_tx sidecar should be removed after successful promotion; stdout: {}, stderr: {}",
        String::from_utf8_lossy(&post_output.stdout),
        String::from_utf8_lossy(&post_output.stderr)
    );

    commit_output
}

fn changed_files_stats_for_tx(
    root: &std::path::Path,
    tx_id: &str,
) -> Vec<(String, Option<i64>, Option<i64>, bool)> {
    let repo_root = Utf8Path::from_path(root).unwrap();
    let storage = StorageManager::open_read_only_sqlite_only(repo_root).unwrap();
    let conn = storage.get_connection();
    let mut stmt = conn
        .prepare(
            "SELECT cf.path, cf.additions, cf.deletions, cf.is_binary
             FROM changed_files cf
             JOIN transactions t ON t.snapshot_id = cf.snapshot_id
             WHERE t.tx_id = ?1
             ORDER BY cf.path",
        )
        .unwrap();
    let rows = stmt
        .query_map(rusqlite::params![tx_id], |row| {
            let is_binary_val: i64 = row.get(3).unwrap_or(0);
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<i64>>(1)?,
                row.get::<_, Option<i64>>(2)?,
                is_binary_val != 0,
            ))
        })
        .unwrap();
    rows.map(|r| r.unwrap()).collect()
}

fn latest_committed_tx_id(root: &std::path::Path) -> Option<String> {
    let repo_root = Utf8Path::from_path(root).unwrap();
    let storage = StorageManager::open_read_only_sqlite_only(repo_root).unwrap();
    let conn = storage.get_connection();
    // `transactions` has no integer id column; use SQLite rowid for insertion
    // order.
    conn.query_row(
        "SELECT tx_id FROM transactions WHERE status = 'COMMITTED' ORDER BY rowid DESC LIMIT 1",
        [],
        |row| row.get::<_, String>(0),
    )
    .ok()
}

fn verify_latest_signature(root: &std::path::Path) -> bool {
    let repo_root = Utf8Path::from_path(root).unwrap();
    let storage = StorageManager::open_read_only_sqlite_only(repo_root).unwrap();
    let db = ledgerful::ledger::db::LedgerDb::new(storage.get_connection());
    let tx_id = latest_committed_tx_id(root).expect("committed tx");
    let entries = db.get_ledger_entries_for_tx(&tx_id).expect("entries");
    let entry = entries.into_iter().next().expect("one entry");
    // Dual-verify by stored sig_version (v2 for new hook commits).
    ledgerful::ledger::crypto::verify_ledger_entry_signature(&entry)
}

#[test]
#[serial(env)]
fn test_post_commit_hook_records_hotspot_trends() {
    let tmp = setup_indexed_repo();
    let root = tmp.path();

    assert_eq!(
        hotspot_trends_count(root),
        0,
        "precondition: no hotspot_trends rows"
    );

    // Invoke the hook bound to the temp repo (synchronous ledger promotion +
    // background thread). Wait briefly for the background thread to finish
    // writing.
    let layout = Layout::new(Utf8Path::from_path(root).unwrap());
    execute_hook_post_commit_for_layout(&layout).unwrap();
    wait_for_condition(
        || hotspot_trends_count(root) > 0,
        Duration::from_secs(5),
        Duration::from_millis(50),
    )
    .expect("post-commit hook should record hotspot_trends rows");

    let head = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(root)
        .output()
        .unwrap();
    let head_hash = String::from_utf8_lossy(&head.stdout).trim().to_string();

    assert!(
        hotspot_trends_for_commit(root, &head_hash) > 0,
        "expected rows recorded for current HEAD"
    );
}

#[test]
fn test_post_commit_hook_is_non_blocking_and_exits_ok() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();

    // No git repo and no state — hook should still return Ok immediately.
    let _guard = DirGuard::new(root);
    let result = execute_hook_post_commit();
    assert!(
        result.is_ok(),
        "hook must silently exit 0 on missing repo/state"
    );
}

#[test]
#[serial(env)]
fn test_hotspots_trend_shows_multiple_timestamps_after_three_commits() {
    let tmp = setup_indexed_repo();
    let root = tmp.path();

    // Record trends for three distinct commits, waiting after each so the
    // background thread has time to write before we add the next commit.
    for i in 1..=3 {
        fs::write(
            root.join("src/lib.rs"),
            format!(
                "pub fn hotspot_fn(x: i32) -> i32 {{\n    if x > {i}00 {{ x + 1 }} else {{ x - 1 }}\n}}\n"
            ),
        )
        .unwrap();
        git_add_and_commit_no_verify(root, &format!("commit {i}"));
        let layout = Layout::new(Utf8Path::from_path(root).unwrap());
        execute_hook_post_commit_for_layout(&layout).unwrap();
        wait_for_condition(
            || hotspot_trends_count(root) >= i as i64,
            Duration::from_secs(5),
            Duration::from_millis(50),
        )
        .expect("hook should record at least one row per commit");
    }

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
    let entries = json["entries"].as_array().unwrap();
    assert!(
        entries.len() >= 3,
        "expected 3+ trend entries, got {}: {stdout}",
        entries.len()
    );
}

#[test]
#[serial(env)]
fn test_hotspots_trend_shows_staleness_hint_when_head_differs() {
    let tmp = setup_indexed_repo();
    let root = tmp.path();

    let layout = Layout::new(Utf8Path::from_path(root).unwrap());
    execute_hook_post_commit_for_layout(&layout).unwrap();
    wait_for_condition(
        || hotspot_trends_count(root) > 0,
        Duration::from_secs(5),
        Duration::from_millis(50),
    )
    .expect("hook should record initial trend");

    // Add a new commit so the last recorded hash no longer matches HEAD.
    fs::write(root.join("src/lib.rs"), "pub fn new_fn() {}\n").unwrap();
    git_add_and_commit_no_verify(root, "after hook");

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
        stdout.contains("Trend data is stale"),
        "expected staleness hint, got: {stdout}"
    );
}

#[test]
#[serial(env)]
fn test_post_commit_hook_records_text_diff_stats() {
    let repo = setup_repo_with_hooks();
    let root = repo.path();

    // Edit an existing text file so the committed diff has non-zero stats.
    fs::write(
        root.join("src/lib.rs"),
        "pub fn hotspot_fn(x: i32) -> i32 {\n    if x > 0 { x + 2 } else { x - 2 }\n}\n",
    )
    .unwrap();

    commit_through_hooks(root, "feat: tweak lib");

    let tx_id = latest_committed_tx_id(root).unwrap_or_else(|| {
        let db_path = root.join(".ledgerful").join("state").join("ledger.db");
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM transactions", [], |row| row.get(0))
            .unwrap();
        eprintln!("DEBUG: transactions count = {count}");
        if count > 0 {
            let rows: Vec<(String, String)> = conn
                .prepare("SELECT tx_id, status FROM transactions")
                .unwrap()
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })
                .unwrap()
                .map(|r| r.unwrap())
                .collect();
            for (id, status) in &rows {
                eprintln!("DEBUG: tx {id} status {status}");
            }
        }
        panic!("committed tx should exist");
    });
    let stats = changed_files_stats_for_tx(root, &tx_id);
    assert_eq!(stats.len(), 1, "expected one changed file");
    let (path, adds, dels, _is_binary) = &stats[0];
    assert_eq!(path, "src/lib.rs");
    assert!(
        adds.is_some() && adds.unwrap() > 0,
        "expected positive additions for text change, got {adds:?}"
    );
    assert!(
        dels.is_some() && dels.unwrap() > 0,
        "expected positive deletions for text change, got {dels:?}"
    );
}

#[test]
#[serial(env)]
fn test_post_commit_hook_records_binary_file_diff_stats_as_null() {
    let repo = setup_repo_with_hooks();
    let root = repo.path();

    // Add a PNG file with a valid magic header so git treats it as binary.
    fs::create_dir_all(root.join("assets")).unwrap();
    let mut png = vec![0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a];
    png.extend(std::iter::repeat_n(0u8, 64));
    fs::write(root.join("assets/icon.png"), &png).unwrap();

    commit_through_hooks(root, "feat: add icon");

    let tx_id = latest_committed_tx_id(root).expect("committed tx should exist");
    let stats = changed_files_stats_for_tx(root, &tx_id);
    let binary = stats
        .iter()
        .find(|(p, _, _, _)| p == "assets/icon.png")
        .expect("binary file should be in changed_files");
    assert_eq!(binary.1, None, "binary additions should be None/null");
    assert_eq!(binary.2, None, "binary deletions should be None/null");
    assert!(binary.3, "is_binary should be true for binary files");
}

#[test]
#[serial(env)]
fn test_post_commit_hook_records_rename_under_new_path() {
    let repo = setup_repo_with_hooks();
    let root = repo.path();

    fs::rename(root.join("src/lib.rs"), root.join("src/lib2.rs")).unwrap();

    commit_through_hooks(root, "refactor: rename lib");

    let tx_id = latest_committed_tx_id(root).expect("committed tx should exist");
    let stats = changed_files_stats_for_tx(root, &tx_id);
    let renamed = stats
        .iter()
        .find(|(p, _, _, _)| p == "src/lib2.rs")
        .expect("stats should be keyed under the rename destination");
    assert!(
        renamed.1.is_some() || renamed.2.is_some(),
        "rename stats should be populated (not NULL), got {renamed:?}"
    );
    assert!(
        !stats.iter().any(|(p, _, _, _)| p == "src/lib.rs"),
        "stats should not be keyed under the rename source, got {stats:?}"
    );
}

#[test]
#[serial(env)]
fn test_post_commit_hook_preserves_signature_with_diff_stats() {
    let repo = setup_repo_with_hooks();
    let root = repo.path();

    fs::write(
        root.join("src/lib.rs"),
        "pub fn hotspot_fn(x: i32) -> i32 {\n    if x > 0 { x + 3 } else { x - 3 }\n}\n",
    )
    .unwrap();

    commit_through_hooks(root, "feat: signed tweak");

    let tx_id = latest_committed_tx_id(root).expect("committed tx should exist");
    let stats = changed_files_stats_for_tx(root, &tx_id);
    assert_eq!(stats.len(), 1);
    assert!(stats[0].1.is_some(), "stats should be present");

    // The signature is computed over the ledger entry fields only; diff stats
    // are attached afterward and must not break verification.
    assert!(
        verify_latest_signature(root),
        "signature must remain valid after diff-stats are attached"
    );
}
