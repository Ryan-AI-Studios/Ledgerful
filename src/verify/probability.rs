use rusqlite::Connection;
use std::collections::HashMap;

#[derive(Debug)]
pub enum ProbabilityError {
    ColdStart(i64),
    InsufficientVariance,
    DatabaseError(String),
}

impl std::fmt::Display for ProbabilityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProbabilityError::ColdStart(runs) => write!(
                f,
                "Probabilistic verification ordering requires at least 10 historical runs (found: {}). Using sequential ordering.",
                runs
            ),
            ProbabilityError::InsufficientVariance => write!(
                f,
                "Insufficient variance in test history (0 failures). Using sequential ordering."
            ),
            ProbabilityError::DatabaseError(msg) => write!(f, "Database error: {}", msg),
        }
    }
}

impl std::error::Error for ProbabilityError {}

#[derive(Debug)]
pub struct CommandStats {
    pub total_runs: i64,
    pub failures: i64,
}

pub fn extract_dataset(
    conn: &Connection,
) -> Result<HashMap<String, CommandStats>, ProbabilityError> {
    let total_runs: i64 = conn
        .query_row(
            "SELECT COUNT(DISTINCT diff_embedding_id) FROM test_outcome_history",
            [],
            |row| row.get(0),
        )
        .map_err(|e| ProbabilityError::DatabaseError(e.to_string()))?;

    if total_runs < 10 {
        return Err(ProbabilityError::ColdStart(total_runs));
    }

    let total_failures: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM test_outcome_history WHERE outcome = 'fail'",
            [],
            |row| row.get(0),
        )
        .map_err(|e| ProbabilityError::DatabaseError(e.to_string()))?;

    if total_failures == 0 {
        return Err(ProbabilityError::InsufficientVariance);
    }

    let mut stmt = conn
        .prepare(
            "SELECT test_file,
                    COUNT(*) as runs,
                    SUM(CASE WHEN outcome = 'fail' THEN 1 ELSE 0 END) as fails
             FROM test_outcome_history
             WHERE diff_embedding_id IN (
                 SELECT DISTINCT diff_embedding_id FROM test_outcome_history
                 ORDER BY recorded_at DESC LIMIT 1000
             )
             GROUP BY test_file",
        )
        .map_err(|e| ProbabilityError::DatabaseError(e.to_string()))?;

    let rows = stmt
        .query_map([], |row| {
            let test_file: String = row.get(0)?;
            let runs: i64 = row.get(1)?;
            let fails: i64 = row.get(2)?;
            Ok((
                test_file,
                CommandStats {
                    total_runs: runs,
                    failures: fails,
                },
            ))
        })
        .map_err(|e| ProbabilityError::DatabaseError(e.to_string()))?;

    let mut dataset = HashMap::new();
    for (cmd, stats) in rows.flatten() {
        dataset.insert(cmd, stats);
    }

    Ok(dataset)
}

pub fn calculate_probabilities(dataset: &HashMap<String, CommandStats>) -> HashMap<String, f64> {
    let alpha = 1.0;
    let num_classes = 2.0;

    let mut probs = HashMap::new();
    for (cmd, stats) in dataset {
        let p = (stats.failures as f64 + alpha) / (stats.total_runs as f64 + alpha * num_classes);
        probs.insert(cmd.clone(), p);
    }
    probs
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::migrations::get_migrations;

    fn setup_db() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        get_migrations().to_latest(&mut conn).unwrap();
        conn
    }

    fn insert_dummy_embedding(conn: &Connection, id: i64) {
        conn.execute(
            "INSERT INTO embeddings (id, entity_type, entity_id, content_hash, model_name, dimensions, vector)
             VALUES (?1, 'test_diff', ?2, 'hash', 'model', 3, x'00000000')",
            rusqlite::params![id, id.to_string()],
        ).unwrap();
    }

    #[test]
    fn test_extract_dataset_cold_start() {
        let conn = setup_db();
        // Insert only 9 runs
        for i in 1..=9 {
            insert_dummy_embedding(&conn, i);
            conn.execute(
                "INSERT INTO test_outcome_history (diff_embedding_id, test_file, outcome, commit_hash) VALUES (?1, 'cmd', 'pass', 'hash')",
                rusqlite::params![i],
            ).unwrap();
        }

        let err = extract_dataset(&conn).unwrap_err();
        match err {
            ProbabilityError::ColdStart(runs) => assert_eq!(runs, 9),
            _ => panic!("Expected ColdStart error"),
        }
    }

    #[test]
    fn test_extract_dataset_insufficient_variance() {
        let conn = setup_db();
        // Insert 10 runs, all pass
        for i in 1..=10 {
            insert_dummy_embedding(&conn, i);
            conn.execute(
                "INSERT INTO test_outcome_history (diff_embedding_id, test_file, outcome, commit_hash) VALUES (?1, 'cmd', 'pass', 'hash')",
                rusqlite::params![i],
            ).unwrap();
        }

        let err = extract_dataset(&conn).unwrap_err();
        match err {
            ProbabilityError::InsufficientVariance => {}
            _ => panic!("Expected InsufficientVariance error"),
        }
    }

    #[test]
    fn test_extract_dataset_success() {
        let conn = setup_db();
        for i in 1..=8 {
            insert_dummy_embedding(&conn, i);
            conn.execute(
                "INSERT INTO test_outcome_history (diff_embedding_id, test_file, outcome, commit_hash) VALUES (?1, 'cmd_a', 'pass', 'hash')",
                rusqlite::params![i],
            ).unwrap();
        }
        for i in 9..=10 {
            insert_dummy_embedding(&conn, i);
            conn.execute(
                "INSERT INTO test_outcome_history (diff_embedding_id, test_file, outcome, commit_hash) VALUES (?1, 'cmd_a', 'fail', 'hash')",
                rusqlite::params![i],
            ).unwrap();
            conn.execute(
                "INSERT INTO test_outcome_history (diff_embedding_id, test_file, outcome, commit_hash) VALUES (?1, 'cmd_b', 'fail', 'hash')",
                rusqlite::params![i],
            ).unwrap();
        }

        let dataset = extract_dataset(&conn).unwrap();
        assert_eq!(dataset.len(), 2);
        assert_eq!(dataset["cmd_a"].total_runs, 10);
        assert_eq!(dataset["cmd_a"].failures, 2);
        assert_eq!(dataset["cmd_b"].total_runs, 2);
        assert_eq!(dataset["cmd_b"].failures, 2);
    }

    #[test]
    fn test_calculate_probabilities() {
        let mut dataset = HashMap::new();
        dataset.insert(
            "cmd_a".to_string(),
            CommandStats {
                total_runs: 10,
                failures: 2,
            },
        );
        dataset.insert(
            "cmd_b".to_string(),
            CommandStats {
                total_runs: 2,
                failures: 2,
            },
        );

        let probs = calculate_probabilities(&dataset);

        // Laplace smoothing: P = (failures + 1) / (runs + 2)
        // cmd_a: (2 + 1) / (10 + 2) = 3 / 12 = 0.25
        assert!((probs["cmd_a"] - 0.25).abs() < 1e-6);

        // cmd_b: (2 + 1) / (2 + 2) = 3 / 4 = 0.75
        assert!((probs["cmd_b"] - 0.75).abs() < 1e-6);
    }
}
