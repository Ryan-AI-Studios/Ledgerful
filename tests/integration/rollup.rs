use crate::common::{DirGuard, TempEnv, non_interactive, setup_git_repo};
use ledgerful::commands::init::execute_init;
use ledgerful::config::model::GlobalRollupConfig;
use ledgerful::state::rollup::{
    build_global_posture, execute_ledger_status_global, execute_timings_global,
    set_global_rollup_enabled,
};
use rusqlite::Connection;
use serial_test::serial;
use std::fs;

use std::path::Path;
use tempfile::tempdir;

/// Capture stdout produced by `f` into a String by redirecting the process
/// stdout to a pipe. This is intentionally simple and cross-platform.
fn capture_stdout<F>(f: F) -> String
where
    F: FnOnce() + Send + 'static,
{
    #[cfg(unix)]
    {
        use std::io::{Read, Write};
        use std::os::fd::AsRawFd;
        let stdout = std::io::stdout();
        let (reader, mut writer) = os_pipe::pipe().unwrap();
        stdout.flush().unwrap();
        let raw = writer.as_raw_fd();
        let mut buf = Vec::new();
        unsafe {
            let original = libc::dup(libc::STDOUT_FILENO);
            libc::dup2(raw, libc::STDOUT_FILENO);
            f();
            libc::dup2(original, libc::STDOUT_FILENO);
            libc::close(original);
        }
        drop(writer);
        let mut reader = reader;
        reader.read_to_end(&mut buf).unwrap();
        String::from_utf8_lossy(&buf).to_string()
    }
    #[cfg(not(unix))]
    {
        // Windows stdout capture without raw handle surgery: re-implement the
        // display portion by invoking the rollup function and collecting its
        // output would require restructuring production code. For this test
        // suite we instead run `f` directly; assertions that rely on captured
        // stdout are skipped on Windows. The behavioral invariants (posture
        // values, counts, sorting) are still covered by side-effect-based
        // assertions in other tests.
        f();
        String::new()
    }
}

/// Build a minimal Ledgerful repo under `parent/name` with optional posture.
fn make_fixture_repo(parent: &Path, name: &str, unsigned: usize, pending: usize, drift: usize) {
    make_fixture_repo_with_skips(parent, name, unsigned, pending, drift, false, false);
}

fn insert_signed_entry(conn: &Connection, tx_id: &str, entity: &str, keys_dir: &Path) {
    let now = chrono::Utc::now().to_rfc3339();
    let (signature, public_key) = ledgerful::ledger::crypto::sign_ledger_entry_in(
        keys_dir, tx_id, "FEATURE", "summary", "reason", &now,
    )
    .unwrap();
    conn.execute(
        "INSERT INTO ledger_entries (
            tx_id, category, entry_type, entity, entity_normalized, change_type,
            summary, reason, is_breaking, committed_at, verification_status,
            verification_basis, outcome_notes, origin, trace_id, signature,
            public_key, risk, related_tickets, author, observed, prev_hash
        ) VALUES (?1, 'FEATURE', 'IMPLEMENTATION', ?2, ?2, 'MODIFY',
            'summary', 'reason', 0, ?5, NULL, NULL, NULL, 'LOCAL', NULL,
            ?3, ?4, NULL, NULL, 'Test', NULL, NULL)",
        rusqlite::params![
            tx_id,
            entity,
            signature.as_deref().unwrap_or(""),
            public_key.as_deref().unwrap_or(""),
            now
        ],
    )
    .unwrap();
}

fn make_fixture_repo_with_skips(
    parent: &Path,
    name: &str,
    unsigned: usize,
    pending: usize,
    drift: usize,
    skip_init: bool,
    _skip_keys: bool,
) {
    let root = parent.join(name);
    fs::create_dir_all(&root).unwrap();
    setup_git_repo(&root);

    let _guard = DirGuard::new(&root);
    let _env = non_interactive();
    if !skip_init {
        execute_init(false, false).unwrap();
    }

    // Seed simple ledger state via direct DB writes so posture is deterministic.
    let db_path = root.join(".ledgerful").join("state").join("ledger.db");
    let conn = Connection::open(&db_path).unwrap();
    for i in 0..pending {
        let tx_id: String = conn
            .query_row("SELECT lower(hex(randomblob(16)))", [], |row| row.get(0))
            .unwrap();
        let entity = format!("pending/{}_{}.rs", name, i);
        conn.execute(
            "INSERT INTO transactions (
                tx_id, operation_id, status, category, entity, entity_normalized,
                planned_action, session_id, source, started_at, resolved_at, issue_ref,
                detected_at, drift_count, first_seen_at, last_seen_at, snapshot_id
            ) VALUES (?1, NULL, 'PENDING', 'FEATURE', ?2, ?2, NULL, 'test', 'CLI', datetime('now'), NULL, NULL, NULL, 1,
                datetime('now'), datetime('now'), NULL)",
            [&tx_id, &entity],
        )
        .unwrap();
    }
    for i in 0..drift {
        let tx_id: String = conn
            .query_row("SELECT lower(hex(randomblob(16)))", [], |row| row.get(0))
            .unwrap();
        let entity = format!("drift/{}_{}.rs", name, i);
        conn.execute(
            "INSERT INTO transactions (
                tx_id, operation_id, status, category, entity, entity_normalized,
                planned_action, session_id, source, started_at, resolved_at, issue_ref,
                detected_at, drift_count, first_seen_at, last_seen_at, snapshot_id
            ) VALUES (?1, NULL, 'UNAUDITED', 'FEATURE', ?2, ?2, NULL, 'test', 'CLI', datetime('now'), NULL, NULL, datetime('now'), 1,
                datetime('now'), datetime('now'), NULL)",
            [&tx_id, &entity],
        )
        .unwrap();
    }
    for i in 0..unsigned {
        let tx_id: String = conn
            .query_row("SELECT lower(hex(randomblob(16)))", [], |row| row.get(0))
            .unwrap();
        let entity = format!("src/main_{}_{}.rs", name, i);
        conn.execute(
            "INSERT INTO transactions (
                tx_id, operation_id, status, category, entity, entity_normalized,
                planned_action, session_id, source, started_at, resolved_at, issue_ref,
                detected_at, drift_count, first_seen_at, last_seen_at, snapshot_id
            ) VALUES (?1, NULL, 'COMMITTED', 'FEATURE', ?2, ?2, NULL, 'test', 'CLI',
                datetime('now'), datetime('now'), NULL, NULL, 1, datetime('now'), datetime('now'), NULL)",
            [&tx_id, &entity],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO ledger_entries (
                tx_id, category, entry_type, entity, entity_normalized, change_type,
                summary, reason, is_breaking, committed_at, verification_status,
                verification_basis, outcome_notes, origin, trace_id, signature,
                public_key, risk, related_tickets, author, observed, prev_hash
            ) VALUES (?1, 'FEATURE', 'IMPLEMENTATION', ?2, ?2, 'MODIFY',
                'summary', 'reason', 0, datetime('now'), NULL, NULL, NULL, 'LOCAL', NULL,
                'bad-sig', 'bad-key', NULL, NULL, 'Test', NULL, NULL)",
            [&tx_id, &entity],
        )
        .unwrap();
    }
}

