use rusqlite::Transaction;
use rusqlite_migration::{HookError, M, MigrationHook};

/// Adds `ledger_entries.sig_version` (m53, Track 0072).
///
/// Durable per-entry Ed25519 payload version:
/// - `1` = legacy five-field basis (historical rows; dual-verify only)
/// - `2` = full provenance basis (new commits)
///
/// Existing rows default to `1` so historical signatures keep verifying.
/// New commits write `sig_version = 2`. Dual-verify is keyed on the stored
/// version only (never heuristic try-both).
///
/// The chain hash (`prev_hash` / `entry_hash`) is version-aware via
/// `compute_entry_hash_versioned` in `ledger::crypto` but remains outside
/// the entry signature domain (0046 Option A).
///
/// Registered unconditionally for the same backward-compat reason as m41–m52.
pub fn m53_ledger_sig_version() -> Vec<M<'static>> {
    vec![M::up_with_hook(
        "",
        add_column_hook(
            "ledger_entries",
            "sig_version",
            "INTEGER NOT NULL DEFAULT 1",
        ),
    )]
}

fn add_column_hook(
    table: &'static str,
    column: &'static str,
    column_type: &'static str,
) -> impl MigrationHook {
    move |tx: &Transaction| -> Result<(), HookError> {
        let existing: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info(?1) WHERE name = ?2",
                [table, column],
                |row| row.get(0),
            )
            .map_err(HookError::from)?;
        if existing == 0 {
            tx.execute_batch(&format!(
                "ALTER TABLE {table} ADD COLUMN {column} {column_type};"
            ))
            .map_err(HookError::from)?;
        }
        Ok(())
    }
}
