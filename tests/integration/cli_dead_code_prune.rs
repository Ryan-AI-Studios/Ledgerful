use ledgerful::commands::dead_code::{ConfirmPrompt, execute_dead_code_with_prompt};
use ledgerful::commands::init::execute_init;
use ledgerful::ledger::db::LedgerDb;
use ledgerful::ledger::types::Category;
use ledgerful::state::layout::Layout;
use ledgerful::state::storage::StorageManager;
use miette::Result;
use std::path::Path;

use crate::common::{DirGuard, git_add_and_commit, setup_git_repo};

struct AlwaysYes;
impl ConfirmPrompt for AlwaysYes {
    fn ask(&self, _message: &str, _default: bool) -> Result<bool> {
        Ok(true)
    }
}

#[test]
fn test_prune_removes_lines_and_records_pending_transaction() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();

    setup_git_repo(root);
    std::fs::create_dir(root.join("src")).unwrap();
    let file = root.join("src/lib.rs");
    std::fs::write(&file, "pub fn dead() -> i32 { 2 }\n").unwrap();
    git_add_and_commit(root, "initial");

    let _guard = DirGuard::new(root);
    execute_init(false).unwrap();

    let root_utf8 = camino::Utf8Path::from_path(root).unwrap();
    seed_dead_code_fixture(root, 2);

    // Write a config that zeroes the git_activity weight so the freshly
    // committed fixture file's zero git-inactivity does not drag the
    // blended confidence below the 0.75 threshold. Reachability (1.0,
    // the symbol is unreachable) and test_coverage (1.0, no tests map
    // to it) then blend to 1.0.
    write_dead_code_config(root_utf8);

    let result =
        execute_dead_code_with_prompt(0.75, 50, false, false, true, false, None, &AlwaysYes);
    assert!(result.is_ok(), "prune command failed: {result:?}");

    let content = std::fs::read_to_string(&file).unwrap();
    assert!(
        !content.contains("pub fn dead()"),
        "expected dead symbol to be removed, got: {content}"
    );
    // The safe default leaves one trailing newline in an otherwise empty file.
    assert_eq!(
        content, "\n",
        "expected file to be left with a single newline, got: {content}"
    );

    // Verify a PENDING ledger transaction exists with the right category and symbol provenance.
    let layout = Layout::new(root_utf8);
    let storage = StorageManager::open_read_only_sqlite_only(&layout.root).unwrap();
    let db = LedgerDb::new(storage.get_connection());
    let pending = db.get_all_pending().unwrap();
    assert_eq!(
        pending.len(),
        1,
        "expected exactly one pending transaction, got {:?}",
        pending
    );
    let tx = &pending[0];
    assert_eq!(tx.category, Category::Refactor);

    let provenance = db.get_token_provenance_for_tx(&tx.tx_id).unwrap();
    assert!(
        provenance
            .iter()
            .any(|p| p.symbol_name == "dead" && p.action.to_string() == "DELETED"),
        "expected DELETED token provenance for 'dead', got {provenance:?}"
    );
}

/// Seeds an isolated unreachable `dead` symbol in `src/lib.rs` and a single
/// ENTRYPOINT `main` in `src/main.rs`. The `dead` symbol has no incoming
/// structural edges, so reachability_score = 1.0; no tests map to it, so
/// test_coverage_score = 1.0. With the git_activity_weight zeroed in the
/// config (see `write_dead_code_config`), the blended confidence is 1.0.
fn seed_dead_code_fixture(root: &Path, _symbol_count: usize) {
    let root_utf8 = camino::Utf8Path::from_path(root).unwrap();
    let layout = ledgerful::state::layout::Layout::new(root_utf8);
    layout.ensure_state_dir().unwrap();

    let storage = ledgerful::state::storage::StorageManager::init(
        layout.state_subdir().join("ledger.db").as_std_path(),
    )
    .unwrap();
    let conn = storage.get_connection();

    // src/main.rs holds the ENTRYPOINT symbol `main`.
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

    // src/lib.rs holds the unreachable INTERNAL symbol `dead` on line 1.
    // Use INSERT OR IGNORE so a row pre-seeded by execute_init is reused
    // rather than duplicated.
    conn.execute(
        "INSERT OR IGNORE INTO project_files (id, file_path, last_indexed_at) \
         VALUES (1, 'src/lib.rs', '2026-01-01T00:00:00Z')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT OR IGNORE INTO project_symbols (id, file_id, qualified_name, symbol_name, symbol_kind, entrypoint_kind, line_start, line_end, last_indexed_at) \
         VALUES (1, 1, 'dead', 'dead', 'Function', 'INTERNAL', 1, 1, '2026-01-01T00:00:00Z')",
        [],
    )
    .unwrap();

    // No structural edges touch symbol id=1, so it is unreachable from the
    // entrypoint. This is the deterministic prune target.

    storage.shutdown().unwrap();
}

/// Write a `.ledgerful/config.toml` that zeroes the git_activity weight so
/// the freshly-committed fixture file (zero git inactivity) does not drag the
/// blended confidence below the 0.75 threshold. Reachability (1.0) and
/// test_coverage (1.0) then blend to 1.0.
fn write_dead_code_config(root: &camino::Utf8Path) {
    let config_dir = root.join(".ledgerful");
    std::fs::create_dir_all(config_dir.as_std_path()).unwrap();
    let config_path = config_dir.join("config.toml");
    std::fs::write(
        config_path.as_std_path(),
        "[dead_code]\nenabled = true\nconfidence_threshold = 0.75\ngit_inactivity_days = 90\nreachability_weight = 1.0\ngit_activity_weight = 0.0\ntest_coverage_weight = 1.0\n",
    )
    .unwrap();
}
