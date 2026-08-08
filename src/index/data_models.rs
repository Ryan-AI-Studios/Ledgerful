use crate::index::languages;
use crate::index::symbols::{Symbol, SymbolKind};
use crate::state::storage::StorageManager;
use miette::{IntoDiagnostic, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use tracing::{info, warn};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ModelKind {
    Struct,
    Interface,
    Class,
    Schema,
    Generated,
}

impl ModelKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            ModelKind::Struct => "STRUCT",
            ModelKind::Interface => "INTERFACE",
            ModelKind::Class => "CLASS",
            ModelKind::Schema => "SCHEMA",
            ModelKind::Generated => "GENERATED",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractedModel {
    pub model_name: String,
    pub language: String,
    pub model_kind: ModelKind,
    pub confidence: f64,
    pub evidence: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataModelStats {
    pub total_models: usize,
    pub files_processed: usize,
    /// Source-language files skipped due to read or parse failure.
    /// Default-safe for older serialized consumers.
    #[serde(default)]
    pub files_skipped: usize,
    /// True when `files_skipped > 0` (inventory rebuild was suppressed).
    #[serde(default)]
    pub partial: bool,
}

pub struct DataModelExtractor<'a> {
    storage: &'a StorageManager,
    repo_path: PathBuf,
}

const DATA_MODEL_BATCH_SIZE: usize = 500;

impl<'a> DataModelExtractor<'a> {
    pub fn new(storage: &'a StorageManager, repo_path: PathBuf) -> Self {
        Self { storage, repo_path }
    }

    pub fn extract(&self) -> Result<DataModelStats> {
        let conn = self.storage.get_connection();

        // 1. Query project_files
        let mut file_stmt = conn
            .prepare(
                "SELECT id, file_path, language FROM project_files WHERE parse_status != 'DELETED'",
            )
            .into_diagnostic()?;

        let file_rows: Vec<(i64, String, Option<String>)> = file_stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            })
            .into_diagnostic()?
            .collect::<Result<Vec<_>, _>>()
            .into_diagnostic()?;

        drop(file_stmt);

        // 2. Query project_symbols
        let mut sym_stmt = conn
            .prepare("SELECT id, file_id, symbol_name, symbol_kind, is_public FROM project_symbols")
            .into_diagnostic()?;

        let symbol_rows: Vec<(i64, i64, String, String, bool)> = sym_stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, i32>(4)? != 0,
                ))
            })
            .into_diagnostic()?
            .collect::<Result<Vec<_>, _>>()
            .into_diagnostic()?;

        drop(sym_stmt);

        // Full extract always rebuilds `data_models` (clear-before-insert). Empty
        // symbols still DELETE so re-extract does not leave stale stacked rows.
        if symbol_rows.is_empty() {
            conn.execute("DELETE FROM data_models", [])
                .into_diagnostic()?;
            info!(
                "No project symbols indexed; cleared data_models and skipping data model extraction."
            );
            return Ok(DataModelStats {
                total_models: 0,
                files_processed: 0,
                files_skipped: 0,
                partial: false,
            });
        }

        // 3. Build symbols_by_file map
        let mut symbols_by_file: HashMap<i64, Vec<Symbol>> = HashMap::new();
        for (_sym_id, file_id, name, kind, is_public) in &symbol_rows {
            let symbol = Symbol {
                name: name.clone(),
                kind: parse_symbol_kind(kind),
                is_public: *is_public,
                cognitive_complexity: None,
                cyclomatic_complexity: None,
                line_start: None,
                line_end: None,
                qualified_name: None,
                byte_start: None,
                byte_end: None,
                entrypoint_kind: None,
                metadata: std::collections::BTreeMap::new(),
            };

            symbols_by_file.entry(*file_id).or_default().push(symbol);
        }

        // 4. Query project_topology for Generated directory roles
        let mut topo_stmt = conn
            .prepare("SELECT dir_path, role FROM project_topology WHERE role = 'GENERATED'")
            .into_diagnostic()?;

        let generated_dirs: Vec<String> = topo_stmt
            .query_map([], |row| row.get::<_, String>(0))
            .into_diagnostic()?
            .filter_map(|r| r.ok())
            .collect();

        drop(topo_stmt);

        // 5. Walk source files and accumulate models (no DB writes yet).
        let mut files_processed = 0usize;
        let mut files_skipped = 0usize;
        let mut all_models: Vec<ModelRow> = Vec::new();

        for (file_id, file_path, language) in &file_rows {
            // Skip non-source language files
            if !matches!(
                language.as_deref(),
                Some("Rust")
                    | Some("TypeScript")
                    | Some("JavaScript")
                    | Some("Python")
                    | Some("Go")
            ) {
                continue;
            }

            let full_path = self.repo_path.join(file_path);
            let content = match std::fs::read_to_string(&full_path) {
                Ok(c) => c,
                Err(e) => {
                    files_skipped += 1;
                    warn!(
                        path = %file_path,
                        error = %e,
                        "skipping data model extract: file unreadable"
                    );
                    continue;
                }
            };

            let path = PathBuf::from(file_path);
            let file_symbols = symbols_by_file.get(file_id).cloned().unwrap_or_default();

            let extracted_models =
                match languages::extract_data_models(&path, &content, &file_symbols) {
                    Ok(m) => m,
                    Err(e) => {
                        files_skipped += 1;
                        warn!(
                            path = %file_path,
                            error = %e,
                            "skipping data model extract: parse failed"
                        );
                        continue;
                    }
                };

            // Ok([]) is NOT a skip — file is fine, just no models.
            files_processed += 1;

            for model in &extracted_models {
                // Check if file is in a Generated directory
                let (model_kind, confidence) = if is_in_generated_dir(file_path, &generated_dirs) {
                    (ModelKind::Generated, 0.6)
                } else {
                    (model.model_kind.clone(), model.confidence)
                };

                all_models.push(ModelRow {
                    model_name: model.model_name.clone(),
                    model_file_id: *file_id,
                    language: model.language.clone(),
                    model_kind: model_kind.as_str().to_string(),
                    confidence,
                    evidence: Some(model.evidence.clone()),
                });
            }
        }

        // Fail-safe inventory: a partial walk must not DELETE good rows.
        // Leave `data_models` untouched and report existing count + skip honesty.
        if files_skipped > 0 {
            let existing_count: i64 = conn
                .query_row("SELECT COUNT(*) FROM data_models", [], |row| row.get(0))
                .into_diagnostic()?;
            warn!(
                files_skipped,
                files_processed,
                existing_models = existing_count,
                "data model extract partial: leaving data_models untouched (no atomic rebuild)"
            );
            return Ok(DataModelStats {
                total_models: existing_count as usize,
                files_processed,
                files_skipped,
                partial: true,
            });
        }

        // 6. Atomic rebuild: one outer transaction DELETE + batched inserts.
        //    Crash during the file walk leaves the previous table intact; crash
        //    mid-tx rolls back to the pre-extract state. Only when every source
        //    file was readable and parseable.
        let total_models = all_models.len();
        {
            let tx = conn.unchecked_transaction().into_diagnostic()?;
            tx.execute("DELETE FROM data_models", [])
                .into_diagnostic()?;
            for chunk in all_models.chunks(DATA_MODEL_BATCH_SIZE) {
                Self::insert_model_batch(&tx, chunk)?;
            }
            tx.commit().into_diagnostic()?;
        }

        info!(
            "Data model extraction complete: {} models from {} files",
            total_models, files_processed
        );

        Ok(DataModelStats {
            total_models,
            files_processed,
            files_skipped: 0,
            partial: false,
        })
    }

    /// Insert a batch of models on an open transaction (no commit).
    fn insert_model_batch(tx: &rusqlite::Transaction<'_>, models: &[ModelRow]) -> Result<()> {
        let now = chrono::Utc::now().to_rfc3339();

        for model in models {
            tx.execute(
                "INSERT INTO data_models \
                 (model_name, model_file_id, language, model_kind, confidence, evidence, last_indexed_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                rusqlite::params![
                    model.model_name,
                    model.model_file_id,
                    model.language,
                    model.model_kind,
                    model.confidence,
                    model.evidence,
                    now,
                ],
            )
            .into_diagnostic()?;
        }

        Ok(())
    }

    /// Delete data models belonging to specific file IDs, for incremental re-indexing.
    /// Kept for tests / API symmetry — production full extract uses atomic rebuild;
    /// incremental per-file cleanup goes through `delete_file_index_dependents`.
    pub fn clear_data_models(&self, file_ids: &[i64]) -> Result<()> {
        if file_ids.is_empty() {
            return Ok(());
        }

        let conn = self.storage.get_connection();
        for &fid in file_ids {
            conn.execute("DELETE FROM data_models WHERE model_file_id = ?1", [fid])
                .into_diagnostic()?;
        }
        Ok(())
    }
}

