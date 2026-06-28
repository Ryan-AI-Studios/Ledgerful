use crate::config::model::LocalModelConfig;
use crate::embed::client::embed_long_text;
use crate::embed::embed_and_store;
use crate::embed::similarity::cosine_sim;
use crate::impact::packet::ImpactPacket;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tracing::{debug, warn};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum TestStatus {
    Passed,
    Failed,
    Skipped,
}

impl TestStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            TestStatus::Passed => "pass",
            TestStatus::Failed => "fail",
            TestStatus::Skipped => "skip",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestOutcome {
    pub test_name: String,
    pub test_file: String,
    pub commit_hash: String,
    pub status: TestStatus,
    pub duration_ms: u64,
    pub diff_summary: String,
}

pub fn build_diff_text(packet: &ImpactPacket) -> String {
    let mut items: Vec<String> = Vec::new();

    for file in &packet.changes {
        items.push(file.path.to_string_lossy().to_string());
        if let Some(ref symbols) = file.symbols {
            for sym in symbols {
                items.push(sym.name.clone());
            }
        }
    }

    items.truncate(200);
    items.join(" ")
}

pub fn record_test_outcomes(
    conn: &Connection,
    embed_config: &LocalModelConfig,
    outcomes: &[TestOutcome],
    diff_text: &str,
) -> Result<usize, String> {
    if embed_config.base_url.is_empty() {
        warn!("Semantic prediction: base_url is empty; skipping outcome recording");
        return Ok(0);
    }

    if diff_text.is_empty() {
        debug!("Semantic prediction: diff_text is empty; skipping outcome recording");
        return Ok(0);
    }

    if outcomes.is_empty() {
        return Ok(0);
    }

    let commit_hash = &outcomes[0].commit_hash;

    embed_and_store(embed_config, conn, "test_diff", commit_hash, diff_text)?;

    let embedding_id: i64 = conn
        .query_row(
            "SELECT id FROM embeddings WHERE entity_type = 'test_diff' AND entity_id = ?1 AND model_name = ?2",
            rusqlite::params![commit_hash, embed_config.embedding_model],
            |row| row.get(0),
        )
        .map_err(|e| e.to_string())?;

    for outcome in outcomes {
        conn.execute(
            "INSERT INTO test_outcome_history (diff_embedding_id, test_file, outcome, commit_hash) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![embedding_id, outcome.test_file, outcome.status.as_str(), outcome.commit_hash],
        )
        .map_err(|e| e.to_string())?;
    }

    Ok(outcomes.len())
}

/// Load diff embeddings with their database IDs, entity IDs, and vectors.
fn load_diff_embeddings(
    conn: &Connection,
    model_name: &str,
) -> Result<Vec<(i64, String, Vec<f32>)>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT id, entity_id, vector FROM embeddings WHERE entity_type = 'test_diff' AND model_name = ?1",
        )
        .map_err(|e| e.to_string())?;

    let rows = stmt
        .query_map(rusqlite::params![model_name], |row| {
            let id: i64 = row.get(0)?;
            let entity_id: String = row.get(1)?;
            let blob: Vec<u8> = row.get(2)?;
            Ok((id, entity_id, blob))
        })
        .map_err(|e| e.to_string())?;

    let mut results = Vec::new();
    for row in rows {
        let (id, entity_id, blob) = row.map_err(|e| e.to_string())?;
        if blob.len() % 4 != 0 {
            continue;
        }
        let floats: Vec<f32> = blob
            .chunks_exact(4)
            .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
            .collect();
        results.push((id, entity_id, floats));
    }

    Ok(results)
}

