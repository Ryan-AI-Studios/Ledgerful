use crate::config::model::{ContractsConfig, LocalModelConfig};
use crate::contracts::parser;
use crate::embed::embed_and_store;
use camino::Utf8Path;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use tracing::warn;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ContractsIndexSummary {
    pub specs_parsed: usize,
    pub endpoints_new: usize,
    pub endpoints_skipped: usize,
    pub endpoints_deleted: usize,
}

pub fn index_contracts(
    config: &ContractsConfig,
    conn: &Connection,
    embed_config: &LocalModelConfig,
    repo_root: &Utf8Path,
) -> Result<ContractsIndexSummary, String> {
    if config.spec_paths.is_empty() {
        return Ok(ContractsIndexSummary::default());
    }

    let mut summary = ContractsIndexSummary::default();
    let mut seen_spec_files: Vec<String> = Vec::new();

    // Collect all spec files from configured paths
    for spec_path in &config.spec_paths {
        let full_path = repo_root.join(spec_path);

        if full_path.is_file() {
            seen_spec_files.push(full_path.to_string());
        } else if full_path.is_dir() {
            if let Ok(entries) = std::fs::read_dir(&full_path) {
                for entry in entries.flatten() {
                    let p = entry.path();
                    if p.is_file()
                        && let Some(ext) = p.extension().and_then(|e| e.to_str())
                        && matches!(ext, "yaml" | "yml" | "json")
                    {
                        seen_spec_files.push(p.to_string_lossy().to_string());
                    }
                }
            }
        } else {
            warn!("Spec path not found: {}", full_path);
        }
    }

    // Parse each spec and index endpoints
    for spec_file in &seen_spec_files {
        let spec_path = std::path::Path::new(spec_file);
        let result = parser::parse_spec_safe(spec_path)?;

        if result.endpoints.is_empty() {
            continue;
        }

        summary.specs_parsed += 1;

        for endpoint in &result.endpoints {
            let content_hash = blake3::hash(endpoint.embed_text.as_bytes())
                .to_hex()
                .to_string();

            let existing: Option<(i64, String)> = conn
                .query_row(
                    "SELECT id, content_hash FROM api_endpoints WHERE spec_path = ?1 AND method = ?2 AND path = ?3",
                    rusqlite::params![spec_file, endpoint.method, endpoint.path],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .ok();

            let hash_matches = existing.as_ref().is_some_and(|(_, h)| h == &content_hash);

            if hash_matches {
                summary.endpoints_skipped += 1;
                continue;
            }

            let tags_str = if endpoint.tags.is_empty() {
                String::new()
            } else {
                endpoint.tags.join(" ")
            };

            if let Some((id, _)) = existing {
                conn.execute(
                    "UPDATE api_endpoints SET summary = ?1, description = ?2, tags = ?3, content_hash = ?4 WHERE id = ?5",
                    rusqlite::params![
                        endpoint.summary,
                        endpoint.description,
                        tags_str,
                        content_hash,
                        id,
                    ],
                )
                .map_err(|e| e.to_string())?;
            } else {
                conn.execute(
                    "INSERT INTO api_endpoints (spec_path, method, path, summary, description, tags, content_hash) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    rusqlite::params![
                        spec_file,
                        endpoint.method,
                        endpoint.path,
                        endpoint.summary,
                        endpoint.description,
                        tags_str,
                        content_hash,
                    ],
                )
                .map_err(|e| e.to_string())?;
            }

            summary.endpoints_new += 1;

            let entity_id = format!("{}::{}::{}", spec_file, endpoint.method, endpoint.path);
            if let Err(e) = embed_and_store(
                embed_config,
                conn,
                "api_endpoint",
                &entity_id,
                &endpoint.embed_text,
            ) {
                warn!("Failed to embed endpoint {}: {}", entity_id, e);
            }
        }
    }

    // Clean up stale endpoints from specs that no longer exist in config
    let placeholders: Vec<String> = (0..seen_spec_files.len())
        .map(|i| format!("?{}", i + 1))
        .collect();
    if !placeholders.is_empty() {
        let query = format!(
            "SELECT spec_path, method, path FROM api_endpoints WHERE spec_path NOT IN ({})",
            placeholders.join(", ")
        );

        let mut stmt = conn.prepare(&query).map_err(|e| e.to_string())?;
        let mut stale_rows: Vec<(String, String, String)> = Vec::new();

        let params: Vec<&dyn rusqlite::types::ToSql> = seen_spec_files
            .iter()
            .map(|s| s as &dyn rusqlite::types::ToSql)
            .collect();

        let rows = stmt
            .query_map(params.as_slice(), |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .map_err(|e| e.to_string())?;

        for row in rows {
            let (spec_path, method, path) = row.map_err(|e| e.to_string())?;
            stale_rows.push((spec_path, method, path));
        }

        for (spec_path, method, path) in &stale_rows {
            conn.execute(
                "DELETE FROM api_endpoints WHERE spec_path = ?1 AND method = ?2 AND path = ?3",
                rusqlite::params![spec_path, method, path],
            )
            .map_err(|e| e.to_string())?;

            let entity_id = format!("{}::{}::{}", spec_path, method, path);
            let _ = conn.execute(
                "DELETE FROM embeddings WHERE entity_type = 'api_endpoint' AND entity_id = ?1",
                rusqlite::params![entity_id],
            );

            summary.endpoints_deleted += 1;
        }
    }

    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::migrations::get_migrations;
    use rusqlite::Connection;
    use std::path::Path;

    fn setup_db() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        let migrations = get_migrations();
        migrations.to_latest(&mut conn).unwrap();
        conn
    }

    fn write_oai3_spec(dir: &Path, filename: &str) {
        let spec = serde_json::json!({
            "openapi": "3.0.0",
            "info": { "title": "Test API", "version": "1.0" },
            "paths": {
                "/items": {
                    "get": {
                        "summary": "List items for the inventory system",
                        "description": "Returns paginated list of items",
                        "operationId": "listItems",
                        "tags": ["items", "inventory"]
                    },
                    "post": {
                        "summary": "Create a new inventory item",
                        "operationId": "createItem",
                        "tags": ["items"]
                    }
                },
                "/items/{itemId}": {
                    "get": {
                        "summary": "Get details of an item by ID",
                        "operationId": "getItem",
                        "tags": ["items"]
                    }
                }
            }
        });
        std::fs::write(
            dir.join(filename),
            serde_json::to_string_pretty(&spec).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn index_contracts_empty_config_returns_zero() {
        let conn = setup_db();
        let config = ContractsConfig::default();
        let embed_config = LocalModelConfig::default();
        let tmp = tempfile::tempdir().unwrap();
        let root = camino::Utf8Path::from_path(tmp.path()).unwrap();

        let result = index_contracts(&config, &conn, &embed_config, root).unwrap();
        assert_eq!(result.specs_parsed, 0);
        assert_eq!(result.endpoints_new, 0);
        assert_eq!(result.endpoints_skipped, 0);
        assert_eq!(result.endpoints_deleted, 0);
    }

    #[test]
    fn index_contracts_fresh_index_endpoints_new() {
        let conn = setup_db();
        let embed_config = LocalModelConfig::default();
        let tmp = tempfile::tempdir().unwrap();
        write_oai3_spec(tmp.path(), "openapi.json");

        let root = camino::Utf8Path::from_path(tmp.path()).unwrap();
        let config = ContractsConfig {
            spec_paths: vec!["openapi.json".to_string()],
            ..Default::default()
        };

        let result = index_contracts(&config, &conn, &embed_config, root).unwrap();
        assert_eq!(result.specs_parsed, 1);
        assert_eq!(result.endpoints_new, 3);
        assert_eq!(result.endpoints_skipped, 0);
        assert_eq!(result.endpoints_deleted, 0);

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM api_endpoints", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 3);
    }

    #[test]
    fn index_contracts_reindex_unchanged_skips_all() {
        let conn = setup_db();
        let embed_config = LocalModelConfig::default();
        let tmp = tempfile::tempdir().unwrap();
        write_oai3_spec(tmp.path(), "openapi.json");

        let root = camino::Utf8Path::from_path(tmp.path()).unwrap();
        let config = ContractsConfig {
            spec_paths: vec!["openapi.json".to_string()],
            ..Default::default()
        };

        let result1 = index_contracts(&config, &conn, &embed_config, root).unwrap();
        assert_eq!(result1.endpoints_new, 3);
        assert_eq!(result1.endpoints_skipped, 0);

        let result2 = index_contracts(&config, &conn, &embed_config, root).unwrap();
        assert_eq!(result2.specs_parsed, 1);
        assert_eq!(result2.endpoints_new, 0);
        assert_eq!(result2.endpoints_skipped, 3);
        assert_eq!(result2.endpoints_deleted, 0);
    }

    #[test]
    fn index_contracts_spec_removed_deletes_endpoints() {
        let conn = setup_db();
        let embed_config = LocalModelConfig::default();
        let tmp = tempfile::tempdir().unwrap();
        write_oai3_spec(tmp.path(), "openapi.json");
        write_oai3_spec(tmp.path(), "other.yaml");

        let root = camino::Utf8Path::from_path(tmp.path()).unwrap();
        let config = ContractsConfig {
            spec_paths: vec!["openapi.json".to_string(), "other.yaml".to_string()],
            ..Default::default()
        };

        let result1 = index_contracts(&config, &conn, &embed_config, root).unwrap();
        assert_eq!(result1.specs_parsed, 2);
        assert_eq!(result1.endpoints_new, 6);
        assert_eq!(result1.endpoints_deleted, 0);

        let config2 = ContractsConfig {
            spec_paths: vec!["openapi.json".to_string()],
            ..Default::default()
        };
        let result2 = index_contracts(&config2, &conn, &embed_config, root).unwrap();
        assert_eq!(result2.specs_parsed, 1);
        assert_eq!(result2.endpoints_skipped, 3);
        assert_eq!(result2.endpoints_deleted, 3);

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM api_endpoints", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 3);
    }

    #[test]
    fn index_contracts_empty_base_url_stores_endpoints_no_embedding() {
        let conn = setup_db();
        let embed_config = LocalModelConfig::default();
        assert!(embed_config.base_url.is_empty());

        let tmp = tempfile::tempdir().unwrap();
        write_oai3_spec(tmp.path(), "openapi.json");

        let root = camino::Utf8Path::from_path(tmp.path()).unwrap();
        let config = ContractsConfig {
            spec_paths: vec!["openapi.json".to_string()],
            ..Default::default()
        };

        let result = index_contracts(&config, &conn, &embed_config, root).unwrap();
        assert_eq!(result.endpoints_new, 3);

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM api_endpoints", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 3);

        let embed_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM embeddings WHERE entity_type = 'api_endpoint'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(embed_count, 0);
    }

    #[test]
    fn index_contracts_directory_of_specs() {
        let conn = setup_db();
        let embed_config = LocalModelConfig::default();
        let tmp = tempfile::tempdir().unwrap();

        let subdir = tmp.path().join("specs");
        std::fs::create_dir(&subdir).unwrap();
        write_oai3_spec(&subdir, "a.json");
        write_oai3_spec(&subdir, "b.yaml");

        let root = camino::Utf8Path::from_path(tmp.path()).unwrap();
        let config = ContractsConfig {
            spec_paths: vec!["specs".to_string()],
            ..Default::default()
        };

        let result = index_contracts(&config, &conn, &embed_config, root).unwrap();
        assert_eq!(result.specs_parsed, 2);
        assert_eq!(result.endpoints_new, 6);
        assert_eq!(result.endpoints_skipped, 0);

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM api_endpoints", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 6);
    }

    #[test]
    fn index_contracts_malformed_spec_skipped() {
        let conn = setup_db();
        let embed_config = LocalModelConfig::default();
        let tmp = tempfile::tempdir().unwrap();

        std::fs::write(tmp.path().join("bad.yaml"), "not: valid: yaml: [").unwrap();
        write_oai3_spec(tmp.path(), "good.json");

        let root = camino::Utf8Path::from_path(tmp.path()).unwrap();
        let config = ContractsConfig {
            spec_paths: vec!["good.json".to_string(), "bad.yaml".to_string()],
            ..Default::default()
        };

        let result = index_contracts(&config, &conn, &embed_config, root).unwrap();
        assert_eq!(result.specs_parsed, 1);
        assert_eq!(result.endpoints_new, 3);
    }
}