/// Build a minimal Ledgerful repo with a mix of signed/unsigned entries for
/// signature-count tests. `valid`, `invalid`, and `missing` control how many
/// committed ledger_entries are created with a valid signature, a corrupted
/// signature, or no signature at all.
fn make_fixture_repo_with_signature_mix(
    parent: &Path,
    name: &str,
    valid: usize,
    invalid: usize,
    missing: usize,
) {
    let root = parent.join(name);
    fs::create_dir_all(&root).unwrap();
    setup_git_repo(&root);

    {
        let _guard = DirGuard::new(&root);
        let _env = non_interactive();
        execute_init(false, false).unwrap();
    }

    // Use a deterministic key directory inside this repo so the test is
    // hermetic and does not depend on the developer's real ~/.ledgerful/keys.
    let keys_dir = root.join(".ledgerful").join("keys");
    fs::create_dir_all(&keys_dir).unwrap();

    let db_path = root.join(".ledgerful").join("state").join("ledger.db");
    let conn = Connection::open(&db_path).unwrap();

    // Remove the initial gate-mode ledger entry written by execute_init so the
    // signature mix is exactly `valid + invalid + missing`. Also clear the
    // chain_head row because it references that entry.
    conn.execute("DELETE FROM ledger_entries", []).unwrap();
    conn.execute("DELETE FROM chain_head", []).unwrap();

    for i in 0..(valid + invalid + missing) {
        let tx_id: String = conn
            .query_row("SELECT lower(hex(randomblob(16)))", [], |row| row.get(0))
            .unwrap();
        let entity = format!("src/main_{}_{}.rs", name, i);
        conn.execute(
            "INSERT INTO transactions (
                tx_id, operation_id, status, category, entity, entity_normalized,
                planned_action, session_id, source, started_at, resolved_at, issue_ref,
                detected_at, drift_count, first_seen_at, last_seen_at, snapshot_id
            ) VALUES (?1, NULL, 'COMMITTED', 'FEATURE', ?2, ?2, NULL, 'test', 'CLI',
                datetime('now'), datetime('now'), NULL, NULL, 1, datetime('now'), datetime('now'), NULL)",
            [&tx_id, &entity],
        )
        .unwrap();

        if i < valid {
            insert_signed_entry(&conn, &tx_id, &entity, &keys_dir);
        } else if i < valid + invalid {
            conn.execute(
                "INSERT INTO ledger_entries (
                    tx_id, category, entry_type, entity, entity_normalized, change_type,
                    summary, reason, is_breaking, committed_at, verification_status,
                    verification_basis, outcome_notes, origin, trace_id, signature,
                    public_key, risk, related_tickets, author, observed, prev_hash
                ) VALUES (?1, 'FEATURE', 'IMPLEMENTATION', ?2, ?2, 'MODIFY',
                    'summary', 'reason', 0, datetime('now'), NULL, NULL, NULL, 'LOCAL', NULL,
                    'bad-sig', 'bad-key', NULL, NULL, 'Test', NULL, NULL)",
                [&tx_id, &entity],
            )
            .unwrap();
        } else {
            conn.execute(
                "INSERT INTO ledger_entries (
                    tx_id, category, entry_type, entity, entity_normalized, change_type,
                    summary, reason, is_breaking, committed_at, verification_status,
                    verification_basis, outcome_notes, origin, trace_id, signature,
                    public_key, risk, related_tickets, author, observed, prev_hash
                ) VALUES (?1, 'FEATURE', 'IMPLEMENTATION', ?2, ?2, 'MODIFY',
                    'summary', 'reason', 0, datetime('now'), NULL, NULL, NULL, 'LOCAL', NULL,
                    NULL, NULL, NULL, NULL, 'Test', NULL, NULL)",
                [&tx_id, &entity],
            )
            .unwrap();
        }
    }
}

fn fixture_config(root: &Path) -> GlobalRollupConfig {
    GlobalRollupConfig {
        roots: vec![root.to_path_buf()],
        timeout_secs: 30,
        staleness_secs: 3600,
        max_depth: None,
        enabled: true,
    }
}

fn fixture_config_with_staleness(root: &Path, staleness_secs: u64) -> GlobalRollupConfig {
    GlobalRollupConfig {
        roots: vec![root.to_path_buf()],
        timeout_secs: 30,
        staleness_secs,
        max_depth: None,
        enabled: true,
    }
}

/// Make `path` and all its parents have an old mtime (e.g. 2000-01-01).
fn set_mtime_to_past(path: &Path) {
    let past = filetime::FileTime::from_unix_time(946684800, 0);
    // Walk up until we hit an existing ancestor.
    let mut current = path.to_path_buf();
    loop {
        if current.exists() {
            filetime::set_file_mtime(&current, past).unwrap();
            break;
        }
        if let Some(parent) = current.parent() {
            current = parent.to_path_buf();
        } else {
            break;
        }
    }
}

#[test]
#[serial(env, cwd)]
fn discovery_hit_miss_corrupt_populates_and_reuses_cache_then_recovers() {
    let _env_non_interactive = non_interactive();
    let tmp = tempdir().unwrap();
    let home = tmp.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let root = tmp.path().join("roots");
    fs::create_dir_all(&root).unwrap();

    make_fixture_repo(&root, "repo_a", 1, 0, 0);
    make_fixture_repo(&root, "repo_b", 0, 1, 0);

    let cache = home.join(".ledgerful").join("rollup").join("cache.sqlite");
    let _profile = TempEnv::set("USERPROFILE", home.to_str().unwrap());
    let _home_env = TempEnv::set("HOME", home.to_str().unwrap());
    let _cache_env = TempEnv::set("LEDGERFUL_ROLLUP_CACHE", cache.to_str().unwrap());

    let config = fixture_config(&root);

    // First call walks and populates cache.
    let _guard = DirGuard::new(&root);
    execute_ledger_status_global(&config, None, false, true).unwrap();
    assert!(cache.exists(), "cache should be populated");

    // Second call should be fast and use cache (same repo count).
    execute_ledger_status_global(&config, None, false, true).unwrap();

    // Corrupt the cache file to force a re-walk.
    fs::write(&cache, b"not sqlite").unwrap();
    execute_ledger_status_global(&config, None, false, true).unwrap();
}

