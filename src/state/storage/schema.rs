use crate::state::storage::connection::StorageManager;
use miette::{IntoDiagnostic, Result};
use std::collections::HashMap;
use std::path::PathBuf;

impl StorageManager {
    pub fn get_directory_classifications(
        &self,
    ) -> Result<Vec<crate::index::topology::DirectoryClassification>> {
        let mut stmt = self
            .conn
            .prepare("SELECT dir_path, role, confidence, evidence FROM project_topology")
            .into_diagnostic()?;

        let rows = stmt
            .query_map([], |row| {
                let role_str: String = row.get(1)?;
                let role = crate::index::topology::DirectoryRole::parse(&role_str)
                    .unwrap_or(crate::index::topology::DirectoryRole::Source);
                Ok(crate::index::topology::DirectoryClassification {
                    dir_path: row.get(0)?,
                    role,
                    confidence: row.get(2)?,
                    evidence: row.get(3)?,
                })
            })
            .into_diagnostic()?;

        let mut results = Vec::new();
        for row in rows {
            results.push(row.into_diagnostic()?);
        }
        Ok(results)
    }

    /// Returns a map of file paths to their internal IDs in the project_files table.
    /// Only includes files that are not marked as DELETED.
    pub fn get_active_file_id_map(&self) -> Result<HashMap<PathBuf, i64>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, file_path FROM project_files WHERE parse_status != 'DELETED'")
            .into_diagnostic()?;

        let rows = stmt
            .query_map([], |row| {
                let id: i64 = row.get(0)?;
                let path: String = row.get(1)?;
                Ok((PathBuf::from(path), id))
            })
            .into_diagnostic()?;

        let mut map = HashMap::new();
        for row in rows {
            let (path, id) = row.into_diagnostic()?;
            map.insert(path, id);
        }
        Ok(map)
    }

    /// Checks if a table exists in the database.
    pub fn table_exists(&self, table_name: &str) -> Result<bool> {
        let exists: bool = self
            .conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
                [table_name],
                |row| row.get(0),
            )
            .into_diagnostic()?;

        Ok(exists)
    }

    /// Checks if a table exists and contains at least one row.
    pub fn table_exists_and_has_data(&self, table_name: &str) -> Result<bool> {
        // Basic validation to prevent injection since we use format! for the table name
        if !table_name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_')
        {
            return Err(miette::miette!("Invalid table name: {}", table_name));
        }

        if !self.table_exists(table_name)? {
            return Ok(false);
        }

        // Then check if it has data
        let query = format!("SELECT EXISTS(SELECT 1 FROM {} LIMIT 1)", table_name);
        let has_data: bool = self
            .conn
            .query_row(&query, [], |row| row.get(0))
            .into_diagnostic()?;

        Ok(has_data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::impact::packet::ImpactPacket;
    use crate::state::storage::connection::in_memory_storage;

    #[test]
    fn test_table_exists() {
        let storage = in_memory_storage();
        assert!(storage.table_exists("snapshots").unwrap());
        assert!(!storage.table_exists("non_existent").unwrap());
    }

    #[test]
    fn test_table_exists_and_has_data() {
        let storage = in_memory_storage();
        // snapshots table is created in migrations, but empty
        assert!(!storage.table_exists_and_has_data("snapshots").unwrap());

        // Save a packet to make it non-empty
        let packet = ImpactPacket::default();
        storage.save_packet(&packet).unwrap();
        assert!(storage.table_exists_and_has_data("snapshots").unwrap());

        // Non-existent table
        assert!(!storage.table_exists_and_has_data("non_existent").unwrap());
    }

    #[test]
    fn test_get_active_file_id_map() {
        let storage = in_memory_storage();
        storage.get_connection().execute(
            "INSERT INTO project_files (file_path, parse_status, last_indexed_at) VALUES ('src/a.rs', 'OK', '2026-01-01T00:00:00Z')",
            [],
        ).unwrap();
        storage.get_connection().execute(
            "INSERT INTO project_files (file_path, parse_status, last_indexed_at) VALUES ('src/b.rs', 'DELETED', '2026-01-01T00:00:00Z')",
            [],
        ).unwrap();

        let map = storage.get_active_file_id_map().unwrap();
        assert_eq!(map.len(), 1);
        assert!(map.contains_key(&PathBuf::from("src/a.rs")));
        assert!(!map.contains_key(&PathBuf::from("src/b.rs")));
    }
}
