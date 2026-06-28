use rusqlite_migration::M;

/// Creates the `hotspot_trends` table used by the post-commit hook to record
/// per-file hotspot scores over time (Track TA18).
///
/// The table is intentionally separate from `hotspot_history` (m38):
/// `hotspot_history` captures full snapshots written by `hotspots --snapshot`
/// and `--bootstrap` and is consumed by dashboard crossing-count queries, while
/// `hotspot_trends` is a lean append-only stream keyed by commit hash for the
/// CLI `hotspots trend` output.
pub fn m46_hotspot_trends() -> Vec<M<'static>> {
    vec![M::up(
        "CREATE TABLE IF NOT EXISTS hotspot_trends (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            file_path TEXT NOT NULL,
            score REAL NOT NULL,
            frequency REAL,
            complexity REAL,
            commit_hash TEXT,
            recorded_at TEXT NOT NULL
        );

        CREATE INDEX IF NOT EXISTS idx_hotspot_trends_file_recorded_at
            ON hotspot_trends(file_path, recorded_at);",
    )]
}
