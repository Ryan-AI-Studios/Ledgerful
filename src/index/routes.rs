use crate::index::languages;
use crate::index::symbols::{Symbol, SymbolKind};
use crate::state::storage::StorageManager;
use miette::{IntoDiagnostic, Result};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use tracing::info;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ExtractedRoute {
    pub method: String,
    pub path_pattern: String,
    pub handler_name: String,
    pub framework: String,
    pub route_source: String,
    pub mount_prefix: Option<String>,
    pub is_dynamic: bool,
    pub route_confidence: f64,
    pub evidence: String,
    #[serde(default)]
    pub auth_requirements: Option<Vec<String>>,
    #[serde(default)]
    pub schema_refs: Option<Vec<String>>,
    #[serde(default)]
    pub owning_service: Option<String>,
    #[serde(default)]
    pub consumers: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteStats {
    pub total_routes: usize,
    pub frameworks_detected: Vec<String>,
    pub files_processed: usize,
}

pub struct RouteExtractor<'a> {
    storage: &'a StorageManager,
    repo_path: PathBuf,
}

const ROUTE_BATCH_SIZE: usize = 500;

impl<'a> RouteExtractor<'a> {
    pub fn new(storage: &'a StorageManager, repo_path: PathBuf) -> Self {
        Self { storage, repo_path }
    }

    pub fn extract(&self) -> Result<RouteStats> {
        let conn = self.storage.get_connection();

        // 1. Query all project_files
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

        // 2. Query all project_symbols
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

        if symbol_rows.is_empty() {
            info!("No project symbols indexed; skipping route extraction.");
            return Ok(RouteStats {
                total_routes: 0,
                frameworks_detected: Vec::new(),
                files_processed: 0,
            });
        }

        // 3. Build lookup maps
        // symbols_by_file: file_id -> Vec<Symbol>
        let mut symbols_by_file: HashMap<i64, Vec<Symbol>> = HashMap::new();
        // symbol_name_to_ids: symbol_name -> Vec<(symbol_id, file_id)>
        let mut symbol_name_to_ids: HashMap<String, Vec<(i64, i64)>> = HashMap::new();

        for (sym_id, file_id, name, kind, is_public) in &symbol_rows {
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
            symbol_name_to_ids
                .entry(name.clone())
                .or_default()
                .push((*sym_id, *file_id));
        }

        // 4. Iterate over source files (Rust, TypeScript, Python)
        let mut total_routes = 0usize;
        let mut files_processed = 0usize;
        let mut frameworks: HashSet<String> = HashSet::new();
        let mut route_batch: Vec<RouteRow> = Vec::new();

        for (file_id, file_path, language) in &file_rows {
            // Skip non-source language files
            if !matches!(
                language.as_deref(),
                Some("Rust") | Some("TypeScript") | Some("JavaScript") | Some("Python")
            ) {
                continue;
            }

            let full_path = self.repo_path.join(file_path);
            let content = match std::fs::read_to_string(&full_path) {
                Ok(c) => c,
                Err(_) => continue,
            };

            let path = PathBuf::from(file_path);
            let file_symbols = symbols_by_file.get(file_id).cloned().unwrap_or_default();

            let extracted_routes = match languages::extract_routes(&path, &content, &file_symbols) {
                Ok(r) => r,
                Err(_) => continue,
            };

            files_processed += 1;

            for route in &extracted_routes {
                frameworks.insert(route.framework.clone());

                // Try to resolve the handler name against symbol_name_to_ids
                let (handler_symbol_id, handler_symbol_name) =
                    if let Some(ids) = symbol_name_to_ids.get(&route.handler_name) {
                        // Prefer symbols in the same file
                        let same_file = ids.iter().find(|(_, fid)| *fid == *file_id);
                        if let Some((sym_id, _)) = same_file {
                            (Some(*sym_id), Some(route.handler_name.clone()))
                        } else if let Some((sym_id, _)) = ids.first() {
                            (Some(*sym_id), Some(route.handler_name.clone()))
                        } else {
                            (None, Some(route.handler_name.clone()))
                        }
                    } else {
                        (None, Some(route.handler_name.clone()))
                    };

                route_batch.push(RouteRow {
                    method: route.method.clone(),
                    path_pattern: route.path_pattern.clone(),
                    handler_symbol_id,
                    handler_symbol_name,
                    handler_file_id: *file_id,
                    framework: route.framework.clone(),
                    route_source: route.route_source.clone(),
                    mount_prefix: route.mount_prefix.clone(),
                    is_dynamic: route.is_dynamic,
                    route_confidence: route.route_confidence,
                    evidence: Some(route.evidence.clone()),
                    auth_requirements: route.auth_requirements.clone(),
                    schema_refs: route.schema_refs.clone(),
                    owning_service: route.owning_service.clone(),
                    consumers: route.consumers.clone(),
                });
            }

            // 5. Batched inserts
            if route_batch.len() >= ROUTE_BATCH_SIZE {
                total_routes += route_batch.len();
                self.insert_route_batch(&route_batch)?;
                route_batch.clear();
            }
        }

        // Flush remaining routes
        if !route_batch.is_empty() {
            total_routes += route_batch.len();
            self.insert_route_batch(&route_batch)?;
        }

        let mut frameworks_detected: Vec<String> = frameworks.into_iter().collect();
        frameworks_detected.sort();

        info!(
            "Route extraction complete: {} routes from {} files, frameworks: {:?}",
            total_routes, files_processed, frameworks_detected
        );

        Ok(RouteStats {
            total_routes,
            frameworks_detected,
            files_processed,
        })
    }