/// Query past test outcomes by embedding the current diff text and finding the
/// most similar stored diff embeddings. Returns (TestOutcome, similarity_score)
/// pairs for the top_k most similar historical diffs.
pub fn query_similar_outcomes(
    conn: &Connection,
    embed_config: &LocalModelConfig,
    diff_text: &str,
    top_k: usize,
) -> Result<Vec<(TestOutcome, f32)>, String> {
    if embed_config.base_url.is_empty() {
        return Ok(Vec::new());
    }

    if diff_text.is_empty() {
        return Ok(Vec::new());
    }

    let diff_embeddings = load_diff_embeddings(conn, &embed_config.embedding_model)?;

    if diff_embeddings.is_empty() {
        return Ok(Vec::new());
    }

    let query_vec = embed_long_text(embed_config, diff_text)?;

    let mut scored: Vec<(i64, f32)> = diff_embeddings
        .iter()
        .filter_map(|(id, _entity_id, vec)| {
            cosine_sim(&query_vec, vec).ok().map(|score| (*id, score))
        })
        .collect();

    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    if top_k < scored.len() {
        scored.truncate(top_k);
    }

    let mut results = Vec::new();
    for (embedding_id, similarity) in &scored {
        let mut stmt = conn
            .prepare(
                "SELECT test_file, outcome, commit_hash FROM test_outcome_history WHERE diff_embedding_id = ?1",
            )
            .map_err(|e| e.to_string())?;

        let outcome_rows = stmt
            .query_map(rusqlite::params![embedding_id], |row| {
                let test_file: String = row.get(0)?;
                let outcome: String = row.get(1)?;
                let commit_hash: Option<String> = row.get(2)?;
                Ok((test_file, outcome, commit_hash))
            })
            .map_err(|e| e.to_string())?;

        for outcome_row in outcome_rows {
            let (test_file, outcome_str, commit_hash) = outcome_row.map_err(|e| e.to_string())?;
            let status = match outcome_str.as_str() {
                "pass" => TestStatus::Passed,
                "fail" => TestStatus::Failed,
                _ => TestStatus::Skipped,
            };
            results.push((
                TestOutcome {
                    test_name: String::new(),
                    test_file,
                    commit_hash: commit_hash.unwrap_or_default(),
                    status,
                    duration_ms: 0,
                    diff_summary: String::new(),
                },
                *similarity,
            ));
        }
    }

    Ok(results)
}

/// Compute semantic scores from similar outcomes by grouping by test_file
/// and averaging the similarity scores. Higher average → higher semantic score.
pub fn compute_semantic_scores(similar_outcomes: &[(TestOutcome, f32)]) -> HashMap<String, f64> {
    let mut file_scores: HashMap<String, (f64, usize)> = HashMap::new();

    for (outcome, sim) in similar_outcomes {
        let entry = file_scores
            .entry(outcome.test_file.clone())
            .or_insert((0.0, 0));
        entry.0 += *sim as f64;
        entry.1 += 1;
    }

    file_scores
        .into_iter()
        .map(|(file, (sum, count))| (file, sum / count as f64))
        .collect()
}

