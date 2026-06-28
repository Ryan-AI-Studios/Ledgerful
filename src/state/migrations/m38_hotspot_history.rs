use rusqlite_migration::M;

pub fn m38_hotspot_history() -> Vec<M<'static>> {
    vec![M::up(
        "CREATE TABLE hotspot_history (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            snapshot_id INTEGER REFERENCES snapshots(id),
            file_path TEXT NOT NULL,
            score REAL NOT NULL,
            display_score REAL NOT NULL,
            complexity INTEGER NOT NULL,
            frequency REAL NOT NULL,
            centrality REAL,
            timestamp TEXT NOT NULL
        );

        CREATE TABLE temporal_coupling_history (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            snapshot_id INTEGER REFERENCES snapshots(id),
            file_a TEXT NOT NULL,
            file_b TEXT NOT NULL,
            score REAL NOT NULL,
            timestamp TEXT NOT NULL
        );

        -- Per M8 opencode-review L5: index `timestamp` so the
        -- `MAX(timestamp)` subquery in
        -- `src/commands/web/server.rs::count_hotspots_crossed` is an
        -- O(1) index seek instead of a full table scan. The
        -- `IF NOT EXISTS` clause is a no-op on a fresh DB and on any
        -- DB that already ran m38 against the original (unindexed)
        -- table.
        CREATE INDEX IF NOT EXISTS idx_hotspot_history_timestamp
            ON hotspot_history(timestamp);",
    )]
}
