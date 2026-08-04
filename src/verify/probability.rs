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

/// Canonical step identity for Bayesian verify join (0140).
///
/// Whitespace is normalized first so argv drift (double spaces, flag order
/// padding) does not split history. Pattern order matters: nextest profiles
/// and scoped filtersets are matched before `nextest-default`.
///
/// **B4 residual:** `nextest-scoped` collapses *all* stem sets into one
/// bucket so Daily 5 can accumulate any scoped-nextest signal. Mixed stems
/// intentionally share Laplace history; a future track may hash stem sets.
pub fn verify_step_key(command: &str) -> String {
    let normalized = command.split_whitespace().collect::<Vec<_>>().join(" ");

    if is_cargo_fmt(&normalized) {
        return "cargo-fmt".to_string();
    }
    if is_cargo_clippy(&normalized) {
        return "cargo-clippy".to_string();
    }
    if is_cargo_nextest(&normalized) {
        // Token-boundary profile match only (avoids `--profile ci` ⊆ `--profile circus`).
        if contains_profile_token(&normalized, "ci") {
            return "nextest-ci".to_string();
        }
        if contains_profile_token(&normalized, "slow") {
            return "nextest-slow".to_string();
        }
        if contains_profile_token(&normalized, "compile-fail") {
            return "nextest-compile-fail".to_string();
        }
        // Scoped before default so -E / --filterset / test(…) share history.
        if is_nextest_scoped(&normalized) {
            return "nextest-scoped".to_string();
        }
        return "nextest-default".to_string();
    }
    if is_cargo_test(&normalized) {
        if normalized.contains("--doc") {
            return "doctest".to_string();
        }
        return "cargo-test".to_string();
    }
    if normalized.contains("git diff --cached --check") {
        return "git-diff-cached-check".to_string();
    }
    if normalized.contains("git diff --check") {
        return "git-diff-check".to_string();
    }

    // Unrecognized: full whitespace-normalized command (legacy / custom).
    normalized
}

fn is_cargo_fmt(cmd: &str) -> bool {
    cmd.contains("cargo fmt") || cmd.starts_with("cargo fmt")
}

fn is_cargo_clippy(cmd: &str) -> bool {
    cmd.contains("cargo clippy")
}

fn is_cargo_nextest(cmd: &str) -> bool {
    cmd.contains("cargo nextest")
}

fn is_cargo_test(cmd: &str) -> bool {
    // Avoid matching "cargo test" inside unrelated text; require cargo test as a token pair.
    cmd.contains("cargo test")
}

/// Match `--profile <name>` as a full token (not a prefix of another profile).
/// e.g. `--profile ci` must not match `--profile circus`.
fn contains_profile_token(cmd: &str, profile: &str) -> bool {
    let needle = format!("--profile {profile}");
    let Some(idx) = cmd.find(&needle) else {
        return false;
    };
    let after = idx + needle.len();
    if after == cmd.len() {
        return true;
    }
    // Next char must be whitespace (token boundary).
    matches!(cmd.as_bytes().get(after), Some(b' ' | b'\t'))
}

fn is_nextest_scoped(cmd: &str) -> bool {
    // Filterset forms used by build_scoped_nextest_command / operators.
    cmd.contains(" -E ")
        || cmd.contains(" -E'")
        || cmd.contains(" -E\"")
        || cmd.contains("--filterset")
        || cmd.contains("test(")
}

