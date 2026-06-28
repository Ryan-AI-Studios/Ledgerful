use ledgerful::commands::verify::{TestMappingState, explain_test_mappings};
use ledgerful::state::storage::StorageManager;
use tempfile::tempdir;

/// Regression coverage for CG-F17: `verify --explain --entity` used to query
/// `tm.test_name` / `tm.source_file_id`, columns that don't exist on the real
/// `test_mapping` schema (`test_symbol_id`/`test_file_id`/`tested_symbol_id`/
/// `tested_file_id`). Because the lookup was wrapped in `unwrap_or_default()`,
/// the schema mismatch silently produced "No test mappings found" for every
/// entity, mapped or not.
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

#[test]
fn test_explain_test_mappings_returns_mapped_tests_for_indexed_file() {
    let tmp = tempdir().unwrap();
    let storage = StorageManager::init(&tmp.path().join("ledger.db")).unwrap();

    insert_file(&storage, 1, "src/lib.rs");
    insert_file(&storage, 2, "tests/lib_test.rs");
    insert_symbol(&storage, 1, 1, "tested_fn");
    insert_symbol(&storage, 2, 2, "test_tested_fn");
    insert_mapping(&storage, 2, 2, Some(1), Some(1));

    let state = explain_test_mappings(storage.get_connection(), "src/lib.rs");
    assert_eq!(
        state,
        TestMappingState::Mapped(vec!["tests/lib_test.rs::test_tested_fn".to_string()])
    );
}

#[test]
fn test_explain_test_mappings_falls_back_to_symbol_name() {
    let tmp = tempdir().unwrap();
    let storage = StorageManager::init(&tmp.path().join("ledger.db")).unwrap();

    insert_file(&storage, 1, "src/lib.rs");
    insert_file(&storage, 2, "tests/lib_test.rs");
    insert_symbol(&storage, 1, 1, "tested_fn");
    insert_symbol(&storage, 2, 2, "test_tested_fn");
    insert_mapping(&storage, 2, 2, Some(1), Some(1));

    // Entity given as a symbol name rather than a file path.
    let state = explain_test_mappings(storage.get_connection(), "tested_fn");
    assert_eq!(
        state,
        TestMappingState::Mapped(vec!["tests/lib_test.rs::test_tested_fn".to_string()])
    );
}

#[test]
fn test_explain_test_mappings_reports_indexed_but_unmapped_entity() {
    let tmp = tempdir().unwrap();
    let storage = StorageManager::init(&tmp.path().join("ledger.db")).unwrap();

    // A mapping must exist somewhere for the table to be non-empty, but it
    // must not reference src/orphan.rs.
    insert_file(&storage, 1, "src/lib.rs");
    insert_file(&storage, 2, "tests/lib_test.rs");
    insert_file(&storage, 3, "src/orphan.rs");
    insert_symbol(&storage, 1, 1, "tested_fn");
    insert_symbol(&storage, 2, 2, "test_tested_fn");
    insert_mapping(&storage, 2, 2, Some(1), Some(1));

    let state = explain_test_mappings(storage.get_connection(), "src/orphan.rs");
    assert_eq!(state, TestMappingState::NoMappingsForEntity);
}

#[test]
fn test_explain_test_mappings_reports_entity_not_indexed() {
    let tmp = tempdir().unwrap();
    let storage = StorageManager::init(&tmp.path().join("ledger.db")).unwrap();

    insert_file(&storage, 1, "src/lib.rs");
    insert_file(&storage, 2, "tests/lib_test.rs");
    insert_symbol(&storage, 1, 1, "tested_fn");
    insert_symbol(&storage, 2, 2, "test_tested_fn");
    insert_mapping(&storage, 2, 2, Some(1), Some(1));

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