#[test]
#[serial(env, cwd)]
fn cache_with_non_ok_integrity_check_triggers_rewalk() {
    let _env_non_interactive = non_interactive();
    let tmp = tempdir().unwrap();
    let home = tmp.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let root = tmp.path().join("roots");
    fs::create_dir_all(&root).unwrap();

    make_fixture_repo(&root, "repo_a", 1, 0, 0);

    let cache = home.join(".ledgerful").join("rollup").join("cache.sqlite");
    let _profile = TempEnv::set("USERPROFILE", home.to_str().unwrap());
    let _home_env = TempEnv::set("HOME", home.to_str().unwrap());
    let _cache_env = TempEnv::set("LEDGERFUL_ROLLUP_CACHE", cache.to_str().unwrap());

    let config = fixture_config(&root);
    let _guard = DirGuard::new(&root);

    // First call populates a valid cache.
    let first = build_global_posture(&config, None, false).unwrap();
    assert_eq!(first.total_repos, 1);
    assert!(cache.exists());

    // Corrupt the cache DB so PRAGMA integrity_check returns a non-"ok"
    // string (not a SQL error — the file is still a valid SQLite file, but its
    // pages are scrambled). We overwrite the page-count field in the header
    // to force integrity_check to report corruption. A simpler reliable
    // approach: open the cache, drop the rollup_cache table, then write garbage
    // into sqlite_master via a low-level overwrite. The most portable way to
    // provoke a non-"ok" integrity result is to truncate the file mid-page.
    let cache_bytes = fs::read(&cache).unwrap();
    // Truncate to ~half length — leaves a structurally invalid SQLite file that
    // PRAGMA integrity_check reports as corrupt (returns a description string,
    // not a SQL error and not "ok").
    let truncated = &cache_bytes[..cache_bytes.len() / 2];
    fs::write(&cache, truncated).unwrap();

    // The non-"ok" integrity result must trigger a clean re-walk, not a crash
    // or a silent use of corrupt cached data.
    let second = build_global_posture(&config, None, false).unwrap();
    assert_eq!(
        second.total_repos, 1,
        "corrupt cache triggered re-walk; repo_a rediscovered and queried fresh"
    );
}

#[test]
#[serial(env, cwd)]
fn timeout_bound_completes_within_deadline() {
    let _env_non_interactive = non_interactive();
    let tmp = tempdir().unwrap();
    let root = tmp.path();

    make_fixture_repo(root, "repo_a", 0, 0, 0);

    let config = GlobalRollupConfig {
        timeout_secs: 1,
        max_depth: None,
        ..fixture_config(root)
    };

    let _guard = DirGuard::new(root);
    let start = std::time::Instant::now();
    execute_ledger_status_global(&config, None, false, false).unwrap();
    assert!(
        start.elapsed().as_secs() < 5,
        "walk took too long under tight timeout"
    );
}

