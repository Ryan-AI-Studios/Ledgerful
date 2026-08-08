use ledgerful::commands::verify::{TestMappingState, explain_test_mappings};
use ledgerful::state::storage::StorageManager;
use tempfile::tempdir;

/// Regression coverage for CG-F17: `verify --explain --entity` used to query
/// `tm.test_name` / `tm.source_file_id`, columns that don't exist on the real
/// `test_mapping` schema (`test_symbol_id`/`test_file_id`/`tested_symbol_id`/
/// `tested_file_id`). Because the lookup was wrapped in `unwrap_or_default()`,
/// the schema mismatch silently produced "No test mappings found" for every
/// entity, mapped or not.
///
/// 0156 extends the same schema-truthful fixtures for path alias / unique
/// suffix / Ambiguous resolution (M7: seed ≥1 mapping so TableEmpty does not
/// short-circuit the resolver; M8: prefer this insert style).
fn insert_file(storage: &StorageManager, id: i64, path: &str) {
    storage
        .get_connection()
        .execute(
            "INSERT INTO project_files (id, file_path, last_indexed_at) VALUES (?1, ?2, '2026-01-01T00:00:00Z')",
            (id, path),
        )
        .unwrap();
}

fn insert_symbol(storage: &StorageManager, id: i64, file_id: i64, name: &str) {
    storage
        .get_connection()
        .execute(
            "INSERT INTO project_symbols (id, file_id, qualified_name, symbol_name, symbol_kind, last_indexed_at) \
             VALUES (?1, ?2, ?3, ?3, 'Function', '2026-01-01T00:00:00Z')",
            (id, file_id, name),
        )
        .unwrap();
}

fn insert_mapping(
    storage: &StorageManager,
    test_symbol_id: i64,
    test_file_id: i64,
    tested_symbol_id: Option<i64>,
    tested_file_id: Option<i64>,
) {
    storage
        .get_connection()
        .execute(
            "INSERT INTO test_mapping (test_symbol_id, test_file_id, tested_symbol_id, tested_file_id, last_indexed_at) \
             VALUES (?1, ?2, ?3, ?4, '2026-01-01T00:00:00Z')",
            (test_symbol_id, test_file_id, tested_symbol_id, tested_file_id),
        )
        .unwrap();
}

/// Baseline: one mapped file + one test file so the table is never empty (M7).
fn seed_baseline_mapping(storage: &StorageManager) {
    insert_file(storage, 1, "src/lib.rs");
    insert_file(storage, 2, "tests/lib_test.rs");
    insert_symbol(storage, 1, 1, "tested_fn");
    insert_symbol(storage, 2, 2, "test_tested_fn");
    insert_mapping(storage, 2, 2, Some(1), Some(1));
}

#[test]
fn test_explain_test_mappings_returns_mapped_tests_for_indexed_file() {
    let tmp = tempdir().unwrap();
    let storage = StorageManager::init(&tmp.path().join("ledger.db")).unwrap();

    seed_baseline_mapping(&storage);

    let state = explain_test_mappings(storage.get_connection(), "src/lib.rs");
    assert_eq!(
        state,
        TestMappingState::Mapped {
            tests: vec!["tests/lib_test.rs::test_tested_fn".to_string()],
            resolved_path: Some("src/lib.rs".to_string()),
        }
    );
}

#[test]
fn test_explain_test_mappings_falls_back_to_symbol_name() {
    let tmp = tempdir().unwrap();
    let storage = StorageManager::init(&tmp.path().join("ledger.db")).unwrap();

    seed_baseline_mapping(&storage);

    // Entity given as a symbol name rather than a file path.
    let state = explain_test_mappings(storage.get_connection(), "tested_fn");
    assert_eq!(
        state,
        TestMappingState::Mapped {
            tests: vec!["tests/lib_test.rs::test_tested_fn".to_string()],
            resolved_path: None,
        }
    );
}

#[test]
fn test_explain_test_mappings_reports_indexed_but_unmapped_entity() {
    let tmp = tempdir().unwrap();
    let storage = StorageManager::init(&tmp.path().join("ledger.db")).unwrap();

    // A mapping must exist somewhere for the table to be non-empty, but it
    // must not reference src/orphan.rs.
    seed_baseline_mapping(&storage);
    insert_file(&storage, 3, "src/orphan.rs");

    let state = explain_test_mappings(storage.get_connection(), "src/orphan.rs");
    assert_eq!(
        state,
        TestMappingState::NoMappingsForEntity {
            resolved_path: Some("src/orphan.rs".to_string()),
        }
    );
}

#[test]
fn test_explain_test_mappings_reports_entity_not_indexed() {
    let tmp = tempdir().unwrap();
    let storage = StorageManager::init(&tmp.path().join("ledger.db")).unwrap();

    seed_baseline_mapping(&storage);

    let state = explain_test_mappings(storage.get_connection(), "src/never_indexed.rs");
    assert_eq!(state, TestMappingState::EntityNotIndexed);
}