/// Band for multi-band Bayesian ordering: fmt=0, clippy=1, everything else=2.
pub fn verify_step_band(step_key: &str) -> u8 {
    match step_key {
        "cargo-fmt" => 0,
        "cargo-clippy" => 1,
        _ => 2,
    }
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

    // Deterministic window: recorded_at is second-resolution and batch inserts
    // share timestamps — tiebreak on diff_embedding_id so the "most recent 1000"
    // set does not drift run-to-run.
    let mut stmt = conn
        .prepare(
            "SELECT test_file,
                    COUNT(*) as runs,
                    SUM(CASE WHEN outcome = 'fail' THEN 1 ELSE 0 END) as fails
             FROM test_outcome_history
             WHERE diff_embedding_id IN (
                 SELECT DISTINCT diff_embedding_id FROM test_outcome_history
                 ORDER BY recorded_at DESC, diff_embedding_id DESC LIMIT 1000
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

    // Re-key by verify_step_key so legacy full-command rows merge with
    // already-keyed rows (sum total_runs and failures on collision).
    let mut dataset: HashMap<String, CommandStats> = HashMap::new();
    for (test_file, stats) in rows.flatten() {
        let key = verify_step_key(&test_file);
        dataset
            .entry(key)
            .and_modify(|existing| {
                existing.total_runs += stats.total_runs;
                existing.failures += stats.failures;
            })
            .or_insert(stats);
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
        )
        .unwrap();
    }

    #[test]
    fn test_verify_step_key_cargo_fmt() {
        assert_eq!(verify_step_key("cargo fmt --all -- --check"), "cargo-fmt");
        assert_eq!(verify_step_key("cargo fmt"), "cargo-fmt");
    }

    #[test]
    fn test_verify_step_key_cargo_clippy() {
        assert_eq!(
            verify_step_key("cargo clippy --all-targets --all-features -- -D warnings"),
            "cargo-clippy"
        );
    }

    #[test]
    fn test_verify_step_key_nextest_ci() {
        assert_eq!(
            verify_step_key("cargo nextest run --workspace --all-features --profile ci"),
            "nextest-ci"
        );
        // Token boundary: "ci" must not match as a prefix of "circus".
        assert_eq!(
            verify_step_key("cargo nextest run --workspace --all-features --profile circus"),
            "nextest-default"
        );
    }

    #[test]
    fn test_verify_step_key_nextest_slow() {
        assert_eq!(
            verify_step_key("cargo nextest run --workspace --all-features --profile slow"),
            "nextest-slow"
        );
    }

    #[test]
    fn test_verify_step_key_nextest_compile_fail() {
        assert_eq!(
            verify_step_key("cargo nextest run --workspace --all-features --profile compile-fail"),
            "nextest-compile-fail"
        );
    }

    #[test]
    fn test_verify_step_key_nextest_scoped_two_filters_same_key() {
        let a = "cargo nextest run --workspace --all-features -E 'test(cli_scan)'";
        let b = "cargo nextest run --workspace --all-features -E 'test(cli_scan) + test(dead_code_prune)'";
        assert_eq!(verify_step_key(a), "nextest-scoped");
        assert_eq!(verify_step_key(b), "nextest-scoped");
        assert_eq!(verify_step_key(a), verify_step_key(b));
    }

    #[test]
    fn test_verify_step_key_nextest_scoped_filterset_flag() {
        assert_eq!(
            verify_step_key("cargo nextest run --workspace --filterset 'test(foo)'"),
            "nextest-scoped"
        );
    }

    #[test]
    fn test_verify_step_key_nextest_default() {
        assert_eq!(
            verify_step_key("cargo nextest run --workspace --all-features"),
            "nextest-default"
        );
    }

    #[test]
    fn test_verify_step_key_doctest() {
        assert_eq!(
            verify_step_key("cargo test --workspace --all-features --doc"),
            "doctest"
        );
    }

    #[test]
    fn test_verify_step_key_cargo_test() {
        assert_eq!(
            verify_step_key("cargo test --workspace --all-features"),
            "cargo-test"
        );
    }

    #[test]
    fn test_verify_step_key_git_diff_check() {
        assert_eq!(verify_step_key("git diff --check"), "git-diff-check");
    }

    #[test]
    fn test_verify_step_key_git_diff_cached_check() {
        assert_eq!(
            verify_step_key("git diff --cached --check"),
            "git-diff-cached-check"
        );
    }

    #[test]
    fn test_verify_step_key_fallback_normalized() {
        let raw = "my-custom-tool   --flag   value";
        assert_eq!(verify_step_key(raw), "my-custom-tool --flag value");
    }

    #[test]
    fn test_verify_step_key_double_space_same_key() {
        let single = "cargo fmt --all -- --check";
        let double = "cargo  fmt  --all  --  --check";
        assert_eq!(verify_step_key(single), verify_step_key(double));
        assert_eq!(verify_step_key(double), "cargo-fmt");
    }

    #[test]
    fn test_verify_step_band() {
        assert_eq!(verify_step_band("cargo-fmt"), 0);
        assert_eq!(verify_step_band("cargo-clippy"), 1);
        assert_eq!(verify_step_band("nextest-ci"), 2);
        assert_eq!(verify_step_band("nextest-scoped"), 2);
        assert_eq!(verify_step_band("unknown"), 2);
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
            )
            .unwrap();
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
            )
            .unwrap();
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
            )
            .unwrap();
        }
        for i in 9..=10 {
            insert_dummy_embedding(&conn, i);
            conn.execute(
                "INSERT INTO test_outcome_history (diff_embedding_id, test_file, outcome, commit_hash) VALUES (?1, 'cmd_a', 'fail', 'hash')",
                rusqlite::params![i],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO test_outcome_history (diff_embedding_id, test_file, outcome, commit_hash) VALUES (?1, 'cmd_b', 'fail', 'hash')",
                rusqlite::params![i],
            )
            .unwrap();
        }

        let dataset = extract_dataset(&conn).unwrap();
        // Unrecognized commands keep normalized full string as key.
        assert_eq!(dataset.len(), 2);
        assert_eq!(dataset["cmd_a"].total_runs, 10);
        assert_eq!(dataset["cmd_a"].failures, 2);
        assert_eq!(dataset["cmd_b"].total_runs, 2);
        assert_eq!(dataset["cmd_b"].failures, 2);
    }

    /// Legacy full-command fmt rows + new step-key rows merge into one stats bucket.
    #[test]
    fn test_extract_dataset_merges_legacy_and_step_key_fmt() {
        let conn = setup_db();
        let legacy_fmt = "cargo fmt --all -- --check";
        let key_fmt = "cargo-fmt";

        // 8 pass under legacy full command
        for i in 1..=8 {
            insert_dummy_embedding(&conn, i);
            conn.execute(
                "INSERT INTO test_outcome_history (diff_embedding_id, test_file, outcome, commit_hash) VALUES (?1, ?2, 'pass', 'hash')",
                rusqlite::params![i, legacy_fmt],
            )
            .unwrap();
        }
        // 1 fail under legacy, 1 fail under new key → 2 fails / 10 runs for cargo-fmt
        insert_dummy_embedding(&conn, 9);
        conn.execute(
            "INSERT INTO test_outcome_history (diff_embedding_id, test_file, outcome, commit_hash) VALUES (?1, ?2, 'fail', 'hash')",
            rusqlite::params![9, legacy_fmt],
        )
        .unwrap();
        insert_dummy_embedding(&conn, 10);
        conn.execute(
            "INSERT INTO test_outcome_history (diff_embedding_id, test_file, outcome, commit_hash) VALUES (?1, ?2, 'fail', 'hash')",
            rusqlite::params![10, key_fmt],
        )
        .unwrap();

        let dataset = extract_dataset(&conn).unwrap();
        assert_eq!(dataset.len(), 1);
        let stats = dataset.get("cargo-fmt").expect("merged cargo-fmt key");
        assert_eq!(stats.total_runs, 10);
        assert_eq!(stats.failures, 2);
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
