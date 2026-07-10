use rusqlite::Transaction;
use rusqlite_migration::{HookError, M, MigrationHook};

/// Adds nullable `additions INTEGER`, `deletions INTEGER`, and
/// `is_binary INTEGER DEFAULT 0` columns to `changed_files` (m48, Track 0037).
///
/// The `additions`/`deletions` columns store per-file addition/deletion counts
/// computed at commit time from the committed diff. `is_binary` is set to 1
/// when git numstat reports the file as binary (`-\t-`), letting the frontend
/// render "binary" instead of the generic "—" used for pre-m48 legacy rows.
///
/// All three are intentionally outside the Ed25519 signing basis in
/// `src/ledger/crypto.rs`; backfilling or updating them never affects ledger
/// signatures.
///
/// Like m41/m42/m44/m45/m46/m47, m48 is registered UNCONDITIONALLY to keep
/// schema_version monotonic across binaries built with different feature sets.
/// The columns are nullable/defaulted, so the surface-area leak is harmless in
/// builds that do not populate them.
///
/// Each ALTER is wrapped in an `M::up_with_hook` with a no-op SQL body and a
/// hook that conditionally runs the ALTER only if the column is missing. This
/// mirrors the m45 idempotent pattern and avoids getting the DB stuck if a
/// previous failed attempt partially added a column.
pub fn m48_changed_files_diff_stats() -> Vec<M<'static>> {
    vec![
        M::up_with_hook("", add_column_hook("changed_files", "additions", "INTEGER")),
        M::up_with_hook("", add_column_hook("changed_files", "deletions", "INTEGER")),
        M::up_with_hook(
            "",
            add_column_hook("changed_files", "is_binary", "INTEGER NOT NULL DEFAULT 0"),
        ),
    ]
}

/// Build a `MigrationHook` that adds a column to a table if and only if it does
/// not already exist.
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