/// Internal struct for accumulating model rows before batch insert.
struct ModelRow {
    model_name: String,
    model_file_id: i64,
    language: String,
    model_kind: String,
    confidence: f64,
    evidence: Option<String>,
}

/// Helper: parse a symbol_kind string from the DB into a SymbolKind enum.
fn parse_symbol_kind(kind: &str) -> SymbolKind {
    match kind {
        "Function" => SymbolKind::Function,
        "Method" => SymbolKind::Method,
        "Class" => SymbolKind::Class,
        "Struct" => SymbolKind::Struct,
        "Enum" => SymbolKind::Enum,
        "Trait" => SymbolKind::Trait,
        "Interface" => SymbolKind::Interface,
        "Type" => SymbolKind::Type,
        "Variable" => SymbolKind::Variable,
        "Constant" => SymbolKind::Constant,
        "Module" => SymbolKind::Module,
        _ => SymbolKind::Function,
    }
}

/// Check whether a file path falls within any Generated-classified directory.
fn is_in_generated_dir(file_path: &str, generated_dirs: &[String]) -> bool {
    // Normalize path separators
    let normalized = file_path.replace('\\', "/");
    for gen_dir in generated_dirs {
        let gen_normalized = gen_dir.replace('\\', "/");
        if normalized.starts_with(&gen_normalized)
            || normalized.contains(&format!("/{gen_normalized}"))
        {
            return true;
        }
    }
    false
}

