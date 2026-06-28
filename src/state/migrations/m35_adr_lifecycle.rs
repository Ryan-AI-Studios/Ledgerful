use rusqlite_migration::M;

pub fn m35_adr_lifecycle() -> Vec<M<'static>> {
    vec![M::up(
        "CREATE TABLE IF NOT EXISTS adr_metadata (
            adr_id                TEXT PRIMARY KEY,
            status                TEXT NOT NULL,
            owner                 TEXT,
            reviewers             TEXT,
            supersedes            TEXT,
            superseded_by         TEXT,
            affected_entities     TEXT,
            decision_scope        TEXT,
            reviewed_at           TEXT,
            review_interval_days  INTEGER,
            last_updated_at       TEXT NOT NULL
         );
         CREATE INDEX IF NOT EXISTS idx_adr_metadata_status ON adr_metadata(status);
         CREATE INDEX IF NOT EXISTS idx_adr_metadata_owner ON adr_metadata(owner);",
    )]
}
