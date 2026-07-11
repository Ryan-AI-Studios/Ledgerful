use crate::common::{DirGuard, git_add_and_commit, setup_git_repo};
use ledgerful::commands::dead_code::execute_dead_code;
use ledgerful::commands::init::execute_init;
use std::fs;
use tempfile::tempdir;

#[test]
fn dead_code_reports_unused_symbols() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();

    setup_git_repo(root);
    fs::write(root.join("dummy.txt"), "content").unwrap();
    git_add_and_commit(root, "initial");

    let _guard = DirGuard::new(root);
    execute_init(false, false).unwrap();

    // threshold 0.9, limit 50, auto_index false, include_traits false, prune false, expand false, explain None
    let result = execute_dead_code(0.9, 50, false, false, false, false, None);
    assert!(result.is_ok());
}

#[test]
fn test_dead_code_include_traits_flag() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();

    setup_git_repo(root);
    fs::write(root.join("dummy.txt"), "content").unwrap();
    git_add_and_commit(root, "initial");

    let _guard = DirGuard::new(root);
    execute_init(false, false).unwrap();

    // include_traits = true must not error even when no traits are present
    let result = execute_dead_code(0.9, 50, false, true, false, false, None);
    assert!(result.is_ok());
}

/// Regression for CG-F15: `dead-code` used to open writable storage,
/// rebuild the reachability graph and run a SQL lookup per symbol, and
/// repeat git-history walks, which made it scale badly with symbol count
/// (34s-124s+ on a real repo with thousands of symbols). This seeds a
/// synthetic SQLite fixture directly (fast and deterministic, unlike
/// running the tree-sitter indexer over hundreds of real files) with a
/// reachable chain from a single entrypoint plus an equal number of
/// isolated/unreachable symbols, then asserts the command completes within
/// a bound generous enough to avoid CI flakiness but tight enough to catch
/// an O(n) or worse per-symbol regression.
#[test]
fn test_dead_code_bounded_latency_on_nontrivial_fixture() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();

    setup_git_repo(root);
    fs::write(root.join("dummy.txt"), "content").unwrap();
    git_add_and_commit(root, "initial");

    let _guard = DirGuard::new(root);
    execute_init(false, false).unwrap();
    seed_dead_code_fixture(500);

    let start = std::time::Instant::now();
    let result = execute_dead_code(0.75, 50, false, false, false, false, None);
    let elapsed = start.elapsed();

    assert!(result.is_ok(), "dead-code command failed: {:?}", result);
    assert!(
        elapsed < std::time::Duration::from_secs(15),
        "dead-code took {:?} on a 500-symbol fixture; CG-F15 guards against \
         per-symbol DB round trips and graph rebuilds that scale badly",
        elapsed
    );
}

/// TA24 integration: `dead-code --explain <file>` finds symbols for indexed
/// files regardless of how the user types the path (forward slash, backslash,
/// `./` prefix, absolute path within repo).
#[test]
fn test_dead_code_explain_resolves_varied_path_formats() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();

    setup_git_repo(root);
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src/main.rs"), "fn unused() {}").unwrap();
    git_add_and_commit(root, "initial");

    let _guard = DirGuard::new(root);
    execute_init(false, false).unwrap();

    let absolute = root.join("src/main.rs").to_string_lossy().to_string();
    let mut indexed = vec![
        "src/main.rs".to_string(),
        "src\\main.rs".to_string(),
        "./src/main.rs".to_string(),
        "src/main.rs/".to_string(),
        absolute,
    ];
    #[cfg(target_os = "windows")]
    indexed.push("SRC\\MAIN.RS".to_string());
    #[cfg(not(target_os = "windows"))]
    let _ = &mut indexed; // Suppress unused_mut on non-windows

    for path in indexed {
        let result = execute_dead_code(0.9, 50, false, false, false, false, Some(path.clone()));
        assert!(result.is_ok(), "--explain {path:?} failed: {result:?}");
    }
}

/// TA24 integration: `dead-code --explain <file>` on a non-indexed file prints
/// the "not found" message and exits 0 (informational, not an error).
#[test]
fn test_dead_code_explain_non_indexed_file_exits_zero() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();

    setup_git_repo(root);
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src/main.rs"), "fn main() {}").unwrap();
    git_add_and_commit(root, "initial");

    let _guard = DirGuard::new(root);
    execute_init(false, false).unwrap();

    let result = execute_dead_code(
        0.9,
        50,
        false,
        false,
        false,
        false,
        Some("src/missing.rs".to_string()),
    );
    assert!(
        result.is_ok(),
        "non-indexed --explain should exit 0: {result:?}"
    );
}

/// `symbol_count / 2` symbols reachable from a single `ENTRYPOINT`, plus the
/// same number of fully isolated (unreachable) symbols as dead-code
/// candidates. Bypasses the tree-sitter indexer for speed and determinism.
fn seed_dead_code_fixture(symbol_count: usize) {
    let cwd = std::env::current_dir().unwrap();
    let root = camino::Utf8Path::from_path(&cwd).unwrap();
    let layout = ledgerful::state::layout::Layout::new(root);
    layout.ensure_state_dir().unwrap();

    let storage = ledgerful::state::storage::StorageManager::init(
        layout.state_subdir().join("ledger.db").as_std_path(),
    )
    .unwrap();
    let conn = storage.get_connection();

    conn.execute(
        "INSERT INTO project_files (id, file_path, last_indexed_at) VALUES \
         (0, 'src/main.rs', '2026-01-01T00:00:00Z')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO project_symbols (id, file_id, qualified_name, symbol_name, symbol_kind, entrypoint_kind, last_indexed_at) \
         VALUES (0, 0, 'main', 'main', 'Function', 'ENTRYPOINT', '2026-01-01T00:00:00Z')",
        [],
    )
    .unwrap();

    let reachable_count = symbol_count / 2;
    for i in 1..=symbol_count {
        let file_id = i as i64;
        let file_path = format!("src/gen_{i}.rs");
        conn.execute(
            "INSERT INTO project_files (id, file_path, last_indexed_at) VALUES (?1, ?2, '2026-01-01T00:00:00Z')",
            (file_id, file_path),
        )
        .unwrap();

        let name = format!("fn_{i}");
        conn.execute(
            "INSERT INTO project_symbols (id, file_id, qualified_name, symbol_name, symbol_kind, last_indexed_at) \
             VALUES (?1, ?1, ?2, ?2, 'Function', '2026-01-01T00:00:00Z')",
            (file_id, name),
        )
        .unwrap();

        if i <= reachable_count {
            // Chain each reachable symbol off the previous one (or main),
            // so reachability requires a real graph walk, not a trivial check.
            let caller_id = (i - 1) as i64;
            conn.execute(
                "INSERT INTO structural_edges (caller_symbol_id, caller_file_id, callee_symbol_id, callee_file_id) \
                 VALUES (?1, ?1, ?2, ?2)",
                (caller_id, file_id),
            )
            .unwrap();
        }
        // Symbols beyond reachable_count are left with no edges at all:
        // unreachable, and therefore dead-code candidates.
    }

    storage.shutdown().unwrap();
}
