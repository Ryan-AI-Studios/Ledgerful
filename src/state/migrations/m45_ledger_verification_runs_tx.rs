use rusqlite::Transaction;
use rusqlite_migration::{HookError, M, MigrationHook};

/// Adds nullable `tx_id TEXT` columns to `verification_runs` and
/// `verification_results` (m45).
///
/// The M8 spec (`conductor/trackM8/spec.md:58`) requires
/// `/api/ledger/:txId` to derive `tests_run` and `flakes` by joining
/// `verification_runs`/`verification_results` against `tx_id`. The
/// m1_to_m10 schema (line 33-49) defines those tables with no link
/// to the ledger's `tx_id` column at all — verification runs existed
/// before the ledger did, and were not retroactively bound to
/// transactions. The new column is NULL for every pre-M8 row (no
/// retroactive backfill is meaningful without knowing which
/// transaction the original run was supposed to gate), so the new
/// endpoint simply returns `tests_run=0, flakes=0` for legacy data
/// — the "honest zero" the spec calls for when the join returns no
/// rows. A follow-up track should populate `tx_id` at verify time
/// when the verify flow is invoked from `ledger commit`.
///
/// Like m42 and m44, m45 is registered UNCONDITIONALLY (not gated on
/// a feature) to preserve backward-compat with existing on-disk DBs
/// that may have been written by a binary built with the `sync`
/// feature. The two ALTERs are no-ops on tables that don't exist.
///
/// **m45 `expected_tables` assertion:** the m45 `verification_runs`
/// and `verification_results` tables are themselves unconditionally
/// asserted in the `expected_tables` check in `migrations/mod.rs`
/// because they were created unconditionally in m1_to_m10.
///
/// **m41-style "no surface-area leak" trade-off (separate concern):**
/// the unconditional m41 registration (and accepting that `sync_state`
/// / `tx_tombstones` tables exist in builds without the `sync` feature)
/// is relevant for the same backward-compat reason as m45, but is a
/// *separate* concern from the `expected_tables` check for m45.
///
/// Each ALTER is wrapped in an `M::up_with_hook` with a no-op SQL
/// body and a hook that conditionally runs the ALTER only if the
/// column is missing. This is required because rusqlite_migration's
/// `user_version` is bumped *after* the SQL runs and the whole
/// transaction is rolled back on error: a plain
/// `ALTER TABLE ... ADD COLUMN` on a DB where the column was added
/// by some prior (failed) attempt would abort the migration, leaving
/// the DB stuck at the prior version forever. The hook-based form
/// succeeds (does nothing) on a DB that already has the column.
pub fn m45_ledger_verification_runs_tx() -> Vec<M<'static>> {
    vec![
        M::up_with_hook("", add_tx_id_column("verification_runs")),
        M::up_with_hook("", add_tx_id_column("verification_results")),
    ]
}

/// Build a `MigrationHook` that adds a `tx_id TEXT` column to the
/// given table if and only if it does not already exist.
///
/// The hook runs inside the migration transaction (`to_latest` wraps
/// the migration list in a single `conn.transaction()`), so the
/// conditional `ALTER` participates in the same atomic commit as
/// the user_version bump.
fn add_tx_id_column(table: &'static str) -> impl MigrationHook {
    move |tx: &Transaction| -> Result<(), HookError> {
        let existing: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info(?1) WHERE name = 'tx_id'",
                [table],
                |row| row.get(0),
            )
            .map_err(HookError::from)?;
        if existing == 0 {
            tx.execute_batch(&format!("ALTER TABLE {} ADD COLUMN tx_id TEXT;", table))
                .map_err(HookError::from)?;
        }
        Ok(())
    }
}