/// Blend rule-based scores with semantic scores using the given weight.
/// - weight = 0.0 → returns rule_scores unchanged
/// - semantic_scores is empty → returns rule_scores unchanged
/// - Tests only in semantic_scores (not in rule_scores) get rule_score = 0.0
/// - Tests only in rule_scores (not in semantic_scores) get semantic_score = 0.0
pub fn blend_scores(
    rule_scores: &HashMap<String, f64>,
    semantic_scores: &HashMap<String, f64>,
    weight: f64,
) -> HashMap<String, f64> {
    if weight == 0.0 || semantic_scores.is_empty() {
        return rule_scores.clone();
    }

    let mut combined: HashMap<String, f64> = HashMap::new();

    for (file, rule) in rule_scores {
        let semantic = semantic_scores.get(file).copied().unwrap_or(0.0);
        combined.insert(file.clone(), (1.0 - weight) * rule + weight * semantic);
    }

    for (file, semantic) in semantic_scores {
        if !combined.contains_key(file) {
            combined.insert(file.clone(), weight * semantic);
        }
    }

    combined
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::impact::packet::ChangedFile;
    use crate::index::symbols::{Symbol, SymbolKind};
    use crate::state::migrations::get_migrations;
    use httpmock::prelude::*;
    use std::path::PathBuf;

    fn setup_db() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        let migrations = get_migrations();
        migrations.to_latest(&mut conn).unwrap();
        conn
    }

    fn make_packet() -> ImpactPacket {
        ImpactPacket {
            head_hash: Some("abc123def".to_string()),
            changes: vec![
                ChangedFile {
                    path: PathBuf::from("src/main.rs"),
                    status: "Modified".to_string(),
                    symbols: Some(vec![
                        Symbol {
                            name: "main".to_string(),
                            kind: SymbolKind::Function,
                            is_public: true,
                            cognitive_complexity: None,
                            cyclomatic_complexity: None,
                            line_start: None,
                            line_end: None,
                            qualified_name: None,
                            byte_start: None,
                            byte_end: None,
                            entrypoint_kind: None,
                            metadata: std::collections::BTreeMap::new(),
                        },
                        Symbol {
                            name: "helper".to_string(),
                            kind: SymbolKind::Function,
                            is_public: false,
                            cognitive_complexity: None,
                            cyclomatic_complexity: None,
                            line_start: None,
                            line_end: None,
                            qualified_name: None,
                            byte_start: None,
                            byte_end: None,
                            entrypoint_kind: None,
                            metadata: std::collections::BTreeMap::new(),
                        },
                    ]),
                    ..ChangedFile::default()
                },
                ChangedFile {
                    path: PathBuf::from("src/lib.rs"),
                    status: "Modified".to_string(),
                    symbols: Some(vec![
                        Symbol {
                            name: "init".to_string(),
                            kind: SymbolKind::Function,
                            is_public: true,
                            cognitive_complexity: None,
                            cyclomatic_complexity: None,
                            line_start: None,
                            line_end: None,
                            qualified_name: None,
                            byte_start: None,
                            byte_end: None,
                            entrypoint_kind: None,
                            metadata: std::collections::BTreeMap::new(),
                        },
                        Symbol {
                            name: "run".to_string(),
                            kind: SymbolKind::Function,
                            is_public: false,
                            cognitive_complexity: None,
                            cyclomatic_complexity: None,
                            line_start: None,
                            line_end: None,
                            qualified_name: None,
                            byte_start: None,
                            byte_end: None,
                            entrypoint_kind: None,
                            metadata: std::collections::BTreeMap::new(),
                        },
                    ]),
                    ..ChangedFile::default()
                },
                ChangedFile {
                    path: PathBuf::from("src/utils.rs"),
                    status: "Modified".to_string(),
                    symbols: Some(vec![Symbol {
                        name: "normalize".to_string(),
                        kind: SymbolKind::Function,
                        is_public: true,
                        cognitive_complexity: None,
                        cyclomatic_complexity: None,
                        line_start: None,
                        line_end: None,
                        qualified_name: None,
                        byte_start: None,
                        byte_end: None,
                        entrypoint_kind: None,
                        metadata: std::collections::BTreeMap::new(),
                    }]),
                    ..ChangedFile::default()
                },
            ],
            ..ImpactPacket::default()
        }
    }

    #[test]
    fn test_status_as_str() {
        assert_eq!(TestStatus::Passed.as_str(), "pass");
        assert_eq!(TestStatus::Failed.as_str(), "fail");
        assert_eq!(TestStatus::Skipped.as_str(), "skip");
    }

    #[test]
    fn build_diff_text_with_changes_contains_paths_and_symbols() {
        let packet = make_packet();
        let text = build_diff_text(&packet);
        assert!(text.contains("src/main.rs"));
        assert!(text.contains("src/lib.rs"));
        assert!(text.contains("src/utils.rs"));
        assert!(text.contains("main"));
        assert!(text.contains("helper"));
        assert!(text.contains("init"));
        assert!(text.contains("run"));
        assert!(text.contains("normalize"));
    }

    #[test]
    fn build_diff_text_empty_packet_returns_empty_string() {
        let packet = ImpactPacket::default();
        let text = build_diff_text(&packet);
        assert_eq!(text, "");
    }

    #[test]
    fn record_test_outcomes_stores_outcomes_in_db() {
        let conn = setup_db();
        let server = MockServer::start();

        server.mock(|when, then| {
            when.method(httpmock::Method::POST).path("/v1/embeddings");
            then.status(200)
                .header("Content-Type", "application/json")
                .json_body(serde_json::json!({
                    "data": [{"embedding": [0.1, 0.2, 0.3]}]
                }));
        });

        let config = LocalModelConfig {
            base_url: server.base_url(),
            embedding_url: None,
            generation_url: None,
            embedding_model: "test-model".to_string(),
            dimensions: 3,
            context_window: 8192,
            timeout_secs: 30,
            ..LocalModelConfig::default()
        };

        let outcomes = vec![
            TestOutcome {
                test_name: "cargo test".to_string(),
                test_file: "tests/test_a.rs".to_string(),
                commit_hash: "abc123".to_string(),
                status: TestStatus::Passed,
                duration_ms: 150,
                diff_summary: "changed src/lib.rs".to_string(),
            },
            TestOutcome {
                test_name: "cargo test".to_string(),
                test_file: "tests/test_b.rs".to_string(),
                commit_hash: "abc123".to_string(),
                status: TestStatus::Failed,
                duration_ms: 200,
                diff_summary: "changed src/lib.rs".to_string(),
            },
            TestOutcome {
                test_name: "cargo clippy".to_string(),
                test_file: "lint".to_string(),
                commit_hash: "abc123".to_string(),
                status: TestStatus::Passed,
                duration_ms: 50,
                diff_summary: "changed src/lib.rs".to_string(),
            },
        ];

        let count = record_test_outcomes(&conn, &config, &outcomes, "changed src/lib.rs").unwrap();
        assert_eq!(count, 3);

        let row_count: i64 = conn
            .query_row("SELECT count(*) FROM test_outcome_history", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(row_count, 3);

        let pass_count: i64 = conn
            .query_row(
                "SELECT count(*) FROM test_outcome_history WHERE outcome = 'pass'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(pass_count, 2);

        let fail_count: i64 = conn
            .query_row(
                "SELECT count(*) FROM test_outcome_history WHERE outcome = 'fail'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(fail_count, 1);
    }

    #[test]
    fn record_test_outcomes_returns_zero_when_base_url_empty() {
        let conn = setup_db();
        let config = LocalModelConfig::default();
        let outcomes = vec![TestOutcome {
            test_name: "cargo test".to_string(),
            test_file: "tests/test.rs".to_string(),
            commit_hash: "abc".to_string(),
            status: TestStatus::Passed,
            duration_ms: 100,
            diff_summary: String::new(),
        }];

        let count = record_test_outcomes(&conn, &config, &outcomes, "some diff").unwrap();
        assert_eq!(count, 0);

        let row_count: i64 = conn
            .query_row("SELECT count(*) FROM test_outcome_history", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(row_count, 0);
    }

    #[test]
    fn record_test_outcomes_skips_when_diff_text_empty() {
        let conn = setup_db();
        let config = LocalModelConfig {
            base_url: "http://localhost:9999".to_string(),
            embedding_url: None,
            generation_url: None,
            embedding_model: "test-model".to_string(),
            dimensions: 3,
            ..LocalModelConfig::default()
        };
        let outcomes = vec![TestOutcome {
            test_name: "cargo test".to_string(),
            test_file: "tests/test.rs".to_string(),
            commit_hash: "abc".to_string(),
            status: TestStatus::Passed,
            duration_ms: 100,
            diff_summary: String::new(),
        }];

        let count = record_test_outcomes(&conn, &config, &outcomes, "").unwrap();
        assert_eq!(count, 0);

        let row_count: i64 = conn
            .query_row("SELECT count(*) FROM test_outcome_history", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(row_count, 0);
    }

    #[test]
    fn query_similar_outcomes_empty_when_no_data() {
        let conn = setup_db();
        let config = LocalModelConfig {
            base_url: "http://localhost:9999".to_string(),
            embedding_url: None,
            generation_url: None,
            embedding_model: "test-model".to_string(),
            dimensions: 3,
            ..LocalModelConfig::default()
        };
        let result = query_similar_outcomes(&conn, &config, "some diff", 30).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn query_similar_outcomes_empty_when_base_url_empty() {
        let conn = setup_db();
        let config = LocalModelConfig::default();
        let result = query_similar_outcomes(&conn, &config, "some diff", 30).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn query_similar_outcomes_empty_when_diff_text_empty() {
        let conn = setup_db();
        let config = LocalModelConfig {
            base_url: "http://localhost:9999".to_string(),
            embedding_url: None,
            generation_url: None,
            embedding_model: "test-model".to_string(),
            dimensions: 3,
            ..LocalModelConfig::default()
        };
        let result = query_similar_outcomes(&conn, &config, "", 30).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn query_similar_outcomes_returns_matching_outcomes() {
        let conn = setup_db();
        let server = MockServer::start();

        server.mock(|when, then| {
            when.method(httpmock::Method::POST).path("/v1/embeddings");
            then.status(200)
                .header("Content-Type", "application/json")
                .json_body(serde_json::json!({
                    "data": [{"embedding": [1.0, 0.0, 0.0]}]
                }));
        });

        let config = LocalModelConfig {
            base_url: server.base_url(),
            embedding_url: None,
            generation_url: None,
            embedding_model: "test-model".to_string(),
            dimensions: 3,
            context_window: 8192,
            timeout_secs: 30,
            ..LocalModelConfig::default()
        };

        // Insert a stored diff embedding with known vector
        let stored_vec: Vec<f32> = vec![1.0, 0.0, 0.0];
        let stored_blob: Vec<u8> = stored_vec.iter().flat_map(|f| f.to_le_bytes()).collect();
        conn.execute(
            "INSERT INTO embeddings (entity_type, entity_id, content_hash, model_name, dimensions, vector)
             VALUES ('test_diff', 'commit-1', 'hash1', 'test-model', 3, ?1)",
            rusqlite::params![stored_blob],
        )
        .unwrap();
        let embedding_id: i64 = conn.last_insert_rowid();

        // Insert test outcomes for that diff
        conn.execute(
            "INSERT INTO test_outcome_history (diff_embedding_id, test_file, outcome, commit_hash)
             VALUES (?1, 'tests/test_a.rs', 'fail', 'commit-1')",
            rusqlite::params![embedding_id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO test_outcome_history (diff_embedding_id, test_file, outcome, commit_hash)
             VALUES (?1, 'tests/test_b.rs', 'pass', 'commit-1')",
            rusqlite::params![embedding_id],
        )
        .unwrap();

        let result = query_similar_outcomes(&conn, &config, "similar change", 30).unwrap();
        assert_eq!(result.len(), 2);

        let test_files: Vec<&str> = result.iter().map(|(o, _)| o.test_file.as_str()).collect();
        assert!(test_files.contains(&"tests/test_a.rs"));
        assert!(test_files.contains(&"tests/test_b.rs"));

        // Similarity should be close to 1.0 since vectors are identical
        for (_outcome, sim) in &result {
            assert!((*sim - 1.0).abs() < 1e-4);
        }
    }

    #[test]
    fn compute_semantic_scores_groups_and_averages() {
        let outcomes = vec![
            (
                TestOutcome {
                    test_name: String::new(),
                    test_file: "tests/foo.rs".to_string(),
                    commit_hash: "abc".to_string(),
                    status: TestStatus::Failed,
                    duration_ms: 0,
                    diff_summary: String::new(),
                },
                0.9_f32,
            ),
            (
                TestOutcome {
                    test_name: String::new(),
                    test_file: "tests/foo.rs".to_string(),
                    commit_hash: "def".to_string(),
                    status: TestStatus::Passed,
                    duration_ms: 0,
                    diff_summary: String::new(),
                },
                0.7_f32,
            ),
            (
                TestOutcome {
                    test_name: String::new(),
                    test_file: "tests/bar.rs".to_string(),
                    commit_hash: "abc".to_string(),
                    status: TestStatus::Failed,
                    duration_ms: 0,
                    diff_summary: String::new(),
                },
                0.5_f32,
            ),
        ];

        let scores = compute_semantic_scores(&outcomes);

        assert!((scores.get("tests/foo.rs").copied().unwrap_or(0.0) - 0.8).abs() < 1e-6);
        assert!((scores.get("tests/bar.rs").copied().unwrap_or(0.0) - 0.5).abs() < 1e-6);
        // foo.rs has higher average (0.8) than bar.rs (0.5)
        assert!(
            scores.get("tests/foo.rs").copied().unwrap_or(0.0)
                > scores.get("tests/bar.rs").copied().unwrap_or(1.0)
        );
    }

    #[test]
    fn compute_semantic_scores_empty_input() {
        let scores = compute_semantic_scores(&[]);
        assert!(scores.is_empty());
    }

    #[test]
    fn blend_scores_weight_zero_returns_rule_only() {
        let rule: HashMap<String, f64> = [
            ("tests/a.rs".to_string(), 0.8),
            ("tests/b.rs".to_string(), 0.6),
        ]
        .into();
        let semantic: HashMap<String, f64> = [
            ("tests/a.rs".to_string(), 0.9),
            ("tests/c.rs".to_string(), 0.7),
        ]
        .into();

        let result = blend_scores(&rule, &semantic, 0.0);
        assert_eq!(result, rule);
    }

    #[test]
    fn blend_scores_weight_one_returns_semantic_only() {
        let rule: HashMap<String, f64> = [
            ("tests/a.rs".to_string(), 0.8),
            ("tests/b.rs".to_string(), 0.6),
        ]
        .into();
        let semantic: HashMap<String, f64> = [
            ("tests/a.rs".to_string(), 0.9),
            ("tests/c.rs".to_string(), 0.7),
        ]
        .into();

        let result = blend_scores(&rule, &semantic, 1.0);

        // tests/a.rs: semantic=0.9
        assert!((result.get("tests/a.rs").copied().unwrap() - 0.9).abs() < 1e-6);
        // tests/b.rs: only in rule, but weight=1 means semantic=0.0 for it, so 1.0 * 0.0 = 0.0
        assert!((result.get("tests/b.rs").copied().unwrap_or(0.0) - 0.0).abs() < 1e-6);
        // tests/c.rs: only in semantic, rule=0.0 → 1.0 * 0.7 = 0.7
        assert!((result.get("tests/c.rs").copied().unwrap() - 0.7).abs() < 1e-6);
    }

    #[test]
    fn blend_scores_combines_correctly() {
        let rule: HashMap<String, f64> = [("tests/a.rs".to_string(), 0.8)].into();
        let semantic: HashMap<String, f64> = [("tests/a.rs".to_string(), 0.6)].into();

        let result = blend_scores(&rule, &semantic, 0.5);
        // (1 - 0.5) * 0.8 + 0.5 * 0.6 = 0.4 + 0.3 = 0.7
        assert!((result.get("tests/a.rs").copied().unwrap() - 0.7).abs() < 1e-6);
    }

    #[test]
    fn blend_scores_empty_semantic_returns_rule_unchanged() {
        let rule: HashMap<String, f64> = [("tests/a.rs".to_string(), 0.8)].into();
        let semantic: HashMap<String, f64> = HashMap::new();

        let result = blend_scores(&rule, &semantic, 0.5);
        assert_eq!(result, rule);
    }

    #[test]
    fn blend_scores_adds_semantic_only_files() {
        let rule: HashMap<String, f64> = HashMap::new();
        let semantic: HashMap<String, f64> = [("tests/new.rs".to_string(), 0.6)].into();

        let result = blend_scores(&rule, &semantic, 0.3);
        // 0.3 * 0.6 = 0.18
        assert!((result.get("tests/new.rs").copied().unwrap() - 0.18).abs() < 1e-6);
    }
}