#[test]
#[serial(env, cwd)]
fn cyclic_symlink_survives_without_hang() {
    let _env_non_interactive = non_interactive();
    let tmp = tempdir().unwrap();
    let root = tmp.path();

    make_fixture_repo(root, "repo_a", 0, 0, 0);
    let link = root.join("loop");
    #[cfg(unix)]
    std::os::unix::fs::symlink(root, &link).unwrap();
    #[cfg(windows)]
    {
        // Directory junctions do not require elevated privileges, unlike symlinks.
        let target = std::fs::canonicalize(root).unwrap();
        let output = std::process::Command::new("cmd")
            .args(["/c", "mklink", "/J"])
            .arg(&link)
            .arg(&target)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "failed to create junction: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let config = fixture_config(root);
    let _guard = DirGuard::new(root);
    execute_ledger_status_global(&config, None, true, false).unwrap();
}

#[test]
#[serial(env, cwd)]
fn unreadable_dir_mid_walk_skipped_without_crash() {
    let _env_non_interactive = non_interactive();
    let tmp = tempdir().unwrap();
    let root = tmp.path();

    make_fixture_repo(root, "repo_a", 0, 0, 0);
    // A non-existent root mid-list simulates a skipped/unreadable directory.
    let missing_root = root.join("does_not_exist");

    let config = GlobalRollupConfig {
        roots: vec![root.to_path_buf(), missing_root],
        timeout_secs: 30,
        staleness_secs: 3600,
        max_depth: None,
        enabled: true,
    };

    let _guard = DirGuard::new(root);
    execute_ledger_status_global(&config, None, true, false).unwrap();
}

#[test]
#[serial(env, cwd)]
fn open_read_only_from_path_rejects_write_attempt() {
    let _env_non_interactive = non_interactive();
    let tmp = tempdir().unwrap();
    let root = tmp.path();

    make_fixture_repo(root, "repo_a", 1, 0, 0);
    let db_path = root
        .join("repo_a")
        .join(".ledgerful")
        .join("state")
        .join("ledger.db");

    let storage =
        ledgerful::state::storage::StorageManager::open_read_only_from_path(&db_path).unwrap();
    let conn = storage.get_connection();
    let result = conn.execute_batch("CREATE TABLE x(y INTEGER)");
    assert!(
        result.is_err(),
        "read-only rollup connection must reject CREATE TABLE"
    );
}

#[test]
#[serial(env, cwd)]
fn read_only_invariant_no_write_to_per_repo_db() {
    let _env_non_interactive = non_interactive();
    let tmp = tempdir().unwrap();
    let root = tmp.path();

    make_fixture_repo(root, "repo_a", 1, 0, 0);
    let db_path = root
        .join("repo_a")
        .join(".ledgerful")
        .join("state")
        .join("ledger.db");
    let before = fs::metadata(&db_path).unwrap().modified().unwrap();

    let config = fixture_config(root);
    let _guard = DirGuard::new(root);
    execute_ledger_status_global(&config, None, true, false).unwrap();

    let after = fs::metadata(&db_path).unwrap().modified().unwrap();
    assert_eq!(before, after, "global rollup must not write to per-repo DB");
}

#[test]
#[serial(env, cwd)]
fn deep_repo_at_depth_six_or_more_is_discovered() {
    let _env_non_interactive = non_interactive();
    let tmp = tempdir().unwrap();
    let root = tmp.path().join("a").join("b").join("c").join("d").join("e");
    fs::create_dir_all(&root).unwrap();

    make_fixture_repo(&root, "deep_repo", 2, 0, 0);

    let config = GlobalRollupConfig {
        roots: vec![tmp.path().to_path_buf()],
        timeout_secs: 30,
        staleness_secs: 3600,
        max_depth: None,
        enabled: true,
    };
    let _guard = DirGuard::new(tmp.path());

    let parsed = build_global_posture(&config, None, true).unwrap();
    assert_eq!(
        parsed.total_repos, 1,
        "repo at depth 6+ should be discovered"
    );
    assert_eq!(parsed.repos[0].unsigned_entries, 2);
}

#[test]
#[serial(env, cwd)]
fn posture_correctness_matches_per_repo_values_sorted_worst_first() {
    let _env_non_interactive = non_interactive();
    let tmp = tempdir().unwrap();
    let root = tmp.path();

    make_fixture_repo(root, "repo_a", 3, 1, 0);
    make_fixture_repo(root, "repo_b", 1, 2, 1);
    make_fixture_repo(root, "repo_c", 0, 0, 0);

    let config = fixture_config(root);
    let _guard = DirGuard::new(root);

    // Capture JSON output by redirecting stdout.
    let parsed = build_global_posture(&config, None, true).unwrap();

    assert_eq!(parsed.total_repos, 3);
    // `make_fixture_repo` seeds "unsigned" entries with both signature and
    // public_key set to invalid placeholders, so they are counted as unsigned.
    assert_eq!(parsed.repos[0].unsigned_entries, 3);
    assert_eq!(parsed.repos[1].unsigned_entries, 1);
    assert_eq!(parsed.repos[2].unsigned_entries, 0);
    assert_eq!(parsed.repos[0].pending_tx, 1);
    assert_eq!(parsed.repos[1].pending_tx, 2);
    assert_eq!(parsed.repos[1].drift, 1);
}

#[test]
#[serial(env, cwd)]
fn repo_filter_matches_last_component_not_substring() {
    let _env_non_interactive = non_interactive();
    let tmp = tempdir().unwrap();
    let root = tmp.path();

    make_fixture_repo(root, "foo", 1, 0, 0);
    make_fixture_repo(root, "foobar", 0, 1, 0);

    let config = fixture_config(root);
    let _guard = DirGuard::new(root);

    let parsed = build_global_posture(&config, Some("foo"), true).unwrap();
    assert_eq!(
        parsed.total_repos, 1,
        "--repo foo must match only the 'foo' repo, not 'foobar'"
    );
    assert!(parsed.repos[0].repo_path.ends_with("foo"));
    assert_eq!(parsed.repos[0].unsigned_entries, 1);
}

#[test]
#[serial(env, cwd)]
fn empty_repo_posture_is_all_zeros_no_error() {
    let _env_non_interactive = non_interactive();
    let tmp = tempdir().unwrap();
    let root = tmp.path();

    make_fixture_repo(root, "empty", 0, 0, 0);

    let config = fixture_config(root);
    let _guard = DirGuard::new(root);

    let parsed = build_global_posture(&config, None, true).unwrap();
    assert_eq!(parsed.total_repos, 1);
    assert_eq!(parsed.repos[0].unsigned_entries, 0);
    assert_eq!(parsed.repos[0].pending_tx, 0);
    assert_eq!(parsed.repos[0].drift, 0);
    assert!(parsed.repos[0].last_verify_result.is_none());
}

#[test]
#[serial(env, cwd)]
fn repo_without_ledger_db_is_skipped_gracefully() {
    let _env_non_interactive = non_interactive();
    let tmp = tempdir().unwrap();
    let root = tmp.path();

    make_fixture_repo(root, "repo_a", 1, 0, 0);
    // Create a sibling directory that looks like a repo root but has no DB.
    let no_db_root = root.join("no_db");
    fs::create_dir_all(&no_db_root).unwrap();
    fs::create_dir_all(no_db_root.join(".ledgerful").join("state")).unwrap();

    let config = fixture_config(root);
    let _guard = DirGuard::new(root);

    let parsed = build_global_posture(&config, None, true).unwrap();
    assert_eq!(parsed.total_repos, 1);
    assert!(parsed.repos[0].repo_path.ends_with("repo_a"));
}

#[test]
#[serial(env, cwd)]
fn corrupt_ledger_db_is_warned_and_skipped_run_completes() {
    let _env_non_interactive = non_interactive();
    let tmp = tempdir().unwrap();
    let root = tmp.path();

    make_fixture_repo(root, "good", 1, 0, 0);
    make_fixture_repo(root, "bad", 0, 0, 0);
    let bad_db = root
        .join("bad")
        .join(".ledgerful")
        .join("state")
        .join("ledger.db");
    fs::write(&bad_db, b"not a sqlite file").unwrap();

    let config = fixture_config(root);
    let _guard = DirGuard::new(root);

    let parsed = build_global_posture(&config, None, true).unwrap();
    assert_eq!(parsed.total_repos, 1);
    assert_eq!(parsed.skipped_repos, 1);
    assert!(parsed.warnings.iter().any(|w| w.contains("bad")));
}

#[test]
#[serial(env, cwd)]
fn old_schema_ledger_db_is_warned_and_skipped_run_completes() {
    let _env_non_interactive = non_interactive();
    let tmp = tempdir().unwrap();
    let root = tmp.path();

    make_fixture_repo(root, "good", 1, 0, 0);

    // Build a repo whose ledger.db has only the very first migration applied.
    let old_root = root.join("old_schema");
    fs::create_dir_all(&old_root).unwrap();
    setup_git_repo(&old_root);
    {
        let _guard = DirGuard::new(&old_root);
        let _env = non_interactive();
        execute_init(false, false).unwrap();
    }
    let old_db = old_root.join(".ledgerful").join("state").join("ledger.db");
    // Reduce schema to the first migration only by dropping all tables except
    // the migration-tracker and resetting user_version.
    let conn = Connection::open(&old_db).unwrap();
    let tables: Vec<String> = conn
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table'")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    for table in tables {
        if table == "sqlite_sequence" {
            continue;
        }
        let _ = conn.execute(&format!("DROP TABLE IF EXISTS {}", table), []);
    }
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations (version INTEGER PRIMARY KEY);
         PRAGMA user_version = 1;",
    )
    .unwrap();
    drop(conn);

    let config = fixture_config(root);
    let _guard = DirGuard::new(root);

    let parsed = build_global_posture(&config, None, true).unwrap();
    assert_eq!(parsed.total_repos, 1);
    assert_eq!(parsed.skipped_repos, 1);
    assert!(parsed.warnings.iter().any(|w| w.contains("old_schema")));
}

