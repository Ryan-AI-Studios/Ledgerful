use rusqlite_migration::M;

pub fn m37_ci_deploy_enrichment() -> Vec<M<'static>> {
    vec![M::up(
        "CREATE TABLE IF NOT EXISTS deploy_manifests (
            file_path      TEXT PRIMARY KEY,
            manifest_type  TEXT NOT NULL,
            risk_tier      INTEGER NOT NULL,
            service_name   TEXT,
            owner          TEXT,
            last_indexed_at TEXT NOT NULL
         );
         ALTER TABLE ci_gates ADD COLUMN workflow_name TEXT;
         ALTER TABLE ci_gates ADD COLUMN environment TEXT;
         ALTER TABLE ci_gates ADD COLUMN artifacts TEXT;
         ALTER TABLE ci_gates ADD COLUMN release_gates TEXT;",
    )]
}
