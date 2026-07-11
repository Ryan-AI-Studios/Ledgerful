use rusqlite::Transaction;
use rusqlite_migration::{HookError, M, MigrationHook};

/// Adds the additive ledger chain hash schema (m51, Track 0046).
///
/// The chain lives entirely outside the 5-field Ed25519 signing basis in
/// `src/ledger/crypto.rs`: a per-entry `prev_hash` column links entries, and a
/// separate signed `chain_head` row binds the latest entry hash, genesis
/// boundary, and chain length. The existing entry signature is never modified.
///
/// `ledger_entries.prev_hash` is nullable. The first post-genesis entry stores
/// `NULL` (empty prev hash); every subsequent entry stores the previous head's
/// `latest_entry_hash`.
///
/// `chain_head` is a singleton table: `id INTEGER PRIMARY KEY CHECK (id = 1)`.
/// The `id = 1` CHECK enforces exactly one head row. The head is signed over
/// `chain_head:{NL}latest_entry_hash:{NL}genesis:{NL}length:{NL}` where NL is a
/// newline.
///
/// Registered unconditionally for the same backward-compat reason as m41-m50:
/// a DB created by a binary that has run m51 would fail the rusqlite_migration
/// pre-flight check on a binary without the migration. The column and table are
/// empty in builds that do not populate them, so the surface-area leak is
/// harmless.
pub fn m51_ledger_chain_hash() -> Vec<M<'static>> {
    vec![
        M::up_with_hook("", add_column_hook("ledger_entries", "prev_hash", "TEXT")),
        M::up_with_hook(
            "",
            create_singleton_table_hook(
                "chain_head",
                "id INTEGER PRIMARY KEY CHECK (id = 1),
                 latest_entry_hash TEXT NOT NULL,
                 genesis TEXT NOT NULL,
                 length INTEGER NOT NULL,
                 head_signature TEXT,
                 head_public_key TEXT,
                 updated_at TEXT NOT NULL",
            ),
        ),
    ]
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

fn create_singleton_table_hook(table: &'static str, schema: &'static str) -> impl MigrationHook {
    move |tx: &Transaction| -> Result<(), HookError> {
        let existing: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
                [table],
                |row| row.get(0),
            )
            .map_err(HookError::from)?;
        if existing == 0 {
            tx.execute_batch(&format!("CREATE TABLE {table} ({schema});"))
                .map_err(HookError::from)?;
        }
        Ok(())
    }
}
