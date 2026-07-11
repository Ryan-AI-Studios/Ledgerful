use rusqlite_migration::M;

/// Adds the `observed` column to `ledger_entries` for track 0050
/// (Observe/Enforce mode). The column is nullable so that historical entries
/// without the marker deserialize as `None`. A value of `1` means the entry
/// was recorded under observe mode with an acknowledged warning.
pub fn m50_ledger_entry_observed() -> Vec<M<'static>> {
    vec![M::up(
        "ALTER TABLE ledger_entries ADD COLUMN observed INTEGER;",
    )]
}
