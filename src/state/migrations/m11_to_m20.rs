use rusqlite_migration::M;

pub fn m11_to_m20() -> Vec<M<'static>> {
    vec![
        M::up(
            "CREATE TABLE IF NOT EXISTS transactions (
                tx_id              TEXT PRIMARY KEY,
                operation_id       TEXT,
                status             TEXT NOT NULL,
                category           TEXT NOT NULL,
                entity             TEXT NOT NULL,
                entity_normalized  TEXT NOT NULL,
                planned_action     TEXT,
                session_id         TEXT NOT NULL,
                source             TEXT NOT NULL DEFAULT 'CLI',
                started_at         TEXT NOT NULL,
                resolved_at        TEXT,
                detected_at        TEXT,
                drift_count        INTEGER DEFAULT 1,
                first_seen_at      TEXT,
                last_seen_at       TEXT,
                issue_ref          TEXT,
                change_type        TEXT,
                summary            TEXT,
                reason             TEXT,
                is_breaking        INTEGER DEFAULT 0,
                verification_status TEXT,
                verification_basis TEXT,
                outcome_notes      TEXT,
                snapshot_id        INTEGER REFERENCES snapshots(id),
                tree_hash          TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_transactions_entity_status ON transactions(entity_normalized, status);
            CREATE UNIQUE INDEX IF NOT EXISTS idx_transactions_unaudited_entity ON transactions(entity_normalized) WHERE status = 'UNAUDITED';
            CREATE UNIQUE INDEX IF NOT EXISTS idx_transactions_pending_entity ON transactions(entity_normalized) WHERE status = 'PENDING';
            CREATE INDEX IF NOT EXISTS idx_transactions_status ON transactions(status);
            CREATE INDEX IF NOT EXISTS idx_transactions_session_id ON transactions(session_id);
            CREATE INDEX IF NOT EXISTS idx_transactions_operation_id ON transactions(operation_id);",
        ),
        M::up(
            "CREATE TABLE IF NOT EXISTS ledger_entries (
                id                 INTEGER PRIMARY KEY AUTOINCREMENT,
                tx_id              TEXT NOT NULL REFERENCES transactions(tx_id),
                operation_id       TEXT,
                category           TEXT NOT NULL,
                entry_type         TEXT NOT NULL DEFAULT 'IMPLEMENTATION',
                entity             TEXT NOT NULL,
                entity_normalized  TEXT NOT NULL,
                change_type        TEXT NOT NULL,
                summary            TEXT NOT NULL,
                reason             TEXT NOT NULL,
                is_breaking        INTEGER DEFAULT 0,
                committed_at       TEXT NOT NULL,
                verification_status TEXT,
                verification_basis TEXT,
                outcome_notes      TEXT,
                issue_ref          TEXT,
                trace_id           TEXT,
                origin             TEXT NOT NULL DEFAULT 'LOCAL',
                snapshot_id        INTEGER REFERENCES snapshots(id),
                tree_hash          TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_ledger_entries_entity ON ledger_entries(entity_normalized);
            CREATE INDEX IF NOT EXISTS idx_ledger_entries_category ON ledger_entries(category);
            CREATE INDEX IF NOT EXISTS idx_ledger_entries_committed_at ON ledger_entries(committed_at);
            CREATE INDEX IF NOT EXISTS idx_ledger_entries_operation_id ON ledger_entries(operation_id);
            
            CREATE VIRTUAL TABLE IF NOT EXISTS ledger_fts
                USING fts5(entity, summary, reason, content=ledger_entries, content_rowid=id);

            CREATE TRIGGER IF NOT EXISTS ledger_fts_ai AFTER INSERT ON ledger_entries BEGIN
                INSERT INTO ledger_fts(rowid, entity, summary, reason) VALUES (new.id, new.entity, new.summary, new.reason);
            END;
            CREATE TRIGGER IF NOT EXISTS ledger_fts_ad AFTER DELETE ON ledger_entries BEGIN
                INSERT INTO ledger_fts(ledger_fts, rowid, entity, summary, reason) VALUES ('delete', old.id, old.entity, old.summary, old.reason);
            END;
            CREATE TRIGGER IF NOT EXISTS ledger_fts_au AFTER UPDATE ON ledger_entries BEGIN
                INSERT INTO ledger_fts(ledger_fts, rowid, entity, summary, reason) VALUES ('delete', old.id, old.entity, old.summary, old.reason);
                INSERT INTO ledger_fts(rowid, entity, summary, reason) VALUES (new.id, new.entity, new.summary, new.reason);
            END;",
        ),
        M::up(
            "CREATE TABLE IF NOT EXISTS tech_stack (
                category           TEXT PRIMARY KEY,
                name               TEXT NOT NULL,
                version_constraint TEXT,
                rules              TEXT NOT NULL DEFAULT '[]',
                locked             INTEGER DEFAULT 0,
                status             TEXT DEFAULT 'ACTIVE',
                entity_type        TEXT DEFAULT 'FILE',
                registered_at      TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS commit_validators (
                id                 INTEGER PRIMARY KEY AUTOINCREMENT,
                category           TEXT NOT NULL,
                name               TEXT NOT NULL,
                description        TEXT,
                executable         TEXT NOT NULL,
                args               TEXT NOT NULL,
                timeout_ms         INTEGER DEFAULT 30000,
                glob               TEXT,
                validation_level   TEXT DEFAULT 'ERROR',
                enabled            INTEGER DEFAULT 1
            );
            CREATE TABLE IF NOT EXISTS category_stack_mappings (
                id                 INTEGER PRIMARY KEY AUTOINCREMENT,
                ledger_category    TEXT NOT NULL,
                stack_category     TEXT NOT NULL REFERENCES tech_stack(category),
                glob               TEXT,
                description        TEXT
            );
            CREATE TABLE IF NOT EXISTS watcher_patterns (
                id                 INTEGER PRIMARY KEY AUTOINCREMENT,
                glob               TEXT NOT NULL,
                category           TEXT NOT NULL,
                source             TEXT NOT NULL DEFAULT 'CONFIG',
                description        TEXT
            );",
        ),
        M::up(
            "CREATE TABLE IF NOT EXISTS token_provenance (
                id                 INTEGER PRIMARY KEY AUTOINCREMENT,
                tx_id              TEXT NOT NULL REFERENCES transactions(tx_id),
                entity             TEXT NOT NULL,
                entity_normalized  TEXT NOT NULL,
                symbol_name        TEXT NOT NULL,
                symbol_type        TEXT NOT NULL,
                action             TEXT NOT NULL -- 'ADDED', 'MODIFIED', 'DELETED'
            );
            CREATE INDEX IF NOT EXISTS idx_token_provenance_tx_id ON token_provenance(tx_id);
            CREATE INDEX IF NOT EXISTS idx_token_provenance_entity_symbol ON token_provenance(entity_normalized, symbol_name);",
        ),
        M::up(
            "CREATE TABLE IF NOT EXISTS project_files (
                id              INTEGER PRIMARY KEY,
                file_path       TEXT NOT NULL,
                language        TEXT,
                content_hash    TEXT,
                git_blob_oid    TEXT,
                file_size       INTEGER,
                mtime_ns        INTEGER,
                parser_version  TEXT NOT NULL DEFAULT '1',
                parse_status    TEXT NOT NULL DEFAULT 'OK',
                last_indexed_at TEXT NOT NULL,
                UNIQUE(file_path)
            );
            CREATE TABLE IF NOT EXISTS index_metadata (
                key   TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS project_symbols (
                id                    INTEGER PRIMARY KEY AUTOINCREMENT,
                file_id               INTEGER NOT NULL REFERENCES project_files(id),
                qualified_name        TEXT NOT NULL,
                symbol_name           TEXT NOT NULL,
                symbol_kind           TEXT NOT NULL,
                visibility            TEXT,
                entrypoint_kind       TEXT NOT NULL DEFAULT 'INTERNAL',
                is_public             INTEGER DEFAULT 0,
                cognitive_complexity  INTEGER,
                cyclomatic_complexity  INTEGER,
                line_start           INTEGER,
                line_end             INTEGER,
                byte_start           INTEGER,
                byte_end             INTEGER,
                signature_hash       TEXT,
                confidence           REAL NOT NULL DEFAULT 1.0,
                evidence             TEXT,
                last_indexed_at      TEXT NOT NULL,
                UNIQUE(file_id, qualified_name, symbol_kind)
            );
            CREATE INDEX IF NOT EXISTS idx_project_symbols_file ON project_symbols(file_id);
            CREATE INDEX IF NOT EXISTS idx_project_symbols_qualified ON project_symbols(qualified_name);
            CREATE INDEX IF NOT EXISTS idx_project_symbols_name ON project_symbols(symbol_name);
            CREATE INDEX IF NOT EXISTS idx_project_symbols_kind ON project_symbols(symbol_kind);
            CREATE INDEX IF NOT EXISTS idx_project_symbols_entrypoint ON project_symbols(entrypoint_kind);
            CREATE TABLE IF NOT EXISTS project_docs (
                id              INTEGER PRIMARY KEY AUTOINCREMENT,
                file_id         INTEGER NOT NULL REFERENCES project_files(id),
                title           TEXT,
                summary         TEXT,
                sections        JSON,
                code_blocks     JSON,
                internal_links  JSON,
                confidence      REAL NOT NULL DEFAULT 1.0,
                last_indexed_at TEXT NOT NULL,
                UNIQUE(file_id)
            );
            CREATE INDEX IF NOT EXISTS idx_project_docs_file_id ON project_docs(file_id);
            CREATE TABLE IF NOT EXISTS project_topology (
                id              INTEGER PRIMARY KEY AUTOINCREMENT,
                dir_path        TEXT NOT NULL,
                role            TEXT NOT NULL,
                confidence      REAL NOT NULL DEFAULT 1.0,
                evidence        TEXT,
                last_indexed_at TEXT NOT NULL,
                UNIQUE(dir_path)
            );
            CREATE UNIQUE INDEX IF NOT EXISTS idx_project_topology_dir_path
                ON project_topology(dir_path);
            CREATE INDEX IF NOT EXISTS idx_project_topology_role
                ON project_topology(role);",
        ),
        M::up(
            "CREATE TABLE IF NOT EXISTS structural_edges (
                id                  INTEGER PRIMARY KEY AUTOINCREMENT,
                caller_symbol_id    INTEGER NOT NULL REFERENCES project_symbols(id),
                caller_file_id      INTEGER NOT NULL REFERENCES project_files(id),
                callee_symbol_id    INTEGER REFERENCES project_symbols(id),
                callee_file_id      INTEGER REFERENCES project_files(id),
                unresolved_callee   TEXT,
                call_kind           TEXT NOT NULL DEFAULT 'DIRECT',
                resolution_status   TEXT NOT NULL DEFAULT 'RESOLVED',
                confidence          REAL NOT NULL DEFAULT 1.0,
                evidence            TEXT,
                FOREIGN KEY (caller_symbol_id) REFERENCES project_symbols(id),
                FOREIGN KEY (caller_file_id) REFERENCES project_files(id),
                FOREIGN KEY (callee_symbol_id) REFERENCES project_symbols(id),
                FOREIGN KEY (callee_file_id) REFERENCES project_files(id)
            );
            CREATE INDEX IF NOT EXISTS idx_structural_edges_caller
                ON structural_edges(caller_symbol_id, caller_file_id);
            CREATE INDEX IF NOT EXISTS idx_structural_edges_callee
                ON structural_edges(callee_symbol_id, callee_file_id);

            CREATE TABLE IF NOT EXISTS api_routes (
                id                  INTEGER PRIMARY KEY AUTOINCREMENT,
                method              TEXT NOT NULL,
                path_pattern        TEXT NOT NULL,
                handler_symbol_id   INTEGER REFERENCES project_symbols(id),
                handler_symbol_name TEXT,
                handler_file_id     INTEGER NOT NULL REFERENCES project_files(id),
                framework           TEXT NOT NULL,
                route_source        TEXT NOT NULL DEFAULT 'DECORATOR',
                mount_prefix        TEXT,
                is_dynamic          INTEGER DEFAULT 0,
                route_confidence    REAL NOT NULL DEFAULT 1.0,
                evidence            TEXT,
                last_indexed_at     TEXT NOT NULL,
                FOREIGN KEY (handler_symbol_id) REFERENCES project_symbols(id),
                FOREIGN KEY (handler_file_id) REFERENCES project_files(id)
            );
            CREATE INDEX IF NOT EXISTS idx_api_routes_handler
                ON api_routes(handler_symbol_id, handler_file_id);
            CREATE INDEX IF NOT EXISTS idx_api_routes_path
                ON api_routes(path_pattern);

            CREATE TABLE IF NOT EXISTS data_models (
                id              INTEGER PRIMARY KEY AUTOINCREMENT,
                model_name      TEXT NOT NULL,
                model_file_id   INTEGER NOT NULL REFERENCES project_files(id),
                language        TEXT NOT NULL,
                model_kind      TEXT NOT NULL DEFAULT 'STRUCT',
                confidence      REAL NOT NULL DEFAULT 1.0,
                evidence        TEXT,
                fields          TEXT,
                last_indexed_at TEXT NOT NULL,
                FOREIGN KEY (model_file_id) REFERENCES project_files(id)
            );
            CREATE INDEX IF NOT EXISTS idx_data_models_name
                ON data_models(model_name);
            CREATE INDEX IF NOT EXISTS idx_data_models_file
                ON data_models(model_file_id);

            CREATE TABLE IF NOT EXISTS symbol_centrality (
                id                      INTEGER PRIMARY KEY AUTOINCREMENT,
                symbol_id               INTEGER NOT NULL REFERENCES project_symbols(id),
                file_id                 INTEGER NOT NULL REFERENCES project_files(id),
                entrypoints_reachable   INTEGER NOT NULL DEFAULT 0,
                betweenness             REAL DEFAULT 0.0,
                last_computed_at        TEXT NOT NULL,
                FOREIGN KEY (symbol_id) REFERENCES project_symbols(id),
                FOREIGN KEY (file_id) REFERENCES project_files(id)
            );
            CREATE INDEX IF NOT EXISTS idx_symbol_centrality_symbol
                ON symbol_centrality(symbol_id);
            CREATE INDEX IF NOT EXISTS idx_symbol_centrality_file
                ON symbol_centrality(file_id);
            CREATE INDEX IF NOT EXISTS idx_symbol_centrality_reachable
                ON symbol_centrality(entrypoints_reachable);",
        ),
        M::up(
            "CREATE TABLE IF NOT EXISTS observability_patterns (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                file_id INTEGER NOT NULL REFERENCES project_files(id),
                line_start INTEGER,
                pattern_kind TEXT NOT NULL DEFAULT 'LOG',
                level TEXT,
                framework TEXT,
                confidence REAL NOT NULL DEFAULT 1.0,
                evidence TEXT,
                in_test INTEGER DEFAULT 0,
                last_indexed_at TEXT NOT NULL,
                FOREIGN KEY (file_id) REFERENCES project_files(id)
            );
            CREATE INDEX IF NOT EXISTS idx_obs_patterns_file ON observability_patterns(file_id);
            CREATE INDEX IF NOT EXISTS idx_obs_patterns_kind ON observability_patterns(pattern_kind);",
        ),
        M::up(
            "CREATE TABLE IF NOT EXISTS test_mapping (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                test_symbol_id INTEGER NOT NULL REFERENCES project_symbols(id),
                test_file_id INTEGER NOT NULL REFERENCES project_files(id),
                tested_symbol_id INTEGER REFERENCES project_symbols(id),
                tested_file_id INTEGER REFERENCES project_files(id),
                confidence REAL NOT NULL DEFAULT 1.0,
                mapping_kind TEXT NOT NULL DEFAULT 'IMPORT',
                evidence TEXT,
                last_indexed_at TEXT NOT NULL,
                FOREIGN KEY (test_symbol_id) REFERENCES project_symbols(id),
                FOREIGN KEY (test_file_id) REFERENCES project_files(id),
                FOREIGN KEY (tested_symbol_id) REFERENCES project_symbols(id),
                FOREIGN KEY (tested_file_id) REFERENCES project_files(id),
                UNIQUE(test_symbol_id, tested_symbol_id)
            );
            CREATE INDEX IF NOT EXISTS idx_test_mapping_tested ON test_mapping(tested_symbol_id);
            CREATE INDEX IF NOT EXISTS idx_test_mapping_test ON test_mapping(test_symbol_id);",
        ),
        M::up(
            "CREATE TABLE IF NOT EXISTS ci_gates (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                ci_file_id INTEGER NOT NULL REFERENCES project_files(id),
                platform TEXT NOT NULL,
                job_name TEXT NOT NULL,
                trigger TEXT,
                steps TEXT,
                last_indexed_at TEXT NOT NULL,
                FOREIGN KEY (ci_file_id) REFERENCES project_files(id)
            );
            CREATE INDEX IF NOT EXISTS idx_ci_gates_file ON ci_gates(ci_file_id);
            CREATE INDEX IF NOT EXISTS idx_ci_gates_platform ON ci_gates(platform);",
        ),
        M::up(
            "CREATE TABLE IF NOT EXISTS env_declarations (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                var_name TEXT NOT NULL,
                source_file_id INTEGER NOT NULL REFERENCES project_files(id),
                source_kind TEXT NOT NULL,
                required INTEGER DEFAULT 0,
                default_value_redacted TEXT,
                description TEXT,
                confidence REAL NOT NULL DEFAULT 1.0,
                last_indexed_at TEXT NOT NULL,
                UNIQUE(var_name, source_file_id, source_kind)
            );
            CREATE INDEX IF NOT EXISTS idx_env_declarations_var ON env_declarations(var_name);
            CREATE INDEX IF NOT EXISTS idx_env_declarations_file ON env_declarations(source_file_id);",
        ),
    ]
}
