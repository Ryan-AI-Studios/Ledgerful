use crate::config::model::{ContractsConfig, LocalModelConfig};
use crate::contracts::AffectedContract;
use crate::embed::similarity::cosine_sim;
use crate::embed::storage::{load_candidates, load_embedding};
use rusqlite::Connection;
use rusqlite::OptionalExtension;
use std::collections::HashMap;

pub fn match_changed_files(
    config: &ContractsConfig,
    conn: &Connection,
    embed_config: &LocalModelConfig,
    changed_files: &[String],
) -> Result<Vec<AffectedContract>, String> {
    // Check if api_endpoints table has data
    let endpoint_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM api_endpoints", [], |row| row.get(0))
        .map_err(|e| format!("Failed to query api_endpoints: {e}"))?;

    if endpoint_count == 0 {
        return Ok(Vec::new());
    }

    // Load all api_endpoint embeddings
    let candidates = load_candidates(conn, "api_endpoint", &embed_config.embedding_model)?;

    if candidates.is_empty() {
        return Ok(Vec::new());
    }

    let mut best_matches: HashMap<String, AffectedContract> = HashMap::new();

    for file_path in changed_files {
        if !file_path.ends_with(".rs")
            && !file_path.ends_with(".py")
            && !file_path.ends_with(".ts")
            && !file_path.ends_with(".tsx")
            && !file_path.ends_with(".js")
            && !file_path.ends_with(".jsx")
        {
            continue;
        }

        // Query pre-indexed file embedding from DB (no embedding on hot path)
        let file_embedding =
            match load_embedding(conn, "file", file_path, &embed_config.embedding_model) {
                Ok(Some(v)) => v,
                Ok(None) => {
                    tracing::debug!("No pre-indexed embedding for {file_path}, skipping");
                    continue;
                }
                Err(e) => {
                    tracing::debug!("Failed to load embedding for {file_path}: {e}, skipping");
                    continue;
                }
            };

        for (entity_id, endpoint_vec) in &candidates {
            let similarity = match cosine_sim(&file_embedding, endpoint_vec) {
                Ok(s) => s,
                Err(_) => continue,
            };

            if similarity < config.match_threshold {
                continue;
            }

            let contract = parse_entity_id(entity_id);
            if contract.path.is_empty() {
                continue;
            }

            let full_contract = AffectedContract {
                endpoint_id: entity_id.clone(),
                similarity,
                ..contract
            };

            let key = format!("{}::{}", full_contract.spec_file, full_contract.endpoint_id);

            match best_matches.get(&key) {
                Some(existing) if existing.similarity >= similarity => {}
                _ => {
                    best_matches.insert(key, full_contract);
                }
            }
        }
    }

    // Query summary from api_endpoints table
    let mut results: Vec<AffectedContract> = Vec::new();
    for mut contract in best_matches.into_values() {
        if contract.summary.is_empty()
            && let Ok(Some(summary)) =
                get_endpoint_summary(conn, &contract.spec_file, &contract.method, &contract.path)
        {
            contract.summary = summary;
        }
        results.push(contract);
    }

    results.sort_by(|a, b| {
        b.similarity
            .partial_cmp(&a.similarity)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.path.cmp(&b.path))
    });

    if results.len() > 10 {
        results.truncate(10);
    }

    Ok(results)
}

fn parse_entity_id(entity_id: &str) -> AffectedContract {
    // entity_id format: "spec_file::METHOD::path"
    let parts: Vec<&str> = entity_id.splitn(3, "::").collect();
    if parts.len() != 3 {
        return AffectedContract {
            endpoint_id: entity_id.to_string(),
            path: String::new(),
            method: String::new(),
            summary: String::new(),
            similarity: 0.0,
            spec_file: String::new(),
        };
    }

    AffectedContract {
        endpoint_id: entity_id.to_string(),
        path: parts[2].to_string(),
        method: parts[1].to_string(),
        summary: String::new(),
        similarity: 0.0,
        spec_file: parts[0].to_string(),
    }
}