// --- Tests ---

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::migrations::get_migrations;
    use rusqlite::Connection;

    fn in_memory_storage() -> StorageManager {
        let conn = Connection::open_in_memory().unwrap();
        let mut conn = conn;
        get_migrations().to_latest(&mut conn).unwrap();
        StorageManager::init_from_conn(conn)
    }

    #[test]
    fn test_data_model_stats_serialization() {
        let stats = DataModelStats {
            total_models: 10,
            files_processed: 5,
            files_skipped: 0,
            partial: false,
        };
        let json = serde_json::to_string(&stats).unwrap();
        assert!(json.contains("total_models"));
        assert!(json.contains("files_processed"));
        assert!(json.contains("files_skipped"));
        assert!(json.contains("partial"));

        // Older payloads without the new fields still deserialize (serde default).
        let legacy = r#"{"total_models":1,"files_processed":0}"#;
        let decoded: DataModelStats = serde_json::from_str(legacy).unwrap();
        assert_eq!(decoded.files_skipped, 0);
        assert!(!decoded.partial);
    }

    #[test]
    fn test_model_kind_as_str() {
        assert_eq!(ModelKind::Struct.as_str(), "STRUCT");
        assert_eq!(ModelKind::Interface.as_str(), "INTERFACE");
        assert_eq!(ModelKind::Class.as_str(), "CLASS");
        assert_eq!(ModelKind::Schema.as_str(), "SCHEMA");
        assert_eq!(ModelKind::Generated.as_str(), "GENERATED");
    }

    #[test]
    fn test_is_in_generated_dir() {
        let dirs = vec!["dist".to_string(), "build".to_string()];
        assert!(is_in_generated_dir("dist/models.ts", &dirs));
        assert!(is_in_generated_dir("src/dist/models.ts", &dirs));
        assert!(!is_in_generated_dir("src/models/user.ts", &dirs));
    }

    #[test]
    fn test_extracted_model_fields() {
        let model = ExtractedModel {
            model_name: "User".to_string(),
            language: "Rust".to_string(),
            model_kind: ModelKind::Struct,
            confidence: 1.0,
            evidence: "derive: Serialize, Deserialize".to_string(),
        };
        assert_eq!(model.model_name, "User");
        assert_eq!(model.language, "Rust");
        assert_eq!(model.model_kind, ModelKind::Struct);
        assert!((model.confidence - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_clear_data_models_by_file_ids() {
        let storage = in_memory_storage();
        let conn = storage.get_connection();

        // Insert a project_file
        conn.execute(
            "INSERT INTO project_files (file_path, language, content_hash, file_size, last_indexed_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            ("src/models/user.rs", "Rust", "hash1", 100, "2026-05-01T00:00:00Z"),
        ).unwrap();
        let file_id = conn.last_insert_rowid();

        // Insert a data model
        conn.execute(
            "INSERT INTO data_models (model_name, model_file_id, language, model_kind, confidence, evidence, last_indexed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params!["User", file_id, "Rust", "STRUCT", 1.0_f64, "derive: Serialize", "2026-05-01T00:00:00Z"],
        ).unwrap();

        // Verify model exists
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM data_models", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1);

        // Clear data models for the file
        let extractor = DataModelExtractor::new(&storage, PathBuf::from("/tmp/test_repo"));
        extractor.clear_data_models(&[file_id]).unwrap();

        // Verify model was deleted
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM data_models", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn test_clear_data_models_empty_ids() {
        let storage = in_memory_storage();
        let extractor = DataModelExtractor::new(&storage, PathBuf::from("/tmp/test_repo"));
        let count_before: i64 = storage
            .get_connection()
            .query_row("SELECT COUNT(*) FROM data_models", [], |row| row.get(0))
            .unwrap();
        // Should be a no-op
        extractor.clear_data_models(&[]).unwrap();
        let count_after: i64 = storage
            .get_connection()
            .query_row("SELECT COUNT(*) FROM data_models", [], |row| row.get(0))
            .unwrap();
        assert_eq!(
            count_before, count_after,
            "clear_data_models with empty IDs must be a no-op"
        );
    }

    #[test]
    fn test_parse_symbol_kind_data_models() {
        assert_eq!(parse_symbol_kind("Function"), SymbolKind::Function);
        assert_eq!(parse_symbol_kind("Class"), SymbolKind::Class);
        assert_eq!(parse_symbol_kind("Struct"), SymbolKind::Struct);
        assert_eq!(parse_symbol_kind("Interface"), SymbolKind::Interface);
        assert_eq!(parse_symbol_kind("Unknown"), SymbolKind::Function);
    }

    #[test]
    fn test_full_pipeline_rust_data_model_extraction() {
        use std::fs;

        // 1. Create a temporary directory with a Rust file that has a Serde struct
        let dir = tempfile::tempdir().expect("failed to create temp dir");
        let models_dir = dir.path().join("src").join("models");
        fs::create_dir_all(&models_dir).expect("failed to create models dir");

        let models_content = r#"use sqlx::FromRow;

#[derive(FromRow)]
pub struct User {
    pub id: i64,
    pub name: String,
}
"#;
        let models_path = models_dir.join("user.rs");
        fs::write(&models_path, models_content).expect("failed to write user.rs");

        // 2. Create an in-memory DB with migrations applied
        let storage = in_memory_storage();

        // 3. Insert project_files and project_symbols entries
        let conn = storage.get_connection();
        conn.execute(
            "INSERT INTO project_files (file_path, language, content_hash, file_size, parse_status, last_indexed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            ("src/models/user.rs", "Rust", "hash_e2e_dm", 200, "OK", "2026-05-01T00:00:00Z"),
        ).unwrap();
        let file_id = conn.last_insert_rowid();

        conn.execute(
            "INSERT INTO project_symbols (file_id, qualified_name, symbol_name, symbol_kind, is_public, confidence, last_indexed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![file_id, "crate::models::User", "User", "Struct", 1, 1.0, "2026-05-01T00:00:00Z"],
        ).unwrap();

        // 4. Run DataModelExtractor::extract()
        let extractor = DataModelExtractor::new(&storage, dir.path().to_path_buf());
        let stats = extractor.extract().expect("data model extraction failed");

        // 5. Verify stats
        assert!(
            stats.files_processed >= 1,
            "expected at least 1 file processed"
        );
        assert!(
            stats.total_models >= 1,
            "expected at least 1 model extracted"
        );
        assert_eq!(stats.files_skipped, 0);
        assert!(!stats.partial);

        // 6. Verify data_models table has entries
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM data_models", [], |row| row.get(0))
            .unwrap();
        assert!(
            count >= 1,
            "expected at least 1 model in data_models table, got {}",
            count
        );
    }

    /// C3: two consecutive extracts must not stack; COUNT == unique models and
    /// stats equal across runs. Also proves re-extract clears a manually stacked
    /// duplicate identity (same name/lang/kind/file_id, different last_indexed_at).
    #[test]
    fn test_extract_twice_does_not_stack_models() {
        use std::fs;

        let dir = tempfile::tempdir().expect("failed to create temp dir");
        let models_dir = dir.path().join("src").join("models");
        fs::create_dir_all(&models_dir).expect("failed to create models dir");

        let models_content = r#"use sqlx::FromRow;

#[derive(FromRow)]
pub struct User {
    pub id: i64,
    pub name: String,
}
"#;
        fs::write(models_dir.join("user.rs"), models_content).expect("failed to write user.rs");

        let storage = in_memory_storage();
        let conn = storage.get_connection();
        conn.execute(
            "INSERT INTO project_files (file_path, language, content_hash, file_size, parse_status, last_indexed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            ("src/models/user.rs", "Rust", "hash_e2e_dm2", 200, "OK", "2026-05-01T00:00:00Z"),
        )
        .unwrap();
        let file_id = conn.last_insert_rowid();

        conn.execute(
            "INSERT INTO project_symbols (file_id, qualified_name, symbol_name, symbol_kind, is_public, confidence, last_indexed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![file_id, "crate::models::User", "User", "Struct", 1, 1.0, "2026-05-01T00:00:00Z"],
        )
        .unwrap();

        let extractor = DataModelExtractor::new(&storage, dir.path().to_path_buf());
        let stats1 = extractor.extract().expect("first extract failed");
        let count1: i64 = conn
            .query_row("SELECT COUNT(*) FROM data_models", [], |row| row.get(0))
            .unwrap();
        assert_eq!(
            count1 as usize, stats1.total_models,
            "after first extract COUNT(*) must equal unique models"
        );
        assert!(
            count1 >= 1,
            "expected at least one model after first extract"
        );

        // Manually stack a duplicate identity (legacy multi-pass residue).
        // Same model_name/language/model_kind/model_file_id; different last_indexed_at.
        let (model_name, language, model_kind, confidence, evidence): (
            String,
            String,
            String,
            f64,
            Option<String>,
        ) = conn
            .query_row(
                "SELECT model_name, language, model_kind, confidence, evidence \
                 FROM data_models LIMIT 1",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .unwrap();
        conn.execute(
            "INSERT INTO data_models \
             (model_name, model_file_id, language, model_kind, confidence, evidence, last_indexed_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                model_name,
                file_id,
                language,
                model_kind,
                confidence,
                evidence,
                "2099-01-01T00:00:00Z",
            ],
        )
        .unwrap();

        let stacked_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM data_models", [], |row| row.get(0))
            .unwrap();
        assert_eq!(
            stacked_count,
            count1 + 1,
            "manual stack must leave unique+1 rows (count1={count1}, stacked={stacked_count})"
        );

        let stats2 = extractor.extract().expect("second extract failed");
        let count2: i64 = conn
            .query_row("SELECT COUNT(*) FROM data_models", [], |row| row.get(0))
            .unwrap();

        assert_eq!(
            count2 as usize, stats2.total_models,
            "COUNT(*) must equal unique models reported in stats after re-extract"
        );
        assert!(
            count2 < stacked_count,
            "re-extract must clear the stacked intermediate (count2={count2} < stacked={stacked_count})"
        );
        assert_eq!(
            count1, count2,
            "re-extract must restore unique count (count1={count1}, count2={count2})"
        );
        assert_eq!(stats1.total_models, stats2.total_models);
        assert_eq!(stats1.files_processed, stats2.files_processed);
        assert_eq!(stats1.files_skipped, 0);
        assert!(!stats1.partial);
        assert_eq!(stats2.files_skipped, 0);
        assert!(!stats2.partial);

        // DISTINCT-style uniqueness: no remaining identity stacks.
        let distinct_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM (
                    SELECT model_name, language, model_kind, model_file_id
                    FROM data_models
                    GROUP BY model_name, language, model_kind, model_file_id
                )",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            distinct_count, count2,
            "after re-extract, DISTINCT identities must equal raw COUNT(*)"
        );
    }

    /// Empty-symbol extract must DELETE prior rows (no stale stacks left behind).
    #[test]
    fn test_extract_empty_symbols_clears_stale_models() {
        let storage = in_memory_storage();
        let conn = storage.get_connection();

        conn.execute(
            "INSERT INTO project_files (file_path, language, content_hash, file_size, last_indexed_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            ("src/models/user.rs", "Rust", "hash1", 100, "2026-05-01T00:00:00Z"),
        )
        .unwrap();
        let file_id = conn.last_insert_rowid();

        conn.execute(
            "INSERT INTO data_models (model_name, model_file_id, language, model_kind, confidence, evidence, last_indexed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                "Stale",
                file_id,
                "Rust",
                "STRUCT",
                1.0_f64,
                "stale",
                "2026-05-01T00:00:00Z"
            ],
        )
        .unwrap();

        let before: i64 = conn
            .query_row("SELECT COUNT(*) FROM data_models", [], |row| row.get(0))
            .unwrap();
        assert_eq!(before, 1);

        // No project_symbols → empty-symbols path must DELETE.
        let extractor = DataModelExtractor::new(&storage, PathBuf::from("/tmp/test_repo"));
        let stats = extractor.extract().unwrap();
        assert_eq!(stats.total_models, 0);
        assert_eq!(stats.files_processed, 0);
        assert_eq!(stats.files_skipped, 0);
        assert!(!stats.partial);

        let after: i64 = conn
            .query_row("SELECT COUNT(*) FROM data_models", [], |row| row.get(0))
            .unwrap();
        assert_eq!(after, 0, "empty-symbols extract must clear data_models");
    }

    /// Partial walk (unreadable source file) must not wipe existing `data_models`.
    #[test]
    fn test_extract_skips_do_not_wipe_existing_models() {
        let storage = in_memory_storage();
        let conn = storage.get_connection();

        // project_file points at a path that does not exist on disk.
        conn.execute(
            "INSERT INTO project_files (file_path, language, content_hash, file_size, last_indexed_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            (
                "src/missing_models.rs",
                "Rust",
                "hash_missing",
                100,
                "2026-05-01T00:00:00Z",
            ),
        )
        .unwrap();
        let file_id = conn.last_insert_rowid();

        // Symbols present so we do not take the empty-symbols early DELETE path.
        conn.execute(
            "INSERT INTO project_symbols (file_id, qualified_name, symbol_name, symbol_kind, last_indexed_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            (
                file_id,
                "crate::User",
                "User",
                "Struct",
                "2026-05-01T00:00:00Z",
            ),
        )
        .unwrap();

        conn.execute(
            "INSERT INTO data_models (model_name, model_file_id, language, model_kind, confidence, evidence, last_indexed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                "KeepMe",
                file_id,
                "Rust",
                "STRUCT",
                1.0_f64,
                "keep",
                "2026-05-01T00:00:00Z"
            ],
        )
        .unwrap();

        let before: i64 = conn
            .query_row("SELECT COUNT(*) FROM data_models", [], |row| row.get(0))
            .unwrap();
        assert_eq!(before, 1);

        // Repo root has no source files → read fails → files_skipped > 0.
        let extractor =
            DataModelExtractor::new(&storage, PathBuf::from("/tmp/ledgerful_no_such_repo"));
        let stats = extractor.extract().unwrap();
        assert!(
            stats.files_skipped > 0,
            "missing on-disk file must count as skip"
        );
        assert!(stats.partial);
        assert_eq!(
            stats.total_models, before as usize,
            "total_models should report existing table count"
        );

        let after: i64 = conn
            .query_row("SELECT COUNT(*) FROM data_models", [], |row| row.get(0))
            .unwrap();
        assert_eq!(
            after, before,
            "partial extract must leave data_models untouched"
        );

        let kept_name: String = conn
            .query_row(
                "SELECT model_name FROM data_models WHERE model_name = 'KeepMe'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(kept_name, "KeepMe");
    }
}
