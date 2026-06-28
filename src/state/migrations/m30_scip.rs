use rusqlite_migration::M;

pub fn m30_scip() -> Vec<M<'static>> {
    vec![M::up(
        "CREATE TABLE IF NOT EXISTS scip_indices (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            index_path TEXT NOT NULL UNIQUE,
            blake3_hash TEXT NOT NULL,
            indexed_at TEXT NOT NULL DEFAULT (datetime('now'))
        );",
    )]
}