fn get_endpoint_summary(
    conn: &Connection,
    spec_file: &str,
    method: &str,
    path: &str,
) -> Result<Option<String>, rusqlite::Error> {
    conn.query_row(
        "SELECT summary FROM api_endpoints WHERE spec_path = ?1 AND method = ?2 AND path = ?3",
        rusqlite::params![spec_file, method, path],
        |row| row.get(0),
    )
    .optional()
    .map(|r| r.flatten())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::migrations::get_migrations;

    fn setup_db() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        let migrations = get_migrations();
        migrations.to_latest(&mut conn).unwrap();
        conn
    }

    fn seed_endpoints(conn: &Connection) -> Vec<String> {
        let endpoints = vec![
            ("specs/api.yaml::GET::/pets", "List all pets"),
            ("specs/api.yaml::POST::/pets", "Create a pet"),
            ("specs/api.yaml::GET::/users", "List users"),
            ("specs/api.yaml::DELETE::/users/{id}", "Delete a user"),
            ("specs/api.yaml::PUT::/products", "Update product"),
        ];

        for (entity_id, summary) in &endpoints {
            let parts: Vec<&str> = entity_id.splitn(3, "::").collect();
            let spec_file = parts[0];
            let method = parts[1];
            let path = parts[2];

            conn.execute(
                "INSERT OR IGNORE INTO api_endpoints (spec_path, method, path, summary, content_hash)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![spec_file, method, path, summary, entity_id],
            )
            .unwrap();
        }

        // Insert mock embeddings for endpoints (use entity_id as mock vector)
        for entity_id in endpoints.iter().map(|(id, _)| id) {
            let vector: Vec<f32> = entity_id
                .chars()
                .map(|c| c as u32 as f32 / 255.0)
                .take(8)
                .collect();
            let blob: Vec<u8> = vector.iter().flat_map(|f| f.to_le_bytes()).collect();
            conn.execute(
                "INSERT INTO embeddings (entity_type, entity_id, content_hash, model_name, dimensions, vector)
                 VALUES ('api_endpoint', ?1, ?2, 'test-model', ?3, ?4)",
                rusqlite::params![entity_id, entity_id, vector.len() as i64, blob],
            )
            .unwrap();
        }

        endpoints.iter().map(|(id, _)| id.to_string()).collect()
    }

    fn seed_file_embeddings(conn: &Connection, files: &[(&str, &[f32])]) {
        for (entity_id, vector) in files {
            let blob: Vec<u8> = vector.iter().flat_map(|f| f.to_le_bytes()).collect();
            conn.execute(
                "INSERT OR IGNORE INTO embeddings (entity_type, entity_id, content_hash, model_name, dimensions, vector)
                 VALUES ('file', ?1, ?2, 'test-model', ?3, ?4)",
                rusqlite::params![entity_id, entity_id, vector.len() as i64, blob],
            )
            .unwrap();
        }
    }

    #[test]
    fn match_changed_files_no_matching_model_returns_empty() {
        let conn = setup_db();
        seed_endpoints(&conn);

        let config = ContractsConfig::default();
        let embed_config = LocalModelConfig::default();
        let changed = vec!["src/main.rs".to_string()];

        let result = match_changed_files(&config, &conn, &embed_config, &changed).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn match_changed_files_empty_endpoints_returns_empty() {
        let conn = setup_db();
        // Don't seed any endpoints

        let config = ContractsConfig::default();
        let embed_config = LocalModelConfig {
            base_url: "http://localhost:11434".to_string(),
            embedding_url: None,
            generation_url: None,
            embedding_model: "test-model".to_string(),
            ..Default::default()
        };
        let changed = vec!["src/main.rs".to_string()];

        let result = match_changed_files(&config, &conn, &embed_config, &changed).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn match_changed_files_empty_changed_list_returns_empty() {
        let conn = setup_db();
        seed_endpoints(&conn);

        let config = ContractsConfig::default();
        let embed_config = LocalModelConfig {
            base_url: "http://localhost:11434".to_string(),
            embedding_url: None,
            generation_url: None,
            embedding_model: "test-model".to_string(),
            ..Default::default()
        };
        let changed: Vec<String> = Vec::new();

        let result = match_changed_files(&config, &conn, &embed_config, &changed).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn matched_when_file_embeddings_present() {
        let conn = setup_db();
        seed_endpoints(&conn);

        // Seed a file embedding that is strongly similar to one of the endpoint vectors
        // Use the same char→f32 mapping as seed_endpoints for exact match
        let file_vector: Vec<f32> = "specs/api.yaml::GET::/pets"
            .chars()
            .map(|c| c as u32 as f32 / 255.0)
            .take(8)
            .collect();
        seed_file_embeddings(&conn, &[("src/api/handlers.rs", &file_vector)]);

        let config = ContractsConfig::default();
        let embed_config = LocalModelConfig {
            base_url: "http://localhost:11434".to_string(),
            embedding_url: None,
            generation_url: None,
            embedding_model: "test-model".to_string(),
            ..Default::default()
        };
        let changed = vec!["src/api/handlers.rs".to_string()];

        let result = match_changed_files(&config, &conn, &embed_config, &changed).unwrap();

        // With an identical vector, should match at least one endpoint
        assert!(
            !result.is_empty(),
            "Should match at least one endpoint when file embedding is present"
        );

        // All results should have valid similarity above threshold
        for contract in &result {
            assert!(
                contract.similarity > 0.0,
                "Similarity should be positive: {}",
                contract.similarity
            );
            assert!(!contract.method.is_empty(), "Method should not be empty");
            assert!(!contract.path.is_empty(), "Path should not be empty");
        }

        // Result should be sorted by similarity descending
        for i in 1..result.len() {
            assert!(
                result[i - 1].similarity >= result[i].similarity,
                "Results should be sorted by similarity descending"
            );
        }
    }

    #[test]
    fn skipped_when_file_embedding_missing() {
        let conn = setup_db();
        seed_endpoints(&conn);
        // Don't seed file embeddings for "src/nonexistent.rs"

        let config = ContractsConfig::default();
        let embed_config = LocalModelConfig {
            base_url: "http://localhost:11434".to_string(),
            embedding_url: None,
            generation_url: None,
            embedding_model: "test-model".to_string(),
            ..Default::default()
        };
        let changed = vec!["src/nonexistent.rs".to_string()];

        let result = match_changed_files(&config, &conn, &embed_config, &changed).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn no_embedding_generated_during_matching() {
        let conn = setup_db();
        seed_endpoints(&conn);

        // Seed a file embedding (no HTTP embedding call on hot path)
        let file_vector = vec![0.01_f32, 0.01, 0.01, 0.01, 0.01, 0.01, 0.01, 0.01];
        seed_file_embeddings(&conn, &[("src/other/mod.rs", &file_vector)]);

        let config = ContractsConfig::default();
        let embed_config = LocalModelConfig {
            base_url: "http://localhost:11434".to_string(),
            embedding_url: None,
            generation_url: None,
            embedding_model: "test-model".to_string(),
            ..Default::default()
        };
        let changed = vec!["src/other/mod.rs".to_string()];

        // Should not panic and should not call embed API
        let result = match_changed_files(&config, &conn, &embed_config, &changed);
        assert!(result.is_ok());
    }

    #[test]
    fn parse_entity_id_valid() {
        let contract = parse_entity_id("specs/api.yaml::GET::/pets");
        assert_eq!(contract.spec_file, "specs/api.yaml");
        assert_eq!(contract.method, "GET");
        assert_eq!(contract.path, "/pets");
    }

    #[test]
    fn parse_entity_id_invalid_returns_empty() {
        let contract = parse_entity_id("just_one_part");
        assert!(contract.path.is_empty());
        assert!(contract.method.is_empty());
    }

    #[test]
    fn get_endpoint_summary_found() {
        let conn = setup_db();
        seed_endpoints(&conn);

        let summary = get_endpoint_summary(&conn, "specs/api.yaml", "GET", "/pets").unwrap();
        assert_eq!(summary, Some("List all pets".to_string()));
    }

    #[test]
    fn get_endpoint_summary_not_found_returns_none() {
        let conn = setup_db();
        seed_endpoints(&conn);

        let summary = get_endpoint_summary(&conn, "specs/api.yaml", "GET", "/nonexistent").unwrap();
        assert_eq!(summary, None);
    }
}
