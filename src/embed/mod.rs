pub mod budget;
pub mod client;
pub mod similarity;
pub mod storage;

use crate::config::model::LocalModelConfig;
use rusqlite::Connection;

pub fn embed_and_store(
    config: &LocalModelConfig,
    conn: &Connection,
    entity_type: &str,
    entity_id: &str,
    text: &str,
) -> Result<bool, String> {
    if config.base_url.is_empty() {
        return Ok(false);
    }

    let hash = storage::content_hash(text);

    let existing: Option<String> = conn
        .query_row(
            "SELECT content_hash FROM embeddings WHERE entity_type = ?1 AND entity_id = ?2 AND model_name = ?3",
            rusqlite::params![entity_type, entity_id, config.embedding_model],
            |row| row.get(0),
        )
        .ok();

    if existing.as_deref() == Some(&hash) {
        return Ok(false);
    }

    let vector = client::embed_long_text(config, text)?;

    storage::upsert_embedding(
        conn,
        entity_type,
        entity_id,
        text,
        &config.embedding_model,
        &vector,
        vector.len(),
    )?;

    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::migrations::get_migrations;
    use httpmock::prelude::*;

    fn setup_db() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        let migrations = get_migrations();
        migrations.to_latest(&mut conn).unwrap();
        conn
    }

    #[test]
    fn embed_and_store_empty_base_url_returns_false() {
        let conn = setup_db();
        let config = LocalModelConfig::default();
        let result = embed_and_store(&config, &conn, "FILE", "test.rs", "some text").unwrap();
        assert!(!result);
    }

    #[test]
    fn embed_and_store_new_text_returns_true() {
        let conn = setup_db();
        let server = MockServer::start();

        server.mock(|when, then| {
            when.method(httpmock::Method::POST).path("/v1/embeddings");
            then.status(200)
                .header("Content-Type", "application/json")
                .json_body(serde_json::json!({
                    "data": [
                        {"embedding": [0.1, 0.2, 0.3]}
                    ]
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

        let result = embed_and_store(&config, &conn, "FILE", "test.rs", "hello world").unwrap();
        assert!(result);
    }

    #[test]
    fn embed_and_store_duplicate_skip_returns_false() {
        let conn = setup_db();
        let server = MockServer::start();

        let mock = server.mock(|when, then| {
            when.method(httpmock::Method::POST).path("/v1/embeddings");
            then.status(200)
                .header("Content-Type", "application/json")
                .json_body(serde_json::json!({
                    "data": [
                        {"embedding": [0.1, 0.2, 0.3]}
                    ]
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

        let text = "hello world";
        let result1 = embed_and_store(&config, &conn, "FILE", "test.rs", text).unwrap();
        assert!(result1);

        let result2 = embed_and_store(&config, &conn, "FILE", "test.rs", text).unwrap();
        assert!(!result2);

        mock.assert_hits(1); // Only one HTTP call
    }
}
