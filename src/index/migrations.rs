use crate::state::storage::StorageManager;
use miette::{IntoDiagnostic, Result};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Migration {
    pub name: String,
    pub file_path: String,
    pub file_id: i64,
    pub changes: Vec<SchemaChange>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SchemaChange {
    CreateTable {
        table: String,
        columns: Vec<ColumnInfo>,
    },
    DropTable {
        table: String,
    },
    AddColumn {
        table: String,
        column: ColumnInfo,
    },
    DropColumn {
        table: String,
        column: String,
    },
    RenameColumn {
        table: String,
        old_name: String,
        new_name: String,
    },
    ModifyColumn {
        table: String,
        column: ColumnInfo,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ColumnInfo {
    pub name: String,
    pub data_type: String,
}

pub struct MigrationParser<'a> {
    storage: &'a StorageManager,
    repo_path: &'a Path,
}

impl<'a> MigrationParser<'a> {
    pub fn new(storage: &'a StorageManager, repo_path: &'a Path) -> Self {
        Self { storage, repo_path }
    }

    pub fn parse_all(&self) -> Result<Vec<Migration>> {
        let conn = self.storage.get_connection();
        let mut stmt = conn
            .prepare("SELECT id, file_path FROM project_files WHERE file_path LIKE '%migration%' OR file_path LIKE '%.sql'")
            .into_diagnostic()?;

        let rows = stmt
            .query_map([], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
            })
            .into_diagnostic()?
            .collect::<std::result::Result<Vec<_>, rusqlite::Error>>()
            .into_diagnostic()?;

        let mut migrations = Vec::new();
        for (file_id, file_path) in rows {
            if let Some(migration) = self.parse_file(file_id, &file_path)? {
                migrations.push(migration);
            }
        }

        Ok(migrations)
    }

    fn parse_file(&self, file_id: i64, file_path: &str) -> Result<Option<Migration>> {
        let full_path = self.repo_path.join(file_path);
        if !full_path.exists() {
            return Ok(None);
        }

        let content = std::fs::read_to_string(&full_path).into_diagnostic()?;
        let name = Path::new(file_path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(file_path)
            .to_string();

        let changes = if file_path.ends_with(".sql") {
            self.parse_sql(&content)
        } else if file_path.ends_with(".rs") {
            self.parse_rust(&content)
        } else {
            Vec::new()
        };

        if changes.is_empty() && !file_path.contains("migration") {
            return Ok(None);
        }

        Ok(Some(Migration {
            name,
            file_path: file_path.to_string(),
            file_id,
            changes,
        }))
    }

    fn parse_sql(&self, content: &str) -> Vec<SchemaChange> {
        let mut changes = Vec::new();

        // CREATE TABLE regex
        let re_create = Regex::new(r"(?i)CREATE\s+TABLE\s+(\w+)\s*\(([\s\S]+?)\)").unwrap();
        for cap in re_create.captures_iter(content) {
            let table = cap[1].to_string();
            let col_defs = &cap[2];
            let mut columns = Vec::new();
            for col_line in col_defs.split(',') {
                let parts: Vec<&str> = col_line.split_whitespace().collect();
                if parts.len() >= 2 {
                    columns.push(ColumnInfo {
                        name: parts[0].to_string(),
                        data_type: parts[1].to_string(),
                    });
                }
            }
            changes.push(SchemaChange::CreateTable { table, columns });
        }

        // ALTER TABLE ADD COLUMN
        let re_add_col =
            Regex::new(r"(?i)ALTER\s+TABLE\s+(\w+)\s+ADD\s+(?:COLUMN\s+)?(\w+)\s+(\w+)").unwrap();
        for cap in re_add_col.captures_iter(content) {
            changes.push(SchemaChange::AddColumn {
                table: cap[1].to_string(),
                column: ColumnInfo {
                    name: cap[2].to_string(),
                    data_type: cap[3].to_string(),
                },
            });
        }

        // DROP TABLE
        let re_drop_table = Regex::new(r"(?i)DROP\s+TABLE\s+(\w+)").unwrap();
        for cap in re_drop_table.captures_iter(content) {
            changes.push(SchemaChange::DropTable {
                table: cap[1].to_string(),
            });
        }

        changes
    }

    fn parse_rust(&self, content: &str) -> Vec<SchemaChange> {
        let mut changes = Vec::new();

        // Heuristic for diesel table! macro or similar
        let re_table_macro = Regex::new(r"table!\s*\{\s*(\w+)\s*\(([\s\S]+?)\)").unwrap();
        for cap in re_table_macro.captures_iter(content) {
            let table = cap[1].to_string();
            let col_defs = &cap[2];
            let mut columns = Vec::new();
            // diesel: name -> type,
            for col_line in col_defs.split(',') {
                if let Some((name, typ)) = col_line.split_once("->") {
                    columns.push(ColumnInfo {
                        name: name.trim().to_string(),
                        data_type: typ.trim().to_string(),
                    });
                }
            }
            changes.push(SchemaChange::CreateTable { table, columns });
        }

        changes
    }
}
