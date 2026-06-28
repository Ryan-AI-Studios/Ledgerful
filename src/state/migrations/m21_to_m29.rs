use rusqlite_migration::M;

pub fn m21_to_m29() -> Vec<M<'static>> {
    vec![
        M::up(
            "CREATE TABLE IF NOT EXISTS env_references (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                file_id INTEGER NOT NULL REFERENCES project_files(id),
                symbol_id INTEGER REFERENCES project_symbols(id),
                var_name TEXT NOT NULL,
                reference_kind TEXT NOT NULL,
                confidence REAL NOT NULL DEFAULT 1.0,
                line_start INTEGER,
                last_indexed_at TEXT NOT NULL,
                UNIQUE(file_id, symbol_id, var_name, reference_kind)
            );
            CREATE INDEX IF NOT EXISTS idx_env_references_file ON env_references(file_id);
            CREATE INDEX IF NOT EXISTS idx_env_references_var ON env_references(var_name);",
        ),
        M::up(
            "CREATE TABLE IF NOT EXISTS embeddings (
                id           INTEGER PRIMARY KEY AUTOINCREMENT,
                entity_type  TEXT    NOT NULL,
                entity_id    TEXT    NOT NULL,
                content_hash TEXT    NOT NULL,
                model_name   TEXT    NOT NULL,
                dimensions   INTEGER NOT NULL,
                vector       BLOB    NOT NULL,
                created_at   TEXT    NOT NULL DEFAULT (datetime('now')),
                UNIQUE (entity_type, entity_id, model_name)
            );
            CREATE INDEX IF NOT EXISTS idx_embeddings_entity ON embeddings (entity_type, entity_id);",
        ),
        M::up(
            "CREATE TABLE IF NOT EXISTS doc_chunks (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                file_path   TEXT    NOT NULL,
                chunk_index INTEGER NOT NULL,
                heading     TEXT,
                content     TEXT    NOT NULL,
                token_count INTEGER NOT NULL,
                UNIQUE (file_path, chunk_index)
            );",
        ),
        M::up(
            "CREATE TABLE IF NOT EXISTS api_endpoints (
                id           INTEGER PRIMARY KEY AUTOINCREMENT,
                spec_path    TEXT NOT NULL,
                method       TEXT NOT NULL,
                path         TEXT NOT NULL,
                summary      TEXT,
                description  TEXT,
                tags         TEXT,
                content_hash TEXT NOT NULL,
                UNIQUE (spec_path, method, path)
            );",
        ),
        M::up(
            "CREATE TABLE IF NOT EXISTS test_outcome_history (
                id                INTEGER PRIMARY KEY AUTOINCREMENT,
                diff_embedding_id INTEGER NOT NULL REFERENCES embeddings(id),
                test_file         TEXT    NOT NULL,
                outcome           TEXT    NOT NULL CHECK (outcome IN ('pass', 'fail', 'skip')),
                commit_hash       TEXT,
                recorded_at       TEXT    NOT NULL DEFAULT (datetime('now'))
            );
            CREATE INDEX IF NOT EXISTS idx_test_history_diff ON test_outcome_history (diff_embedding_id);",
        ),
        M::up(
            "CREATE TABLE IF NOT EXISTS observability_snapshots (
                id           INTEGER PRIMARY KEY AUTOINCREMENT,
                service_name TEXT NOT NULL,
                error_rate   REAL,
                latency_p99  REAL,
                recorded_at  TEXT NOT NULL DEFAULT (datetime('now'))
            );",
        ),
        M::up(
            "DROP TABLE IF EXISTS observability_snapshots;
             CREATE TABLE observability_snapshots (
                 id           INTEGER PRIMARY KEY AUTOINCREMENT,
                 signal_type  TEXT NOT NULL,
                 signal_label TEXT NOT NULL,
                 metric_value REAL NOT NULL,
                 raw_excerpt  TEXT NOT NULL,
                 captured_at  TEXT NOT NULL DEFAULT (datetime('now')),
                 diff_pair_id TEXT NOT NULL
             );",
        ),
        M::up(
            "ALTER TABLE project_files ADD COLUMN service_name TEXT;
             CREATE INDEX IF NOT EXISTS idx_project_files_service ON project_files(service_name);",
        ),
        M::up(
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_project_symbols_unique_key \
             ON project_symbols(file_id, qualified_name, symbol_kind);",
        ),
    ]
}
