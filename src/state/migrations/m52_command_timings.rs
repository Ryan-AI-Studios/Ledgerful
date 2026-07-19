use rusqlite_migration::M;

/// Creates the local-only `command_timings` table (m52, Track 0043).
///
/// Outer rows (one per CLI invocation) and inner span rows share this table.
/// `run_id` links an invocation's outer + inner rows so capture can flush them
/// in a single batched transaction. The table is not a `LedgerEntry` and never
/// enters the Ed25519 signing basis.
///
/// Registered unconditionally for the same backward-compat reason as m41–m51:
/// a DB created by a binary that has run m52 would fail the rusqlite_migration
/// pre-flight check on a binary without the migration. The table is empty in
/// builds that do not populate it, so the surface-area leak is harmless.
pub fn m52_command_timings() -> Vec<M<'static>> {
    vec![M::up(
        "CREATE TABLE IF NOT EXISTS command_timings (
            id              INTEGER PRIMARY KEY AUTOINCREMENT,
            run_id          TEXT NOT NULL,
            ts_utc          TEXT NOT NULL,
            command         TEXT NOT NULL,
            duration_ms     INTEGER NOT NULL,
            exit_code       INTEGER NOT NULL,
            repo_size_bytes INTEGER,
            argv_hash       TEXT,
            ledger_tx_id    TEXT,
            parent_span_id  TEXT,
            span_name       TEXT,
            notes           TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_command_timings_run     ON command_timings(run_id);
        CREATE INDEX IF NOT EXISTS idx_command_timings_command ON command_timings(command);
        CREATE INDEX IF NOT EXISTS idx_command_timings_ts_utc  ON command_timings(ts_utc);",
    )]
}
