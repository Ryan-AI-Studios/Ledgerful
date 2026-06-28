use httpmock::prelude::*;
use ledgerful::config::model::LocalModelConfig;
use ledgerful::state::storage::StorageManager;
use ledgerful::verify::ci_predictor::{
    CIJobOutcome, query_similar_ci_outcomes, record_ci_outcomes,
};
use ledgerful::verify::semantic_predictor::TestStatus;
use tempfile::tempdir;

#[test]
fn test_ci_outcome_recording_and_query() {
    let temp = tempdir().unwrap();
    let db_path = temp.path().join("ledger.db");
    let storage = StorageManager::init(&db_path).unwrap();
    let conn = storage.get_connection();

    // Setup project_files for foreign key
    conn.execute(
        "INSERT INTO project_files (file_path, content_hash, file_size, last_indexed_at) VALUES (?1, 'hash', 100, 'now')",
        [".github/workflows/ci.yml"],
    ).unwrap();
    let _ci_file_id: i64 = conn.last_insert_rowid();

    let server = MockServer::start();
    let embed_config = LocalModelConfig {
        base_url: server.base_url(),
        embedding_url: None,
        generation_url: None,
        embedding_model: "test-embed".to_string(),
        ..LocalModelConfig::default()
    };

    // Mock embedding calls
    server.mock(|when, then| {
        when.method(POST).path("/v1/embeddings");
        then.status(200)
            .header("Content-Type", "application/json")
            .json_body(serde_json::json!({
                "data": [{ "embedding": vec![0.1; 384] }]
            }));
    });

    let diff_text = "Modified src/lib.rs to fix a bug.";
    let outcomes = vec![CIJobOutcome {
        job_name: "test-linux".to_string(),
        platform: "linux".to_string(),
        ci_file_path: ".github/workflows/ci.yml".to_string(),
        commit_hash: "abc123".to_string(),
        status: TestStatus::Failed,
        duration_ms: 120000,
    }];

    // Record
    let count = record_ci_outcomes(conn, &embed_config, &outcomes, diff_text).unwrap();
    assert_eq!(count, 1);

    // Query
    let similar = query_similar_ci_outcomes(conn, &embed_config, diff_text, 5).unwrap();
    assert_eq!(similar.len(), 1);
    assert_eq!(similar[0].0.job_name, "test-linux");
    assert_eq!(similar[0].0.status, TestStatus::Failed);
}
