use rusqlite_migration::M;

pub fn m42_usage_counters() -> Vec<M<'static>> {
    // Schema for the per-repo usage counter store.
    //
    // Originally (M7 r1) this table was `(command_name, count,
    // window_start)`, where `window_start` was per-row window metadata.
    // The `window_start` column was never read by any code path (L4
    // review) and the 7-day flush gate uses the global `last_sent_at`
    // instead, so it has been dropped from the schema.
    //
    // M7 r2 attempted to add a `last_seen_day TEXT` column to track
    // per-command calendar days, but because `command_name` is the
    // sole PRIMARY KEY the UPSERT's `ON CONFLICT` clause overwrote
    // that column on every increment — `SELECT COUNT(DISTINCT
    // last_seen_day)` always returned 1 for the typical "user runs
    // `scan` daily" pattern. The H2 regression fix moves the
    // day-tracking to a separate `usage_days` table (m44), so this
    // table is back to its minimal `(command_name, count)` shape.
    // Pre-existing DBs from the r2 install may have a lingering
    // `last_seen_day` column (the `CREATE TABLE IF NOT EXISTS` here
    // is a no-op for them), but the column is no longer written to
    // or read from.
    vec![M::up(
        "CREATE TABLE IF NOT EXISTS usage_counters (
            command_name TEXT NOT NULL PRIMARY KEY,
            count INTEGER NOT NULL DEFAULT 0
        );",
    )]
}