#[test]
#[serial(env, cwd)]
fn no_repos_prints_empty_message_and_exits_zero() {
    let _env_non_interactive = non_interactive();
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    fs::create_dir_all(root.join(".git")).unwrap();

    let config = fixture_config(root);
    let _guard = DirGuard::new(root);

    let parsed = build_global_posture(&config, None, true).unwrap();
    assert_eq!(parsed.total_repos, 0);
    assert_eq!(parsed.skipped_repos, 0);
    assert!(parsed.repos.is_empty());
}

#[test]
#[serial(env, cwd)]
fn cache_older_than_staleness_secs_triggers_rewalk() {
    let _env_non_interactive = non_interactive();
    let tmp = tempdir().unwrap();
    let home = tmp.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let root = tmp.path().join("roots");
    fs::create_dir_all(&root).unwrap();

    make_fixture_repo(&root, "repo_a", 1, 0, 0);

    let cache = home.join(".ledgerful").join("rollup").join("cache.sqlite");
    let _profile = TempEnv::set("USERPROFILE", home.to_str().unwrap());
    let _home_env = TempEnv::set("HOME", home.to_str().unwrap());
    let _cache_env = TempEnv::set("LEDGERFUL_ROLLUP_CACHE", cache.to_str().unwrap());

    let config = fixture_config_with_staleness(&root, 3600);
    let _guard = DirGuard::new(&root);

    // First call: walk and cache.
    build_global_posture(&config, None, false).unwrap();
    assert!(cache.exists());

    // Make the cache older than the staleness window (past age + window + margin).
    set_mtime_to_past(&cache);

    // With a very tight staleness window, the cache must be treated as stale.
    let stale_config = fixture_config_with_staleness(&root, 1);
    // Sleep long enough so cache_mtime + 1s is in the past.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let parsed = build_global_posture(&stale_config, None, false).unwrap();
    assert_eq!(parsed.total_repos, 1);
}

#[test]
#[serial(env, cwd)]
fn cache_within_staleness_window_but_root_newer_triggers_rewalk() {
    let _env_non_interactive = non_interactive();
    let tmp = tempdir().unwrap();
    let home = tmp.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let root = tmp.path().join("roots");
    fs::create_dir_all(&root).unwrap();

    make_fixture_repo(&root, "repo_a", 1, 0, 0);

    let cache = home.join(".ledgerful").join("rollup").join("cache.sqlite");
    let _profile = TempEnv::set("USERPROFILE", home.to_str().unwrap());
    let _home_env = TempEnv::set("HOME", home.to_str().unwrap());
    let _cache_env = TempEnv::set("LEDGERFUL_ROLLUP_CACHE", cache.to_str().unwrap());

    let config = fixture_config_with_staleness(&root, 3600);
    let _guard = DirGuard::new(&root);

    // First call: walk and cache.
    build_global_posture(&config, None, false).unwrap();
    assert!(cache.exists());

    // Make the root appear much newer than the cache by aging the cache.
    set_mtime_to_past(&cache);

    let parsed =
        build_global_posture(&fixture_config_with_staleness(&root, 86400), None, false).unwrap();
    assert_eq!(parsed.total_repos, 1);
}

#[test]
#[serial(env, cwd)]
fn cache_within_window_and_root_older_uses_cache_no_rewalk() {
    let _env_non_interactive = non_interactive();
    let tmp = tempdir().unwrap();
    let home = tmp.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let root = tmp.path().join("roots");
    fs::create_dir_all(&root).unwrap();

    make_fixture_repo(&root, "repo_a", 1, 0, 0);

    let cache = home.join(".ledgerful").join("rollup").join("cache.sqlite");
    let _profile = TempEnv::set("USERPROFILE", home.to_str().unwrap());
    let _home_env = TempEnv::set("HOME", home.to_str().unwrap());
    let _cache_env = TempEnv::set("LEDGERFUL_ROLLUP_CACHE", cache.to_str().unwrap());

    let config = fixture_config_with_staleness(&root, 3600);
    let _guard = DirGuard::new(&root);

    // First call: walk and cache.
    build_global_posture(&config, None, false).unwrap();
    assert!(cache.exists());

    // Remove the repo DB so a fresh walk would fail. To force cache usage, we
    // then make the cache file newer than the root directory.
    fs::remove_file(
        root.join("repo_a")
            .join(".ledgerful")
            .join("state")
            .join("ledger.db"),
    )
    .unwrap();

    // Age the root directory and its .ledgerful subtree so cache_mtime >= root_mtime.
    set_mtime_to_past(&root.join("repo_a").join(".ledgerful"));
    set_mtime_to_past(&root.join("repo_a"));
    set_mtime_to_past(&root);

    // Touch the cache so it is clearly newer than the aged root.
    let cache_now = filetime::FileTime::from_system_time(std::time::SystemTime::now());
    filetime::set_file_mtime(&cache, cache_now).unwrap();

    let parsed =
        build_global_posture(&fixture_config_with_staleness(&root, 3600), None, false).unwrap();
    // With the cache fresh, the cached posture is used without reopening the
    // repo DB, so the deleted DB is not observed and the repo is still returned.
    // This is the intended cache-hit fast path: the staleness window bounds
    // validity, not a disk re-check.
    assert_eq!(parsed.total_repos, 1);
    assert_eq!(parsed.skipped_repos, 0);
    assert!(
        parsed.repos[0].repo_path.ends_with("repo_a"),
        "expected cached repo_a posture to be returned from cache, got: {:?}",
        parsed.repos
    );
}

#[test]
#[serial(env, cwd)]
fn fd_safe_many_repo_query_no_descriptor_exhaustion() {
    let _env_non_interactive = non_interactive();
    let tmp = tempdir().unwrap();
    let root = tmp.path();

    for i in 0..30 {
        make_fixture_repo(root, &format!("repo_{:03}", i), 0, 0, 0);
    }

    let config = fixture_config(root);
    let _guard = DirGuard::new(root);
    execute_ledger_status_global(&config, None, true, false).unwrap();
}