#[test]
fn test_explain_test_mappings_reports_empty_table() {
    let tmp = tempdir().unwrap();
    let storage = StorageManager::init(&tmp.path().join("ledger.db")).unwrap();

    // No rows inserted anywhere; `test_mapping` exists (via migration) but is empty.
    let state = explain_test_mappings(storage.get_connection(), "src/lib.rs");
    assert_eq!(state, TestMappingState::TableEmpty);
}

/// DoD-1 / M1: `module.rs` when only `module/mod.rs` is indexed → alias Mapped.
#[test]
fn test_explain_test_mappings_dir_module_alias_rs_to_mod() {
    let tmp = tempdir().unwrap();
    let storage = StorageManager::init(&tmp.path().join("ledger.db")).unwrap();

    seed_baseline_mapping(&storage);
    insert_file(&storage, 10, "src/pkg/mod.rs");
    insert_file(&storage, 11, "tests/pkg_test.rs");
    insert_symbol(&storage, 10, 10, "pkg_fn");
    insert_symbol(&storage, 11, 11, "test_pkg_fn");
    insert_mapping(&storage, 11, 11, Some(10), Some(10));

    let state = explain_test_mappings(storage.get_connection(), "src/pkg.rs");
    assert_eq!(
        state,
        TestMappingState::Mapped {
            tests: vec!["tests/pkg_test.rs::test_pkg_fn".to_string()],
            resolved_path: Some("src/pkg/mod.rs".to_string()),
        }
    );
}

/// DoD-1b / M1: extensionless `src/pkg` when only `src/pkg.rs` is indexed.
#[test]
fn test_explain_test_mappings_extensionless_alias_to_rs() {
    let tmp = tempdir().unwrap();
    let storage = StorageManager::init(&tmp.path().join("ledger.db")).unwrap();

    seed_baseline_mapping(&storage);
    insert_file(&storage, 20, "src/pkg.rs");
    insert_file(&storage, 21, "tests/pkg_test.rs");
    insert_symbol(&storage, 20, 20, "pkg_fn");
    insert_symbol(&storage, 21, 21, "test_pkg_fn");
    insert_mapping(&storage, 21, 21, Some(20), Some(20));

    let state = explain_test_mappings(storage.get_connection(), "src/pkg");
    assert_eq!(
        state,
        TestMappingState::Mapped {
            tests: vec!["tests/pkg_test.rs::test_pkg_fn".to_string()],
            resolved_path: Some("src/pkg.rs".to_string()),
        }
    );
}

/// DoD-1b / M1 / L1: extensionless `src/pkg` when only `src/pkg/mod.rs` is
/// indexed (the other alias candidate). Guards against alias list regressing
/// to `.rs` only.
#[test]
fn test_explain_test_mappings_extensionless_alias_to_mod() {
    let tmp = tempdir().unwrap();
    let storage = StorageManager::init(&tmp.path().join("ledger.db")).unwrap();

    seed_baseline_mapping(&storage);
    // Only the mod.rs candidate — not src/pkg.rs.
    insert_file(&storage, 22, "src/pkg/mod.rs");
    insert_file(&storage, 23, "tests/pkg_mod_test.rs");
    insert_symbol(&storage, 22, 22, "pkg_mod_fn");
    insert_symbol(&storage, 23, 23, "test_pkg_mod_fn");
    insert_mapping(&storage, 23, 23, Some(22), Some(22));

    let state = explain_test_mappings(storage.get_connection(), "src/pkg");
    assert_eq!(
        state,
        TestMappingState::Mapped {
            tests: vec!["tests/pkg_mod_test.rs::test_pkg_mod_fn".to_string()],
            resolved_path: Some("src/pkg/mod.rs".to_string()),
        }
    );
}

/// DoD-3 / M2: unique full-input suffix resolves to the sole match.
#[test]
fn test_explain_test_mappings_unique_path_suffix() {
    let tmp = tempdir().unwrap();
    let storage = StorageManager::init(&tmp.path().join("ledger.db")).unwrap();

    seed_baseline_mapping(&storage);
    insert_file(&storage, 30, "src/commands/doctor/finding.rs");
    insert_file(&storage, 31, "tests/finding_test.rs");
    insert_symbol(&storage, 30, 30, "finding_fn");
    insert_symbol(&storage, 31, 31, "test_finding_fn");
    insert_mapping(&storage, 31, 31, Some(30), Some(30));

    let state = explain_test_mappings(storage.get_connection(), "finding.rs");
    assert_eq!(
        state,
        TestMappingState::Mapped {
            tests: vec!["tests/finding_test.rs::test_finding_fn".to_string()],
            resolved_path: Some("src/commands/doctor/finding.rs".to_string()),
        }
    );
}

