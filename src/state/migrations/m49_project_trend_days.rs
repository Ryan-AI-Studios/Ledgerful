use rusqlite_migration::M;

/// Creates the `project_trend_days` table — a daily rollup of project-level
/// hotspot trends (Track 0038). Follows the `usage_days`/m44 pattern: a single
/// `day TEXT PK` table, registered unconditionally.
///
/// The table is populated incrementally by the post-commit hook after
/// `hotspot_trends` insertion, plus a bounded (90-day) idempotent catch-up
/// from existing `hotspot_trends` data. The `GET /api/trends` endpoint reads
/// from this table — no per-request git-history walk.
pub fn m49_project_trend_days() -> Vec<M<'static>> {
    vec![M::up(
        "CREATE TABLE IF NOT EXISTS project_trend_days (
            day TEXT NOT NULL PRIMARY KEY,
            score REAL NOT NULL DEFAULT 0,
            changes INTEGER NOT NULL DEFAULT 0,
            high_risk_count INTEGER NOT NULL DEFAULT 0
        );",
    )]
}
