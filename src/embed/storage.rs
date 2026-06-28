use rusqlite::Connection;

pub fn content_hash(text: &str) -> String {
    blake3::hash(text.as_bytes()).to_hex().to_string()
}

pub fn upsert_embedding(
    conn: &Connection,
    entity_type: &str,
    entity_id: &str,
    text: &str,
    model_name: &str,
    vector: &[f32],
    dimensions: usize,
) -> Result<(), String> {
    if vector.len() != dimensions {
        return Err(format!(
            "Dimension mismatch: vector has {} elements, expected {}",
            vector.len(),
            dimensions
        ));
    }

    let hash = content_hash(text);

    // Check if row exists with same (entity_type, entity_id, model_name)
    let existing: Option<(i64, String)> = conn
        .query_row(
            "SELECT id, content_hash FROM embeddings WHERE entity_type = ?1 AND entity_id = ?2 AND model_name = ?3",
            rusqlite::params![entity_type, entity_id, model_name],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .ok();

    match existing {
        Some((_id, existing_hash)) if existing_hash == hash => {
            // Same content hash — no-op
            Ok(())
        }
        Some((id, _)) => {
            // Hash differs — update
            let vector_blob: Vec<u8> = vector.iter().flat_map(|f| f.to_le_bytes()).collect();
            conn.execute(
                "UPDATE embeddings SET content_hash = ?1, vector = ?2, created_at = datetime('now') WHERE id = ?3",
                rusqlite::params![hash, vector_blob, id],
            )
            .map_err(|e| e.to_string())?;
            Ok(())
        }
        None => {
            // Insert new
            let vector_blob: Vec<u8> = vector.iter().flat_map(|f| f.to_le_bytes()).collect();
            conn.execute(
                "INSERT INTO embeddings (entity_type, entity_id, content_hash, model_name, dimensions, vector)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![
                    entity_type,
                    entity_id,
                    hash,
                    model_name,
                    dimensions as i64,
                    vector_blob,
                ],
            )
            .map_err(|e| e.to_string())?;
            Ok(())
        }
    }
}

pub fn load_embedding(
    conn: &Connection,
    entity_type: &str,
    entity_id: &str,
    model_name: &str,
) -> Result<Option<Vec<f32>>, String> {
    let vector_blob: Option<Vec<u8>> = conn
        .query_row(
            "SELECT vector FROM embeddings WHERE entity_type = ?1 AND entity_id = ?2 AND model_name = ?3",
            rusqlite::params![entity_type, entity_id, model_name],
            |row| row.get(0),
        )
        .ok();

    match vector_blob {
        Some(blob) => {
            if blob.len() % 4 != 0 {
                return Err("Corrupt vector blob: length not a multiple of 4".to_string());
            }
            let floats: Vec<f32> = blob
                .chunks_exact(4)
                .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
                .collect();
            Ok(Some(floats))
        }
        None => Ok(None),
    }
}

pub fn embedding_count(conn: &Connection) -> Result<usize, String> {
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM embeddings", [], |row| row.get(0))
        .map_err(|e| e.to_string())?;
    Ok(count as usize)
}

pub fn load_candidates(
    conn: &Connection,
    entity_type: &str,
    model_name: &str,
) -> Result<Vec<(String, Vec<f32>)>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT entity_id, vector FROM embeddings WHERE entity_type = ?1 AND model_name = ?2",
        )
        .map_err(|e| e.to_string())?;

    let rows = stmt
        .query_map(rusqlite::params![entity_type, model_name], |row| {
            let entity_id: String = row.get(0)?;
            let blob: Vec<u8> = row.get(1)?;
            Ok((entity_id, blob))
        })
        .map_err(|e| e.to_string())?;

    let mut candidates = Vec::new();
    for row in rows {
        let (entity_id, blob) = row.map_err(|e| e.to_string())?;
        if blob.len() % 4 != 0 {
            return Err("Corrupt vector blob: length not a multiple of 4".to_string());
        }
        let floats: Vec<f32> = blob
            .chunks_exact(4)
            .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
            .collect();
        candidates.push((entity_id, floats));
    }

    Ok(candidates)
}