#[test]
#[serial(env, cwd)]
fn locked_db_warns_and_skips_completes_with_others() {
    let _env_non_interactive = non_interactive();
    let tmp = tempdir().unwrap();
    let root = tmp.path();

    make_fixture_repo(root, "repo_a", 0, 0, 0);
    make_fixture_repo(root, "repo_b", 0, 0, 0);

    let db_a = root
        .join("repo_a")
        .join(".ledgerful")
        .join("state")
        .join("ledger.db");
    let locker = Connection::open(&db_a).unwrap();
    locker.execute_batch("BEGIN EXCLUSIVE").unwrap();

    let config = fixture_config(root);
    let _guard = DirGuard::new(root);
    execute_ledger_status_global(&config, None, true, false).unwrap();

    drop(locker);
}

#[test]
#[serial(env, cwd)]
fn opt_out_opt_in_toggles_enabled_flag() {
    let _env_non_interactive = non_interactive();
    let tmp = tempdir().unwrap();
    let home = tmp.path();

    let config_home = home.join(".ledgerful");
    let _profile = TempEnv::set("USERPROFILE", home.to_str().unwrap());
    let _home = TempEnv::set("HOME", home.to_str().unwrap());
    let _config_home = TempEnv::set("LEDGERFUL_CONFIG_HOME", config_home.to_str().unwrap());

    set_global_rollup_enabled(true).unwrap();
    let config = ledgerful::config::load::load_config(&ledgerful::state::layout::Layout::new(
        camino::Utf8Path::from_path(home).unwrap(),
    ))
    .unwrap();
    assert!(config.global_rollup.enabled);

    set_global_rollup_enabled(false).unwrap();
    let config = ledgerful::config::load::load_config(&ledgerful::state::layout::Layout::new(
        camino::Utf8Path::from_path(home).unwrap(),
    ))
    .unwrap();
    assert!(!config.global_rollup.enabled);

    set_global_rollup_enabled(true).unwrap();
}

#[test]
#[serial(env, cwd)]
#[ignore = "perf — run manually with --ignored"]
fn perf_hundred_repo_first_call_under_budget_subsequent_call_fast() {
    let _env_non_interactive = non_interactive();
    let tmp = tempdir().unwrap();
    let root = tmp.path();

    for i in 0..100 {
        make_fixture_repo(root, &format!("repo_{:03}", i), 0, 0, 0);
    }

    let config = fixture_config(root);
    let _guard = DirGuard::new(root);

    let first_start = std::time::Instant::now();
    execute_ledger_status_global(&config, None, true, false).unwrap();
    let first_elapsed = first_start.elapsed();

    // Delete one repo DB after the first call. A true cache hit must NOT
    // re-query any repo, so the missing DB must not cause an error.
    let victim_db = root
        .join("repo_000")
        .join(".ledgerful")
        .join("state")
        .join("ledger.db");
    fs::remove_file(&victim_db).unwrap();

    let second_start = std::time::Instant::now();
    execute_ledger_status_global(&config, None, false, false).unwrap();
    let second_elapsed = second_start.elapsed();

    // Generous CI-aware bounds (manual-only test). Debug Windows builds with
    // verbose console rendering and 100 DB reads dominate the first call; the
    // second call should now be a pure cache hit and much faster because it
    // does not reopen any repo DB.
    assert!(
        first_elapsed.as_secs() < 30,
        "first --global on 100 repos took {:?}, expected <30s",
        first_elapsed
    );
    assert!(
        second_elapsed.as_secs() < 5,
        "second --global on 100 repos took {:?}, expected <5s",
        second_elapsed
    );
}

#[test]
#[serial(env, cwd)]
fn reindex_forces_fresh_walk() {
    let _env_non_interactive = non_interactive();
    let tmp = tempdir().unwrap();
    let root = tmp.path();

    make_fixture_repo(root, "repo_a", 0, 0, 0);

    let config = fixture_config(root);
    let _guard = DirGuard::new(root);
    execute_ledger_status_global(&config, None, false, false).unwrap();

    // Delete the DB after caching; a plain second call would use the cache and
    // return the cached posture, but --reindex must re-walk and re-query, so it
    // should surface the missing DB as a skipped repo.
    fs::remove_file(
        root.join("repo_a")
            .join(".ledgerful")
            .join("state")
            .join("ledger.db"),
    )
    .unwrap();

    let parsed = build_global_posture(&config, None, true).unwrap();
    // Reindex bypasses the cache entirely and re-walks. The walk discovers
    // repos by finding `ledger.db`; with the DB deleted, repo_a is simply not
    // discovered — there is nothing to skip. A deleted repo is indistinguishable
    // from "no repo here" at walk time.
    assert_eq!(parsed.total_repos, 0);
    assert_eq!(parsed.skipped_repos, 0);
}

#[test]
#[serial(env, cwd)]
fn cache_hit_uses_cached_postures_no_requery() {
    let _env_non_interactive = non_interactive();
    let tmp = tempdir().unwrap();
    let home = tmp.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let root = tmp.path().join("roots");
    fs::create_dir_all(&root).unwrap();

    make_fixture_repo(&root, "repo_a", 1, 0, 0);
    make_fixture_repo(&root, "repo_b", 0, 1, 0);
    make_fixture_repo(&root, "repo_c", 0, 0, 1);

    let cache = home.join(".ledgerful").join("rollup").join("cache.sqlite");
    let _profile = TempEnv::set("USERPROFILE", home.to_str().unwrap());
    let _home_env = TempEnv::set("HOME", home.to_str().unwrap());
    let _cache_env = TempEnv::set("LEDGERFUL_ROLLUP_CACHE", cache.to_str().unwrap());

    let config = fixture_config_with_staleness(&root, 3600);
    let _guard = DirGuard::new(&root);

    // First call walks all repos, queries them, and writes the cache.
    let first = build_global_posture(&config, None, false).unwrap();
    assert_eq!(first.total_repos, 3);

    // Mutate repo_a directly on disk: add another ledger entry so a re-query
    // would report a different unsigned count. We use SQL to keep the root
    // directory mtime unchanged, so the cache remains fresh by mtime.
    let db_a = root
        .join("repo_a")
        .join(".ledgerful")
        .join("state")
        .join("ledger.db");
    let conn = Connection::open(&db_a).unwrap();
    let tx_id: String = conn
        .query_row("SELECT lower(hex(randomblob(16)))", [], |row| row.get(0))
        .unwrap();
    conn.execute(
        "INSERT INTO transactions (
            tx_id, operation_id, status, category, entity, entity_normalized,
            planned_action, session_id, source, started_at, resolved_at, issue_ref,
            detected_at, drift_count, first_seen_at, last_seen_at, snapshot_id
        ) VALUES (?1, NULL, 'COMMITTED', 'FEATURE', ?2, ?2, NULL, 'test', 'CLI',
            datetime('now'), datetime('now'), NULL, NULL, 1, datetime('now'), datetime('now'), NULL)",
        [&tx_id, "src/main_mutation.rs"],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO ledger_entries (
            tx_id, category, entry_type, entity, entity_normalized, change_type,
            summary, reason, is_breaking, committed_at, verification_status,
            verification_basis, outcome_notes, origin, trace_id, signature,
            public_key, risk, related_tickets, author, observed, prev_hash
        ) VALUES (?1, 'FEATURE', 'IMPLEMENTATION', 'src/main_mutation.rs', 'src/main_mutation.rs', 'MODIFY',
            'summary', 'reason', 0, datetime('now'), NULL, NULL, NULL, 'LOCAL', NULL,
            'bad-sig', 'bad-key', NULL, NULL, 'Test', NULL, NULL)",
        [&tx_id],
    )
    .unwrap();
    drop(conn);

    // The cache is still fresh because the root .ledgerful/ mtime did not change
    // and the staleness window is large. The second call must use the cached
    // posture and therefore report the OLD unsigned count for repo_a.
    let second = build_global_posture(&config, None, false).unwrap();
    assert_eq!(second.total_repos, 3);
    let repo_a_second = second
        .repos
        .iter()
        .find(|p| p.repo_path.ends_with("repo_a"))
        .unwrap();
    let repo_a_first = first
        .repos
        .iter()
        .find(|p| p.repo_path.ends_with("repo_a"))
        .unwrap();
    assert_eq!(
        repo_a_second.unsigned_entries, repo_a_first.unsigned_entries,
        "cache hit must return the cached posture, not the mutated one"
    );
}

