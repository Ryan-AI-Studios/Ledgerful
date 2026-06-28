use rusqlite_migration::M;

pub fn m41_sync() -> Vec<M<'static>> {
    vec![M::up(
        "CREATE TABLE IF NOT EXISTS sync_state (
            id INTEGER PRIMARY KEY CHECK (id = 1),
            last_extract_hlc TEXT,
            last_apply_hlc TEXT,
            last_run_at TEXT,
            device_id TEXT NOT NULL DEFAULT ''
        );
        CREATE TABLE IF NOT EXISTS tx_tombstones (
            tx_id TEXT NOT NULL PRIMARY KEY,
            tombstone_hlc TEXT NOT NULL,
            reason TEXT NOT NULL
        );
        ALTER TABLE ledger_entries ADD COLUMN entry_hlc TEXT;",
    )]
}