    fn insert_route_batch(&self, routes: &[RouteRow]) -> Result<()> {
        let conn = self.storage.get_connection();
        let tx = conn.unchecked_transaction().into_diagnostic()?;
        let now = chrono::Utc::now().to_rfc3339();

        for route in routes {
            let auth_reqs = serde_json::to_string(&route.auth_requirements)
                .unwrap_or_else(|_| "[]".to_string());
            let schema_refs =
                serde_json::to_string(&route.schema_refs).unwrap_or_else(|_| "[]".to_string());
            let consumers =
                serde_json::to_string(&route.consumers).unwrap_or_else(|_| "[]".to_string());

            tx.execute(
                "INSERT INTO api_routes \
                 (method, path_pattern, handler_symbol_id, handler_symbol_name, handler_file_id, \
                  framework, route_source, mount_prefix, is_dynamic, route_confidence, evidence, \
                  auth_requirements, schema_refs, owning_service, consumers, last_indexed_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
                rusqlite::params![
                    route.method,
                    route.path_pattern,
                    route.handler_symbol_id,
                    route.handler_symbol_name,
                    route.handler_file_id,
                    route.framework,
                    route.route_source,
                    route.mount_prefix,
                    route.is_dynamic as i32,
                    route.route_confidence,
                    route.evidence,
                    auth_reqs,
                    schema_refs,
                    route.owning_service,
                    consumers,
                    now,
                ],
            )
            .into_diagnostic()?;
        }

        tx.commit().into_diagnostic()?;
        Ok(())
    }

    /// Delete routes belonging to specific file IDs, for incremental re-indexing.
    pub fn clear_routes(&self, file_ids: &[i64]) -> Result<()> {
        if file_ids.is_empty() {
            return Ok(());
        }

        let conn = self.storage.get_connection();
        for &fid in file_ids {
            conn.execute("DELETE FROM api_routes WHERE handler_file_id = ?1", [fid])
                .into_diagnostic()?;
        }
        Ok(())
    }
}

/// Internal struct for accumulating route rows before batch insert.
struct RouteRow {
    method: String,
    path_pattern: String,
    handler_symbol_id: Option<i64>,
    handler_symbol_name: Option<String>,
    handler_file_id: i64,
    framework: String,
    route_source: String,
    mount_prefix: Option<String>,
    is_dynamic: bool,
    route_confidence: f64,
    evidence: Option<String>,
    auth_requirements: Option<Vec<String>>,
    schema_refs: Option<Vec<String>>,
    owning_service: Option<String>,
    consumers: Option<Vec<String>>,
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
    fn test_route_stats_serialization() {
        let stats = RouteStats {
            total_routes: 5,
            frameworks_detected: vec!["Axum".to_string(), "Express".to_string()],
            files_processed: 3,
        };
        let json = serde_json::to_string(&stats).unwrap();
        assert!(json.contains("total_routes"));
        assert!(json.contains("frameworks_detected"));
        assert!(json.contains("files_processed"));
    }

    #[test]
    fn test_extracted_route_equality() {
        let r1 = ExtractedRoute {
            method: "GET".to_string(),
            path_pattern: "/users".to_string(),
            handler_name: "get_users".to_string(),
            framework: "Axum".to_string(),
            route_source: "DECORATOR".to_string(),
            mount_prefix: None,
            is_dynamic: false,
            route_confidence: 1.0,
            evidence: "test".to_string(),
            auth_requirements: None,
            schema_refs: None,
            owning_service: None,
            consumers: None,
        };
        let r2 = r1.clone();
        assert_eq!(r1, r2);
    }

    #[test]
    fn test_route_extractor_empty_symbols() {
        let storage = in_memory_storage();
        let extractor = RouteExtractor::new(&storage, PathBuf::from("/tmp/test_repo"));

        let stats = extractor.extract().unwrap();
        assert_eq!(stats.total_routes, 0);
        assert_eq!(stats.files_processed, 0);
        assert!(stats.frameworks_detected.is_empty());
    }