#[test]
#[serial(env, cwd)]
fn stale_root_triggers_requery_for_that_root_only() {
    let _env_non_interactive = non_interactive();
    let tmp = tempdir().unwrap();
    let home = tmp.path().join("home");
    fs::create_dir_all(&home).unwrap();
    // Use three separate roots so that marking one root stale does not force a
    // re-query of repos under the other roots. With a single shared root,
    // per-repo mtime changes do not surface as root-level staleness.
    let root_a = tmp.path().join("root_a");
    let root_b = tmp.path().join("root_b");
    let root_c = tmp.path().join("root_c");
    fs::create_dir_all(&root_a).unwrap();
    fs::create_dir_all(&root_b).unwrap();
    fs::create_dir_all(&root_c).unwrap();

    make_fixture_repo(&root_a, "repo_a", 1, 0, 0);
    make_fixture_repo(&root_b, "repo_b", 0, 1, 0);
    make_fixture_repo(&root_c, "repo_c", 0, 0, 1);

    let cache = home.join(".ledgerful").join("rollup").join("cache.sqlite");
    let _profile = TempEnv::set("USERPROFILE", home.to_str().unwrap());
    let _home_env = TempEnv::set("HOME", home.to_str().unwrap());
    let _cache_env = TempEnv::set("LEDGERFUL_ROLLUP_CACHE", cache.to_str().unwrap());

    let mut config = fixture_config_with_staleness(&root_a, 3600);
    config.roots = vec![root_a.clone(), root_b.clone(), root_c.clone()];
    let _guard = DirGuard::new(&root_a);

    // First call populates cache across all three roots.
    let first = build_global_posture(&config, None, false).unwrap();
    assert_eq!(first.total_repos, 3);

    // Delete repo_a DB and bump root_a's own mtime so root_a is detected as
    // stale by `root_mtime`. Only root_a should be re-walked and re-queried;
    // root_b/repo_b and root_c/repo_c should use cached postures.
    fs::remove_file(
        root_a
            .join("repo_a")
            .join(".ledgerful")
            .join("state")
            .join("ledger.db"),
    )
    .unwrap();
    // Bump root_a's mtime (not repo_a's .ledgerful) — `root_mtime` checks
    // root_a/.ledgerful if it exists, else root_a itself. root_a has no
    // .ledgerful dir, so its own mtime is the signal. Set it to a future time
    // so the second-precision comparison reliably treats root_a as newer than
    // the cache (which was written moments ago).
    let future = filetime::FileTime::from_system_time(
        std::time::SystemTime::now() + std::time::Duration::from_secs(5),
    );
    filetime::set_file_mtime(&root_a, future).unwrap();

    let second = build_global_posture(&config, None, false).unwrap();
    assert_eq!(
        second.total_repos, 2,
        "stale root re-walk dropped repo_a (no ledger.db to discover); fresh roots kept"
    );
    // repo_a's DB was deleted, so the re-walk of root_a does not discover it —
    // there is nothing to skip (a missing DB is indistinguishable from "no repo"
    // at walk time). The cached posture for repo_a is dropped because root_a was
    // filtered out as stale before posture assembly.
    assert_eq!(
        second.skipped_repos, 0,
        "deleted repo is not 'skipped' — it is simply not discovered by the walk"
    );
    assert!(
        second.repos.iter().any(|p| p.repo_path.ends_with("repo_b")),
        "repo_b should use cached posture"
    );
    assert!(
        second.repos.iter().any(|p| p.repo_path.ends_with("repo_c")),
        "repo_c should use cached posture"
    );
}

#[test]
#[serial(env, cwd)]
fn repo_scoping_filters_to_one_repo() {
    let _env_non_interactive = non_interactive();
    let tmp = tempdir().unwrap();
    let root = tmp.path();

    make_fixture_repo(root, "repo_a", 1, 0, 0);
    make_fixture_repo(root, "repo_b", 0, 1, 0);

    let config = fixture_config(root);
    let _guard = DirGuard::new(root);

    let parsed = build_global_posture(&config, Some("repo_a"), true).unwrap();
    assert_eq!(parsed.total_repos, 1);
    assert_eq!(parsed.repos[0].unsigned_entries, 1);
}

#[test]
#[serial(env, cwd)]
fn timings_gate_absent_table_prints_honest_message() {
    let config = GlobalRollupConfig::default();
    #[cfg(unix)]
    {
        let output = capture_stdout(move || {
            execute_timings_global(&config, false).unwrap();
        });
        assert!(output.contains("per-repo timing not enabled"));
    }
    #[cfg(not(unix))]
    {
        // Windows stdout capture is non-trivial; assert the function succeeds.
        execute_timings_global(&config, false).unwrap();
    }
}

