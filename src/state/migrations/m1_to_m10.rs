use rusqlite_migration::M;

pub fn m1_to_m10() -> Vec<M<'static>> {
    vec![
        M::up(
            "CREATE TABLE snapshots (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                timestamp TEXT NOT NULL,
                head_hash TEXT,
                branch_name TEXT,
                is_clean INTEGER NOT NULL,
                packet_json TEXT NOT NULL
            );",
        ),
        M::up(
            "CREATE TABLE batches (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                timestamp TEXT NOT NULL,
                event_count INTEGER NOT NULL,
                batch_json TEXT NOT NULL
            );",
        ),
        M::up(
            "CREATE TABLE changed_files (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                snapshot_id INTEGER REFERENCES snapshots(id),
                path TEXT NOT NULL,
                status TEXT NOT NULL,
                is_staged INTEGER NOT NULL
            );",
        ),
        M::up(
            "CREATE TABLE verification_runs (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                timestamp TEXT NOT NULL,
                plan_json TEXT,
                overall_pass INTEGER NOT NULL
            );",
        ),
        M::up(
            "CREATE TABLE verification_results (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                run_id INTEGER REFERENCES verification_runs(id),
                command TEXT NOT NULL,
                exit_code INTEGER NOT NULL,
                duration_ms INTEGER NOT NULL,
                truncated INTEGER NOT NULL
            );",
        ),
        M::up(
            "CREATE TABLE symbols (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                snapshot_id INTEGER REFERENCES snapshots(id),
                file_path TEXT NOT NULL,
                symbol_name TEXT NOT NULL,
                symbol_kind TEXT NOT NULL,
                is_public INTEGER NOT NULL
            );",
        ),
        M::up("ALTER TABLE symbols ADD COLUMN cognitive_complexity INTEGER;"),
        M::up("ALTER TABLE symbols ADD COLUMN cyclomatic_complexity INTEGER;"),
        M::up(
            "CREATE TABLE federated_links (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                sibling_name TEXT NOT NULL UNIQUE,
                sibling_path TEXT NOT NULL,
                last_scanned_at TEXT NOT NULL
            );",
        ),
        M::up(
            "CREATE TABLE federated_dependencies (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                local_symbol TEXT NOT NULL,
                sibling_name TEXT NOT NULL,
                sibling_symbol TEXT NOT NULL,
                FOREIGN KEY (sibling_name) REFERENCES federated_links(sibling_name)
            );",
        ),
    ]
}
