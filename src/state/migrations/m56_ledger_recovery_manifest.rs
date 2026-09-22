use rusqlite_migration::M;

/// Durable adoption manifest (m56, Track 0417).
///
/// Registered unconditionally so schema_version stays monotonic. An older
/// binary fails `rusqlite_migration` preflight after this migration has run.
pub fn m56_ledger_recovery_manifest() -> Vec<M<'static>> {
    vec![M::up(
        "CREATE TABLE ledger_recovery_manifest (
            manifest_digest TEXT PRIMARY KEY,
            manifest_json TEXT NOT NULL,
            repo_identity TEXT NOT NULL,
            stored_head TEXT,
            adopted_count INTEGER NOT NULL,
            created_at TEXT NOT NULL
        );",
    )]
}