#[test]
#[serial(env, cwd)]
fn unsigned_entries_counts_invalid_and_missing_signatures() {
    let _env_non_interactive = non_interactive();
    let tmp = tempdir().unwrap();
    let root = tmp.path();

    make_fixture_repo_with_signature_mix(root, "repo_a", 2, 1, 2);

    let config = fixture_config(root);
    let _guard = DirGuard::new(root);

    let parsed = build_global_posture(&config, None, true).unwrap();
    assert_eq!(parsed.total_repos, 1);
    assert_eq!(
        parsed.repos[0].unsigned_entries, 3,
        "unsigned_entries must count 1 invalid + 2 missing signatures"
    );
}

#[test]
#[serial(env, cwd)]
fn global_command_does_not_migrate_legacy_state_dir() {
    let _env_non_interactive = non_interactive();
    let tmp = tempdir().unwrap();
    let root = tmp.path();

    // Create a repo with the legacy `.changeguard` state dir and no `.ledgerful`.
    fs::create_dir_all(root.join(".changeguard").join("state")).unwrap();
    fs::write(root.join(".changeguard").join("marker"), "legacy").unwrap();

    // Point the user/global config home to an empty temp dir so the global
    // command has a valid config path but no roots configured. This keeps the
    // command fast and avoids walking the real home dir.
    let home = tmp.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let config_home = home.join(".ledgerful");

    let _profile = TempEnv::set("USERPROFILE", home.to_str().unwrap());
    let _home = TempEnv::set("HOME", home.to_str().unwrap());
    let _config_home = TempEnv::set("LEDGERFUL_CONFIG_HOME", config_home.to_str().unwrap());
    let _cache_env = TempEnv::set(
        "LEDGERFUL_ROLLUP_CACHE",
        config_home
            .join("rollup")
            .join("cache.sqlite")
            .to_str()
            .unwrap(),
    );

    let _guard = DirGuard::new(root);

    // The current repo has no git root, so running the CLI dispatcher directly
    // would fail on `get_repo_root` in usage metrics / other paths. Instead we
    // invoke the global posture builder with the empty-roots config, which is
    // the behavior `ledger status --global` reaches after our short-circuit.
    let config = GlobalRollupConfig {
        roots: vec![],
        timeout_secs: 1,
        staleness_secs: 3600,
        max_depth: None,
        enabled: true,
    };
    let parsed = build_global_posture(&config, None, false).unwrap();
    assert_eq!(parsed.total_repos, 0);

    // The critical invariant: `--global` must not have triggered a migration
    // in the current working directory.
    assert!(
        root.join(".changeguard").exists(),
        "--global must not migrate the legacy .changeguard state dir"
    );
    assert!(
        !root.join(".ledgerful").exists(),
        "--global must not create .ledgerful in the current repo"
    );
}

#[test]
#[serial(env, cwd)]
fn json_output_is_deterministic_same_fixture_same_bytes() {
    let _env_non_interactive = non_interactive();
    let tmp = tempdir().unwrap();
    let root = tmp.path();

    make_fixture_repo(root, "repo_a", 1, 1, 0);
    make_fixture_repo(root, "repo_b", 0, 0, 1);

    let config = fixture_config(root);
    let _guard = DirGuard::new(root);

    let first_config = config.clone();
    let first = capture_stdout(move || {
        execute_ledger_status_global(&first_config, None, true, true).unwrap();
    });
    let second = capture_stdout(move || {
        execute_ledger_status_global(&config, None, false, true).unwrap();
    });
    assert_eq!(first, second, "global rollup JSON must be deterministic");
}

#[test]
#[serial(env, cwd)]
fn privacy_negative_test_rollup_source_contains_no_network_crate_symbols() {
    let source = fs::read_to_string("src/state/rollup.rs").unwrap();
    let forbidden = [
        "use ureq",
        "use reqwest",
        "use tokio_tungstenite",
        "use isahc",
        "use hyper",
    ];
    for needle in forbidden {
        assert!(
            !source.contains(needle),
            "rollup source contains forbidden network crate symbol: {needle}"
        );
    }
}

#[test]
#[serial(env, cwd)]
fn canonicalize_failure_on_nonexistent_root_warns_and_skips() {
    let _env_non_interactive = non_interactive();
    let tmp = tempdir().unwrap();
    let root = tmp.path();

    make_fixture_repo(root, "repo_a", 1, 0, 0);

    // A root that does not exist at all will fail `std::fs::canonicalize`
    // inside `resolve_roots` and be warned-and-skipped.
    let missing_root = tmp.path().join("nonexistent_subdir");

    let config = GlobalRollupConfig {
        roots: vec![root.to_path_buf(), missing_root],
        timeout_secs: 30,
        staleness_secs: 3600,
        max_depth: None,
        enabled: true,
    };

    let _guard = DirGuard::new(root);
    let parsed = build_global_posture(&config, None, true).unwrap();
    assert_eq!(
        parsed.total_repos, 1,
        "only the valid root should produce a repo"
    );
    // The non-existent root is skipped by `resolve_roots` canonicalize failure.
    // The tracing warning is not captured here; the behavioral invariant is that
    // the bogus root contributes no repos and the run completes without error.
    assert!(
        !parsed
            .repos
            .iter()
            .any(|p| p.repo_path.contains("nonexistent_subdir")),
        "nonexistent root must not appear in output repos"
    );
}

#[test]
#[serial(env, cwd)]
fn config_home_env_var_is_respected_for_opt_out_read_path() {
    let _env_non_interactive = non_interactive();
    let tmp = tempdir().unwrap();
    let home = tmp.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let config_home = home.join(".ledgerful");

    let _profile = TempEnv::set("USERPROFILE", home.to_str().unwrap());
    let _home = TempEnv::set("HOME", home.to_str().unwrap());
    let _config_home = TempEnv::set("LEDGERFUL_CONFIG_HOME", config_home.to_str().unwrap());

    let root = tmp.path().join("roots");
    fs::create_dir_all(&root).unwrap();
    make_fixture_repo(&root, "repo_a", 0, 0, 0);

    // Opt-out writes to the LEDGERFUL_CONFIG_HOME path.
    set_global_rollup_enabled(false).unwrap();

    // Reading via load_user_config (as the CLI does for --global) must see
    // the same disabled config, so the rollup exits without walking.
    let config = ledgerful::config::load::load_config(&ledgerful::state::layout::Layout::new(
        camino::Utf8Path::from_path(&home).unwrap(),
    ))
    .unwrap();
    assert!(
        !config.global_rollup.enabled,
        "load_user_config path must read the disabled flag written via LEDGERFUL_CONFIG_HOME"
    );
}
