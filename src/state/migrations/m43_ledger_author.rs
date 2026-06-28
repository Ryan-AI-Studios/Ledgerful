use rusqlite_migration::M;

pub fn m43_ledger_author() -> Vec<M<'static>> {
    vec![M::up(
        "ALTER TABLE ledger_entries ADD COLUMN author TEXT NOT NULL DEFAULT 'unknown';",
    )]
}