/// DoD-3 / M2: multi-match basename → EntityAmbiguous, sorted, no silent pick.
#[test]
fn test_explain_test_mappings_ambiguous_mod_rs_suffix() {
    let tmp = tempdir().unwrap();
    let storage = StorageManager::init(&tmp.path().join("ledger.db")).unwrap();

    seed_baseline_mapping(&storage);
    insert_file(&storage, 40, "src/a/mod.rs");
    insert_file(&storage, 41, "src/b/mod.rs");
    insert_file(&storage, 42, "src/c/mod.rs");
    // Seed a mapping so TableEmpty does not short-circuit (M7); map to lib.
    // (mod.rs files themselves need not be mapped.)

    let state = explain_test_mappings(storage.get_connection(), "mod.rs");
    match state {
        TestMappingState::EntityAmbiguous { query, candidates } => {
            assert_eq!(query, "mod.rs");
            assert_eq!(
                candidates,
                vec![
                    "src/a/mod.rs".to_string(),
                    "src/b/mod.rs".to_string(),
                    "src/c/mod.rs".to_string(),
                ]
            );
        }
        other => panic!("expected EntityAmbiguous, got {other:?}"),
    }
}

/// DoD-3 / L2: 11 suffix hits → EntityAmbiguous with all 11 candidates sorted
/// by `file_path` (display cap of 10 + "and N more" is CLI-side; data must
/// retain the full ordered list).
#[test]
fn test_explain_test_mappings_ambiguous_mod_rs_eleven_sorted() {
    let tmp = tempdir().unwrap();
    let storage = StorageManager::init(&tmp.path().join("ledger.db")).unwrap();

    seed_baseline_mapping(&storage);
    // Lexicographic dirs a01..a11 so ORDER BY file_path is deterministic.
    let mut expected: Vec<String> = Vec::with_capacity(11);
    for i in 1..=11 {
        let path = format!("src/a{i:02}/mod.rs");
        insert_file(&storage, 100 + i, &path);
        expected.push(path);
    }
    expected.sort();

    let state = explain_test_mappings(storage.get_connection(), "mod.rs");
    match state {
        TestMappingState::EntityAmbiguous { query, candidates } => {
            assert_eq!(query, "mod.rs");
            assert_eq!(candidates.len(), 11, "data side must keep all hits");
            assert_eq!(candidates, expected);
            // Contract for CLI display cap (show min(10), overflow = total-10).
            assert_eq!(candidates.len().min(10), 10);
            assert_eq!(candidates.len() - 10, 1);
        }
        other => panic!("expected EntityAmbiguous with 11 candidates, got {other:?}"),
    }
}

/// BS1: both `X.rs` and `X/mod.rs` present, query `X.rs` → Exact (not mod alias).
#[test]
fn test_explain_test_mappings_exact_beats_alias_when_both_exist() {
    let tmp = tempdir().unwrap();
    let storage = StorageManager::init(&tmp.path().join("ledger.db")).unwrap();

    seed_baseline_mapping(&storage);
    insert_file(&storage, 50, "src/dual.rs");
    insert_file(&storage, 51, "src/dual/mod.rs");
    insert_file(&storage, 52, "tests/dual_rs_test.rs");
    insert_file(&storage, 53, "tests/dual_mod_test.rs");
    insert_symbol(&storage, 50, 50, "dual_rs_fn");
    insert_symbol(&storage, 51, 51, "dual_mod_fn");
    insert_symbol(&storage, 52, 52, "test_dual_rs");
    insert_symbol(&storage, 53, 53, "test_dual_mod");
    insert_mapping(&storage, 52, 52, Some(50), Some(50));
    insert_mapping(&storage, 53, 53, Some(51), Some(51));

    let state = explain_test_mappings(storage.get_connection(), "src/dual.rs");
    assert_eq!(
        state,
        TestMappingState::Mapped {
            tests: vec!["tests/dual_rs_test.rs::test_dual_rs".to_string()],
            resolved_path: Some("src/dual.rs".to_string()),
        }
    );
}

/// Extensionless when both `{p}.rs` and `{p}/mod.rs` exist → do not guess (fall through).
#[test]
fn test_explain_test_mappings_extensionless_both_alias_candidates_no_guess() {
    let tmp = tempdir().unwrap();
    let storage = StorageManager::init(&tmp.path().join("ledger.db")).unwrap();

    seed_baseline_mapping(&storage);
    insert_file(&storage, 60, "src/both.rs");
    insert_file(&storage, 61, "src/both/mod.rs");

    // Suffix `src/both` matches neither via `= ? OR LIKE '%/'||?` for `.rs`/`mod.rs`.
    // Symbol `src/both` won't exist → EntityNotIndexed (not silent pick).
    let state = explain_test_mappings(storage.get_connection(), "src/both");
    assert_eq!(state, TestMappingState::EntityNotIndexed);
}

#[cfg(target_os = "windows")]
#[test]
fn test_explain_test_mappings_windows_case_fold_exact() {
    let tmp = tempdir().unwrap();
    let storage = StorageManager::init(&tmp.path().join("ledger.db")).unwrap();

    seed_baseline_mapping(&storage);

    let state = explain_test_mappings(storage.get_connection(), "Src/Lib.rs");
    assert_eq!(
        state,
        TestMappingState::Mapped {
            tests: vec!["tests/lib_test.rs::test_tested_fn".to_string()],
            resolved_path: Some("src/lib.rs".to_string()),
        }
    );
}