    #[test]
    fn test_clear_routes_by_file_ids() {
        let storage = in_memory_storage();

        let conn = storage.get_connection();

        // Insert a project_file
        conn.execute(
            "INSERT INTO project_files (file_path, language, content_hash, file_size, last_indexed_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            ("src/routes.rs", "Rust", "hash1", 100, "2026-05-01T00:00:00Z"),
        ).unwrap();
        let file_id = conn.last_insert_rowid();

        // Insert a symbol for the handler
        conn.execute(
            "INSERT INTO project_symbols (file_id, qualified_name, symbol_name, symbol_kind, last_indexed_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            (file_id, "crate::get_users", "get_users", "Function", "2026-05-01T00:00:00Z"),
        ).unwrap();
        let symbol_id = conn.last_insert_rowid();

        // Insert a route
        conn.execute(
            "INSERT INTO api_routes
                (method, path_pattern, handler_symbol_id, handler_symbol_name, handler_file_id,
                 framework, route_source, is_dynamic, route_confidence, last_indexed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            rusqlite::params![
                "GET",
                "/users",
                symbol_id,
                "get_users",
                file_id,
                "Axum",
                "DECORATOR",
                0,
                1.0,
                "2026-05-01T00:00:00Z",
            ],
        )
        .unwrap();

        // Verify route exists
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM api_routes", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1);

        // Clear routes for the file
        let extractor = RouteExtractor::new(&storage, PathBuf::from("/tmp/test_repo"));
        extractor.clear_routes(&[file_id]).unwrap();

        // Verify route was deleted
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM api_routes", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn test_clear_routes_empty_ids() {
        let storage = in_memory_storage();
        let extractor = RouteExtractor::new(&storage, PathBuf::from("/tmp/test_repo"));
        // Should be a no-op
        extractor.clear_routes(&[]).unwrap();
    }

    #[test]
    fn test_parse_symbol_kind_routes() {
        assert_eq!(parse_symbol_kind("Function"), SymbolKind::Function);
        assert_eq!(parse_symbol_kind("Method"), SymbolKind::Method);
        assert_eq!(parse_symbol_kind("Class"), SymbolKind::Class);
        assert_eq!(parse_symbol_kind("Unknown"), SymbolKind::Function);
    }

    /// E2E test: RouteExtractor with a real Rust project file that has routes.
    #[test]
    fn test_full_pipeline_rust_route_extraction() {
        use std::fs;

        // 1. Create a temporary directory with an Axum-style Rust file
        let dir = tempfile::tempdir().expect("failed to create temp dir");
        let src_dir = dir.path().join("src");
        fs::create_dir_all(&src_dir).expect("failed to create src dir");

        let routes_content = r#"use axum::Router;
use axum::routing::get;

async fn get_users() {}
async fn create_user() {}

fn app() -> Router {
    Router::new()
        .route("/users", get(get_users))
        .route("/users", axum::routing::post(create_user))
}
"#;
        let routes_path = src_dir.join("routes.rs");
        fs::write(&routes_path, routes_content).expect("failed to write routes.rs");

        // 2. Create an in-memory DB with migrations applied
        let storage = in_memory_storage();

        // 3. Insert project_files and project_symbols entries
        let conn = storage.get_connection();
        conn.execute(
            "INSERT INTO project_files (file_path, language, content_hash, file_size, last_indexed_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            ("src/routes.rs", "Rust", "hash_e2e_route", 200, "2026-05-01T00:00:00Z"),
        ).unwrap();
        let file_id = conn.last_insert_rowid();

        for (qualified, name) in [
            ("crate::get_users", "get_users"),
            ("crate::create_user", "create_user"),
        ] {
            conn.execute(
                "INSERT INTO project_symbols (file_id, qualified_name, symbol_name, symbol_kind, is_public, confidence, last_indexed_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                (file_id, qualified, name, "Function", 1, 1.0, "2026-05-01T00:00:00Z"),
            ).unwrap();
        }

        // 4. Run RouteExtractor::extract()
        let extractor = RouteExtractor::new(&storage, dir.path().to_path_buf());
        let stats = extractor.extract().expect("route extraction failed");

        // 5. Verify stats
        assert!(
            stats.files_processed >= 1,
            "expected at least 1 file processed"
        );
        assert!(
            stats.total_routes >= 1,
            "expected at least 1 route extracted"
        );

        // 6. Verify api_routes table has entries
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM api_routes", [], |row| row.get(0))
            .unwrap();
        assert!(
            count >= 1,
            "expected at least 1 route in api_routes table, got {}",
            count
        );
    }
}
