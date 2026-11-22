use rusqlite_migration::M;

/// Durable immutable analysis baselines (m57, Track 0601).
///
/// Registered unconditionally so schema_version stays monotonic. An older
/// binary fails `rusqlite_migration` preflight after this migration has run.
///
/// Immutability here means refuse-overwrite plus direct `UPDATE`/`DELETE`
/// triggers; it is not tamper-evidence and does not intercept
/// `INSERT OR REPLACE` (SQLite does not fire the delete trigger for a
/// REPLACE conflict unless `recursive_triggers` is enabled, which this
/// database does not enable). The storage module therefore only ever issues a
/// plain `INSERT`.
pub fn m57_analysis_baselines() -> Vec<M<'static>> {
    vec![M::up(
        "CREATE TABLE analysis_baselines (
            name TEXT PRIMARY KEY,
            baseline_schema INTEGER NOT NULL,
            payload_kind TEXT NOT NULL,
            identity_json TEXT NOT NULL,
            payload_json TEXT NOT NULL,
            payload_digest TEXT NOT NULL,
            created_at TEXT NOT NULL
        );
        CREATE TRIGGER IF NOT EXISTS analysis_baselines_no_update
            BEFORE UPDATE ON analysis_baselines
            BEGIN
                SELECT RAISE(ABORT, 'analysis_baselines rows are immutable');
            END;
        CREATE TRIGGER IF NOT EXISTS analysis_baselines_no_delete
            BEFORE DELETE ON analysis_baselines
            BEGIN
                SELECT RAISE(ABORT, 'analysis_baselines rows are immutable');
            END;",
    )]
}