pub fn clear_all_embeddings(conn: &Connection) -> Result<(), String> {
    conn.execute("DELETE FROM embeddings", [])
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::migrations::get_migrations;
    use rusqlite::Connection;

    fn setup_db() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        let migrations = get_migrations();
        migrations.to_latest(&mut conn).unwrap();
        conn
    }

    #[test]
    fn test_content_hash_deterministic() {
        let h1 = content_hash("hello world");
        let h2 = content_hash("hello world");
        let h3 = content_hash("hello different");
        assert_eq!(h1, h2);
        assert_ne!(h1, h3);
        assert_eq!(h1.len(), 64); // blake3 hex is 64 chars
    }

    #[test]
    fn test_upsert_then_load_roundtrip() {
        let conn = setup_db();
        let vector: Vec<f32> = vec![1.0, 2.0, 3.0, 4.0];

        upsert_embedding(
            &conn,
            "FILE",
            "src/main.rs",
            "hello world",
            "test-model",
            &vector,
            4,
        )
        .unwrap();

        let loaded = load_embedding(&conn, "FILE", "src/main.rs", "test-model")
            .unwrap()
            .expect("should find embedding");
        assert_eq!(loaded, vector);
        assert_eq!(embedding_count(&conn).unwrap(), 1);
    }

    #[test]
    fn test_upsert_same_content_no_duplicate() {
        let conn = setup_db();
        let vector: Vec<f32> = vec![1.0, 2.0];

        upsert_embedding(
            &conn,
            "FILE",
            "src/main.rs",
            "same content",
            "test-model",
            &vector,
            2,
        )
        .unwrap();
        upsert_embedding(
            &conn,
            "FILE",
            "src/main.rs",
            "same content",
            "test-model",
            &vector,
            2,
        )
        .unwrap();

        assert_eq!(embedding_count(&conn).unwrap(), 1);
    }

    #[test]
    fn test_upsert_changed_content_replaces_row() {
        let conn = setup_db();
        let v1: Vec<f32> = vec![1.0, 2.0];
        let v2: Vec<f32> = vec![3.0, 4.0];

        upsert_embedding(
            &conn,
            "FILE",
            "src/main.rs",
            "original text",
            "test-model",
            &v1,
            2,
        )
        .unwrap();
        upsert_embedding(
            &conn,
            "FILE",
            "src/main.rs",
            "changed text",
            "test-model",
            &v2,
            2,
        )
        .unwrap();

        // Should still have exactly one row
        assert_eq!(embedding_count(&conn).unwrap(), 1);

        // The vector should be the new one
        let loaded = load_embedding(&conn, "FILE", "src/main.rs", "test-model")
            .unwrap()
            .expect("should find embedding");
        assert_eq!(loaded, v2);
    }

    #[test]
    fn test_upsert_dimension_mismatch_returns_error() {
        let conn = setup_db();
        let vector: Vec<f32> = vec![1.0, 2.0];

        let result = upsert_embedding(
            &conn,
            "FILE",
            "src/main.rs",
            "text",
            "test-model",
            &vector,
            768,
        );
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Dimension mismatch"));
    }

    #[test]
    fn test_load_nonexistent_returns_none() {
        let conn = setup_db();
        let result = load_embedding(&conn, "FILE", "nonexistent", "test-model").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_clear_all_embeddings() {
        let conn = setup_db();
        let v: Vec<f32> = vec![1.0];

        upsert_embedding(&conn, "FILE", "a.rs", "text a", "model", &v, 1).unwrap();
        upsert_embedding(&conn, "FILE", "b.rs", "text b", "model", &v, 1).unwrap();
        assert_eq!(embedding_count(&conn).unwrap(), 2);

        clear_all_embeddings(&conn).unwrap();
        assert_eq!(embedding_count(&conn).unwrap(), 0);
    }

    #[test]
    fn test_load_candidates_returns_stored_embeddings() {
        let conn = setup_db();
        let v1: Vec<f32> = vec![1.0, 2.0, 3.0];
        let v2: Vec<f32> = vec![4.0, 5.0, 6.0];
        let v3: Vec<f32> = vec![7.0, 8.0, 9.0];

        upsert_embedding(&conn, "FILE", "a.rs", "text a", "model-x", &v1, 3).unwrap();
        upsert_embedding(&conn, "FILE", "b.rs", "text b", "model-x", &v2, 3).unwrap();
        upsert_embedding(&conn, "FILE", "c.rs", "text c", "model-x", &v3, 3).unwrap();

        let candidates = load_candidates(&conn, "FILE", "model-x").unwrap();
        assert_eq!(candidates.len(), 3);

        let ids: Vec<&str> = candidates.iter().map(|(id, _)| id.as_str()).collect();
        assert!(ids.contains(&"a.rs"));
        assert!(ids.contains(&"b.rs"));
        assert!(ids.contains(&"c.rs"));
    }

    #[test]
    fn test_upsert_different_models_same_entity() {
        let conn = setup_db();
        let v1: Vec<f32> = vec![1.0, 2.0];
        let v2: Vec<f32> = vec![3.0, 4.0];

        upsert_embedding(&conn, "FILE", "src/main.rs", "text", "model-a", &v1, 2).unwrap();
        upsert_embedding(&conn, "FILE", "src/main.rs", "text", "model-b", &v2, 2).unwrap();

        assert_eq!(embedding_count(&conn).unwrap(), 2);

        let loaded_a = load_embedding(&conn, "FILE", "src/main.rs", "model-a")
            .unwrap()
            .expect("model-a should exist");
        assert_eq!(loaded_a, v1);

        let loaded_b = load_embedding(&conn, "FILE", "src/main.rs", "model-b")
            .unwrap()
            .expect("model-b should exist");
        assert_eq!(loaded_b, v2);
    }
}
