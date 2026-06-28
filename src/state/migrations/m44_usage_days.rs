use rusqlite_migration::M;

pub fn m44_usage_days() -> Vec<M<'static>> {
    // Schema for the per-repo usage-days store.
    //
    // The original M7 r2 implementation tracked the calendar day of
    // each counter increment as a `last_seen_day` column on
    // `usage_counters`. Because `command_name` is the sole PRIMARY
    // KEY of that table, the UPSERT's `ON CONFLICT` clause overwrote
    // `last_seen_day` on every increment — a user running the same
    // command on N distinct days ended up with one row whose
    // `last_seen_day` was just the most recent day, so
    // `SELECT COUNT(DISTINCT last_seen_day)` returned 1, not N. The
    // spec example (`spec.md:53`) shows `5`; the implementation could
    // only ever produce 1 for the typical "user runs `scan` daily"
    // pattern.
    //
    // This migration introduces a separate `usage_days` table whose
    // sole job is to track which calendar days have seen at least
    // one command invocation. Each `increment_counter` call inserts
    // `INSERT OR IGNORE INTO usage_days(day) VALUES (?1)` with the
    // current UTC date, and `read_active_days` simply counts rows in
    // this table — independent of the `usage_counters` UPSERT and
    // therefore free of the overwriting bug.
    vec![M::up(
        "CREATE TABLE IF NOT EXISTS usage_days (
            day TEXT NOT NULL PRIMARY KEY
        );",
    )]
}
