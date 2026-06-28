use rusqlite_migration::M;

pub fn m31_ci_predict() -> Vec<M<'static>> {
    vec![M::up(
        "CREATE TABLE IF NOT EXISTS ci_outcome_history (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            diff_embedding_id INTEGER NOT NULL,
            ci_file_id INTEGER NOT NULL,
            job_name TEXT NOT NULL,
            platform TEXT NOT NULL,
            outcome TEXT NOT NULL,
            commit_hash TEXT NOT NULL,
            recorded_at TEXT NOT NULL DEFAULT (datetime('now')),
            FOREIGN KEY(diff_embedding_id) REFERENCES embeddings(id),
            FOREIGN KEY(ci_file_id) REFERENCES project_files(id)
        );",
    )]
}
