use ledgerful::state::storage_cozo::CozoStorage;
use std::path::PathBuf;

#[test]
fn test_new_repo_gets_full_schema() {
    let storage = CozoStorage::new(&PathBuf::from("")).expect("Failed to init memory cozo");
    let cols = storage
        .run_script("::columns ledger_entry")
        .expect("Failed to get columns");
    assert_eq!(cols.rows.len(), 16);
}

#[test]
fn old_schema_is_migrated_to_current_version() {
    let storage = CozoStorage::new(&PathBuf::from("")).expect("Failed to init memory cozo");

    // 1. Manually downgrade schema to 12 columns
    storage
        .run_script("::remove ledger_entry")
        .expect("Failed to remove");
    storage.run_script(":create ledger_entry { id: Int => tx_id: String, category: String, entry_type: String, entity_normalized: String, change_type: String, summary: String, reason: String, committed_at: String, is_breaking: Bool, verification_status: String, trace_id: String }").expect("Failed to create old");

    // 2. Set version to 1
    storage
        .run_script("?[key, value] <- [['cozo_schema_version', '1']] :put cozo_meta")
        .expect("Failed to set version");

    // 3. Insert some data
    storage.run_script("?[id, tx_id, category, entry_type, entity_normalized, change_type, summary, reason, committed_at, is_breaking, verification_status, trace_id] <- [[1, 'tx1', 'FEAT', 'intent', 'src', 'add', 'sum', 'reaz', 'now', false, 'pass', 'tr1']] :put ledger_entry").expect("Failed to insert data");

    // 4. Run migration
    storage.migrate_cozo_schema().expect("Migration failed");

    // 5. Verify 16 columns
    let cols = storage
        .run_script("::columns ledger_entry")
        .expect("Failed to get columns");
    assert_eq!(cols.rows.len(), 16);

    // 6. Verify data preserved and new columns padded
    let res = storage
        .run_script("?[tx_id, signature] := *ledger_entry{tx_id, signature}")
        .expect("Failed to query");
    assert_eq!(res.rows.len(), 1);
    assert_eq!(res.rows[0][0], cozo::DataValue::Str("tx1".into()));
    assert_eq!(res.rows[0][1], cozo::DataValue::Str("".into()));
}

#[test]
fn test_migration_is_idempotent() {
    let storage = CozoStorage::new(&PathBuf::from("")).expect("Failed to init memory cozo");
    storage.migrate_cozo_schema().expect("Migration 1 failed");
    storage.migrate_cozo_schema().expect("Migration 2 failed");
    let cols = storage
        .run_script("::columns ledger_entry")
        .expect("Failed to get columns");
    assert_eq!(cols.rows.len(), 16);
}
