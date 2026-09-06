use crate::index::content_cache::load_source_content;
use crate::state::storage::StorageManager;
use miette::{IntoDiagnostic, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tracing::info;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TestMappingStats {
    pub total_mappings: usize,
    pub import_mappings: usize,
    pub naming_convention_mappings: usize,
    pub same_file_mappings: usize,
    pub coverage_mappings: usize,
    pub files_processed: usize,
}

struct TestMappingRow {
    test_symbol_id: Option<i64>,
    test_file_id: i64,
    tested_symbol_id: Option<i64>,
    tested_file_id: Option<i64>,
    confidence: f64,
    mapping_kind: String,
    evidence: Option<String>,
}

struct IndexedFunction {
    id: i64,
    name: String,
    file_id: i64,
    path: String,
    language: Option<String>,
    metadata: Option<String>,
}

impl IndexedFunction {
    fn is_test(&self) -> bool {
        is_test_function(&self.name, &self.path, self.language.as_deref())
            || metadata_marks_test(self.metadata.as_deref())
    }
}

const TEST_MAPPING_BATCH_SIZE: usize = 500;

pub struct TestMapper<'a> {
    storage: &'a StorageManager,
    repo_path: PathBuf,
    content_cache: Option<&'a HashMap<String, Arc<str>>>,
}

impl<'a> TestMapper<'a> {
    pub fn new(storage: &'a StorageManager, repo_path: PathBuf) -> Self {
        Self {
            storage,
            repo_path,
            content_cache: None,
        }
    }

    pub fn with_content_cache(mut self, cache: &'a HashMap<String, Arc<str>>) -> Self {
        self.content_cache = Some(cache);
        self
    }

    pub fn extract(&self) -> Result<TestMappingStats> {
        let conn = self.storage.get_connection();

        // 1. Query all function symbols (test-set SELECT includes metadata).
        let mut test_stmt = conn
            .prepare(
                "SELECT ps.id, ps.symbol_name, ps.file_id, pf.file_path, pf.language, ps.metadata
                 FROM project_symbols ps
                 JOIN project_files pf ON ps.file_id = pf.id
                 WHERE ps.symbol_kind = 'Function'
                 AND pf.parse_status != 'DELETED'",
            )
            .into_diagnostic()?;

        let all_function_rows: Vec<IndexedFunction> = test_stmt
            .query_map([], |row| {
                Ok(IndexedFunction {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    file_id: row.get(2)?,
                    path: row.get(3)?,
                    language: row.get(4)?,
                    metadata: row.get(5)?,
                })
            })
            .into_diagnostic()?
            .collect::<Result<Vec<_>, _>>()
            .into_diagnostic()?;

        drop(test_stmt);

        let test_functions: Vec<&IndexedFunction> =
            all_function_rows.iter().filter(|f| f.is_test()).collect();

        let mut functions_by_file: HashMap<i64, Vec<&IndexedFunction>> = HashMap::new();
        for f in &all_function_rows {
            functions_by_file.entry(f.file_id).or_default().push(f);
        }

        // 3. Build lookup (narrow SELECT — no metadata).
        let mut symbol_lookup: HashMap<String, Vec<(i64, i64, String)>> = HashMap::new();
        let mut sym_stmt = conn
            .prepare(
                "SELECT ps.id, ps.symbol_name, ps.file_id, ps.qualified_name
                 FROM project_symbols ps
                 JOIN project_files pf ON ps.file_id = pf.id
                 WHERE ps.symbol_kind = 'Function'
                 AND pf.parse_status != 'DELETED'",
            )
            .into_diagnostic()?;

        let symbol_rows: Vec<(i64, String, i64, String)> = sym_stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })
            .into_diagnostic()?
            .collect::<Result<Vec<_>, _>>()
            .into_diagnostic()?;

        drop(sym_stmt);

        for (id, name, file_id, qualified_name) in &symbol_rows {
            symbol_lookup.entry(name.clone()).or_default().push((
                *id,
                *file_id,
                qualified_name.clone(),
            ));
        }

        // 4. Delete existing
        {
            let conn = self.storage.get_connection();
            conn.execute("DELETE FROM test_mapping", [])
                .into_diagnostic()?;
        }

        let mut total_mappings = 0usize;
        let mut import_mappings = 0usize;
        let mut naming_convention_mappings = 0usize;
        let mut same_file_mappings = 0usize;
        let mut batch: Vec<TestMappingRow> = Vec::new();
        let mut processed_test_files: std::collections::HashSet<i64> =
            std::collections::HashSet::new();
        let mut content_by_path: HashMap<String, Option<Arc<str>>> = HashMap::new();

        for test in &test_functions {
            processed_test_files.insert(test.file_id);
            let key = test.path.replace('\\', "/");
            let content = match content_by_path.entry(key) {
                std::collections::hash_map::Entry::Occupied(e) => e.get().clone(),
                std::collections::hash_map::Entry::Vacant(e) => {
                    let loaded =
                        load_source_content(self.content_cache, &test.path, &self.repo_path).ok();
                    e.insert(loaded).clone()
                }
            };

            if let Some(content) = content {
                let imported_names = extract_imported_names(content.as_ref(), &test.path);
                for imported in &imported_names {
                    if let Some(candidates) = symbol_lookup.get(imported) {
                        for (tested_sym_id, tested_file_id, _qn) in candidates {
                            if *tested_sym_id == test.id {
                                continue;
                            }
                            batch.push(TestMappingRow {
                                test_symbol_id: Some(test.id),
                                test_file_id: test.file_id,
                                tested_symbol_id: Some(*tested_sym_id),
                                tested_file_id: Some(*tested_file_id),
                                confidence: 1.0,
                                mapping_kind: "IMPORT".to_string(),
                                evidence: Some(format!("import: {}", imported)),
                            });
                            import_mappings += 1;
                            if batch.len() >= TEST_MAPPING_BATCH_SIZE {
                                total_mappings += batch.len();
                                self.insert_batch(&batch)?;
                                batch.clear();
                            }
                        }
                    }
                }
            }

            if is_same_file_eligible_test_path(&test.path)
                && let Some(siblings) = functions_by_file.get(&test.file_id)
            {
                for prod in siblings {
                    if prod.id == test.id {
                        continue;
                    }
                    if prod.is_test() {
                        continue;
                    }
                    if prod.name == "default" {
                        continue;
                    }
                    batch.push(TestMappingRow {
                        test_symbol_id: Some(test.id),
                        test_file_id: test.file_id,
                        tested_symbol_id: Some(prod.id),
                        tested_file_id: Some(test.file_id),
                        confidence: 0.7,
                        mapping_kind: "SAME_FILE".to_string(),
                        evidence: Some(format!("same_file: {} -> {}", test.name, prod.name)),
                    });
                    same_file_mappings += 1;
                    if batch.len() >= TEST_MAPPING_BATCH_SIZE {
                        total_mappings += batch.len();
                        self.insert_batch(&batch)?;
                        batch.clear();
                    }
                }
            }

            let stripped_name = strip_test_prefix(&test.name);
            if stripped_name != test.name.as_str()
                && let Some(candidates) = symbol_lookup.get(stripped_name)
            {
                for (tested_sym_id, tested_file_id, _qn) in candidates {
                    if *tested_sym_id == test.id {
                        continue;
                    }
                    batch.push(TestMappingRow {
                        test_symbol_id: Some(test.id),
                        test_file_id: test.file_id,
                        tested_symbol_id: Some(*tested_sym_id),
                        tested_file_id: Some(*tested_file_id),
                        confidence: 0.5,
                        mapping_kind: "NAMING_CONVENTION".to_string(),
                        evidence: Some(format!("naming: {} -> {}", test.name, stripped_name)),
                    });
                    naming_convention_mappings += 1;
                    if batch.len() >= TEST_MAPPING_BATCH_SIZE {
                        total_mappings += batch.len();
                        self.insert_batch(&batch)?;
                        batch.clear();
                    }
                }
            }
        }

        // 6. LCOV
        let mut coverage_mappings = 0;
        if let Ok(lcov_stats) = self.import_lcov_if_present(&mut batch) {
            coverage_mappings = lcov_stats.coverage_mappings;
            total_mappings += lcov_stats.total_mappings;
        }

        if !batch.is_empty() {
            total_mappings += batch.len();
            self.insert_batch(&batch)?;
        }

        Ok(TestMappingStats {
            total_mappings,
            import_mappings,
            naming_convention_mappings,
            same_file_mappings,
            coverage_mappings,
            files_processed: processed_test_files.len(),
        })
    }

    fn import_lcov_if_present(&self, batch: &mut Vec<TestMappingRow>) -> Result<TestMappingStats> {
        let lcov_path = self.repo_path.join("lcov.info");
        if !lcov_path.exists() {
            return Ok(TestMappingStats {
                total_mappings: 0,
                import_mappings: 0,
                naming_convention_mappings: 0,
                same_file_mappings: 0,
                coverage_mappings: 0,
                files_processed: 0,
            });
        }

        info!("Importing coverage from {}", lcov_path.display());
        let content = std::fs::read_to_string(&lcov_path).into_diagnostic()?;

        let mut current_file_path = None;
        let mut mappings = 0;
        let conn = self.storage.get_connection();

        let test_file_id: i64 = match conn.query_row(
            "SELECT id FROM project_files WHERE file_path = 'lcov.info'",
            [],
            |row| row.get(0),
        ) {
            Ok(id) => id,
            Err(_) => {
                conn.execute(
                    "INSERT INTO project_files (file_path, language, parse_status) VALUES ('lcov.info', 'LCOV', 'OK')",
                    []
                ).into_diagnostic()?;
                conn.last_insert_rowid()
            }
        };

        let mut file_id_cache: HashMap<String, i64> = HashMap::new();

        for line in content.lines() {
            if let Some(stripped) = line.strip_prefix("SF:") {
                current_file_path = Some(stripped.to_string().replace('\\', "/"));
            } else if line == "end_of_record"
                && let Some(path) = current_file_path.take()
            {
                let file_id = if let Some(&id) = file_id_cache.get(&path) {
                    Some(id)
                } else {
                    let id: Option<i64> = conn
                        .query_row(
                            "SELECT id FROM project_files WHERE file_path = ?1",
                            [path.as_str()],
                            |row| row.get(0),
                        )
                        .ok();
                    if let Some(i) = id {
                        file_id_cache.insert(path, i);
                    }
                    id
                };

                if let Some(fid) = file_id {
                    batch.push(TestMappingRow {
                        test_symbol_id: None,
                        test_file_id,
                        tested_symbol_id: None,
                        tested_file_id: Some(fid),
                        confidence: 0.9,
                        mapping_kind: "COVERAGE".to_string(),
                        evidence: Some("lcov.info".to_string()),
                    });
                    mappings += 1;
                }
            }
        }

        Ok(TestMappingStats {
            total_mappings: mappings,
            import_mappings: 0,
            naming_convention_mappings: 0,
            same_file_mappings: 0,
            coverage_mappings: mappings,
            files_processed: 1,
        })
    }

    fn insert_batch(&self, rows: &[TestMappingRow]) -> Result<()> {
        let conn = self.storage.get_connection();
        let tx = conn.unchecked_transaction().into_diagnostic()?;
        let now = chrono::Utc::now().to_rfc3339();

        for row in rows {
            tx.execute(
                "INSERT OR IGNORE INTO test_mapping \
                 (test_symbol_id, test_file_id, tested_symbol_id, tested_file_id, confidence, mapping_kind, evidence, last_indexed_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                rusqlite::params![
                    row.test_symbol_id,
                    row.test_file_id,
                    row.tested_symbol_id,
                    row.tested_file_id,
                    row.confidence,
                    row.mapping_kind,
                    row.evidence,
                    now,
                ],
            )
            .into_diagnostic()?;
        }

        tx.commit().into_diagnostic()?;
        Ok(())
    }
}

/// Path-only test-file heuristic (language-independent).
///
/// Works with `language = None`. Includes directory layouts (`tests/`,
/// `__tests__/`, …), co-located suffixes (`*_test.rs`, `*.test.ts`,
/// `*_test.py`), and Go `*_test.go` / `_test.go` sibling convention.
pub(crate) fn is_test_path(path: &str) -> bool {
    let normalized_path = path.replace('\\', "/");
    if normalized_path.starts_with("tests/")
        || normalized_path.starts_with("test/")
        || normalized_path.contains("/tests/")
        || normalized_path.contains("/test/")
        || normalized_path.contains("/__tests__/")
        || normalized_path.contains("/spec/")
    {
        return true;
    }
    if normalized_path.ends_with("_test.rs")
        || normalized_path.ends_with("_tests.rs")
        || normalized_path.ends_with(".test.ts")
        || normalized_path.ends_with(".test.tsx")
        || normalized_path.ends_with(".test.js")
        || normalized_path.ends_with(".test.jsx")
        || normalized_path.ends_with(".spec.ts")
        || normalized_path.ends_with(".spec.tsx")
        || normalized_path.ends_with(".spec.js")
        || normalized_path.ends_with(".spec.jsx")
        || normalized_path.ends_with("_test.py")
        || normalized_path.ends_with("_test.go")
    {
        return true;
    }
    // Python files named test_*.py (path segment after final /)
    if let Some(file_name) = normalized_path.rsplit('/').next()
        && file_name.starts_with("test_")
        && file_name.ends_with(".py")
    {
        return true;
    }
    false
}

/// Symbol-name (and optional language-aware) test heuristic.
///
/// Path-only classification belongs in [`is_test_path`]. Name conventions such
/// as `test_*` / `*_test` are language-agnostic; `language` is reserved for
/// future language-specific symbol rules and may be `None`.
pub(crate) fn is_test_symbol(name: &str, _path: &str, _language: Option<&str>) -> bool {
    if name.starts_with("test_") || name.ends_with("_test") {
        return true;
    }
    false
}

fn is_test_function(name: &str, file_path: &str, language: Option<&str>) -> bool {
    is_test_symbol(name, file_path, language) || is_test_path(file_path)
}

/// Product-path in-file tests only. Local copy of the 0278 picker segment
/// list — do not import `commands::test_mapping` (index ↛ commands).
fn is_same_file_eligible_test_path(path: &str) -> bool {
    if is_test_path(path) {
        return false;
    }
    let normalized = path.replace('\\', "/");
    if normalized
        .split('/')
        .filter(|seg| !seg.is_empty())
        .any(|seg| matches!(seg, "vendor" | "deps_src" | "third_party"))
    {
        return false;
    }
    let basename = normalized.rsplit('/').next().unwrap_or(normalized.as_str());
    let stem = basename.strip_suffix(".rs").unwrap_or(basename);
    stem != "test" && stem != "tests"
}

fn metadata_marks_test(raw: Option<&str>) -> bool {
    let Some(raw) = raw else {
        return false;
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(raw) else {
        return false;
    };
    matches!(value.get("test"), Some(v) if v.as_str() == Some("true"))
}

fn strip_test_prefix(name: &str) -> &str {
    if let Some(stripped) = name.strip_prefix("test_") {
        return stripped;
    }
    if let Some(stripped) = name.strip_suffix("_test") {
        return stripped;
    }
    if let Some(stripped) = name.strip_prefix("it_") {
        return stripped;
    }
    if let Some(stripped) = name.strip_prefix("should_") {
        return stripped;
    }
    name
}

fn extract_imported_names(content: &str, file_path: &str) -> Vec<String> {
    let extension = std::path::Path::new(file_path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");
    match extension {
        "rs" => extract_rust_imported_names(content),
        "ts" | "tsx" | "js" | "jsx" => extract_typescript_imported_names(content),
        "py" => extract_python_imported_names(content),
        _ => Vec::new(),
    }
}

fn extract_rust_imported_names(content: &str) -> Vec<String> {
    let mut names = Vec::new();
    for line in content.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with("use ") {
            continue;
        }
        let use_path = trimmed
            .strip_prefix("use ")
            .unwrap_or("")
            .trim()
            .trim_end_matches(';')
            .trim();
        if let Some(start) = use_path.find("::{") {
            let brace_start = start + 3;
            let group_content = if let Some(end) = use_path[brace_start..].find('}') {
                &use_path[brace_start..brace_start + end]
            } else {
                &use_path[brace_start..]
            };
            for item in group_content.split(',') {
                let name = item.trim().to_string();
                if !name.is_empty() {
                    names.push(name);
                }
            }
        } else {
            if let Some(last_segment) = use_path.rsplit("::").next() {
                let name = last_segment.trim().to_string();
                if !name.is_empty() && name != "self" && name != "super" {
                    names.push(name);
                }
            }
        }
    }
    names.sort_unstable();
    names.dedup();
    names
}

fn extract_typescript_imported_names(content: &str) -> Vec<String> {
    let mut names = Vec::new();
    for line in content.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with("import ") {
            continue;
        }
        if let Some(brace_start) = trimmed.find('{') {
            if let Some(brace_end) = trimmed.find('}') {
                let group = &trimmed[brace_start + 1..brace_end];
                for item in group.split(',') {
                    let name = item.split_whitespace().next().unwrap_or("").to_string();
                    if !name.is_empty() {
                        names.push(name);
                    }
                }
            }
        } else if trimmed.starts_with("import * as ") {
            let after_as = trimmed.strip_prefix("import * as ").unwrap_or("");
            if let Some(name) = after_as.split_whitespace().next() {
                names.push(name.to_string());
            }
        } else if trimmed.starts_with("import ") && !trimmed.contains(" from ") {
        } else {
            let parts: Vec<&str> = trimmed.split_whitespace().collect();
            if parts.len() >= 2 && parts[0] == "import" && parts[1] != "type" {
                let name = parts[1].to_string();
                if !name.is_empty() && name != "from" && name != "{" {
                    names.push(name);
                }
            }
        }
    }
    names.sort_unstable();
    names.dedup();
    names
}

fn extract_python_imported_names(content: &str) -> Vec<String> {
    let mut names = Vec::new();
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("from ") {
            if let Some(import_idx) = trimmed.find(" import ") {
                let after_import = &trimmed[import_idx + 8..];
                for item in after_import.split(',') {
                    let name = item.split_whitespace().next().unwrap_or("").to_string();
                    if !name.is_empty() {
                        names.push(name);
                    }
                }
            }
        } else if trimmed.starts_with("import ") {
            let after_import = trimmed.strip_prefix("import ").unwrap_or("");
            for item in after_import.split(',') {
                let name = item.split_whitespace().next().unwrap_or("").to_string();
                if !name.is_empty() {
                    names.push(name);
                }
            }
        }
    }
    names.sort_unstable();
    names.dedup();
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_test_function_prefix() {
        assert!(is_test_function("test_foo", "src/lib.rs", Some("Rust")));
    }

    #[test]
    fn test_is_test_symbol_name_conventions() {
        assert!(is_test_symbol("test_foo", "src/lib.rs", None));
        assert!(is_test_symbol("foo_test", "src/lib.rs", Some("Rust")));
        assert!(!is_test_symbol("execute_foo", "src/lib.rs", None));
    }

    #[test]
    fn test_is_test_path_generic_dirs() {
        assert!(is_test_path("tests/integration/cli.rs"));
        assert!(is_test_path("test/unit/foo.py"));
        assert!(is_test_path("src/__tests__/foo.ts"));
        assert!(is_test_path(r"src\__tests__\foo.ts"));
        assert!(!is_test_path("src/commands/foo.rs"));
    }

    #[test]
    fn test_is_test_path_language_suffixes_without_language() {
        assert!(is_test_path("src/foo_test.rs"));
        assert!(is_test_path("src/foo_tests.rs"));
        assert!(is_test_path("src/foo.test.ts"));
        assert!(is_test_path("src/foo.spec.tsx"));
        assert!(is_test_path("pkg/foo_test.py"));
        assert!(is_test_path("test_helpers.py"));
    }

    #[test]
    fn test_is_test_path_go_test_file() {
        assert!(is_test_path("pkg/foo_test.go"));
        assert!(is_test_path("foo_test.go"));
        assert!(is_test_path("internal/_test.go"));
        assert!(!is_test_path("pkg/foo.go"));
        assert!(!is_test_path("pkg/test_util.go"));
    }

    #[test]
    fn test_strip_test_prefix() {
        assert_eq!(strip_test_prefix("test_foo"), "foo");
    }

    #[test]
    fn test_metadata_marks_test_tolerant_parse() {
        assert!(!metadata_marks_test(None));
        assert!(!metadata_marks_test(Some("not-json")));
        assert!(!metadata_marks_test(Some("{}")));
        assert!(!metadata_marks_test(Some(r#"{"test":true}"#)));
        assert!(!metadata_marks_test(Some(r#"{"test":"false"}"#)));
        assert!(metadata_marks_test(Some(r#"{"test":"true"}"#)));
    }

    #[test]
    fn test_is_same_file_eligible_test_path_drops_tests_vendor_and_tests_rs() {
        assert!(is_same_file_eligible_test_path("src/foo.rs"));
        assert!(is_same_file_eligible_test_path("src/exec/boundary.rs"));
        assert!(!is_same_file_eligible_test_path("tests/foo.rs"));
        assert!(!is_same_file_eligible_test_path("src/verify/plan/tests.rs"));
        assert!(!is_same_file_eligible_test_path("src/test.rs"));
        assert!(!is_same_file_eligible_test_path(
            "vendor/sqlite3-src/source/sqlite3.c"
        ));
        assert!(!is_same_file_eligible_test_path("deps_src/foo.c"));
        assert!(!is_same_file_eligible_test_path("third_party/bar.c"));
        assert!(!is_same_file_eligible_test_path(
            r"vendor\sqlite3-src\source\sqlite3.c"
        ));
    }

    fn mapper_storage() -> crate::state::storage::StorageManager {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        let mut conn = conn;
        crate::state::migrations::get_migrations()
            .to_latest(&mut conn)
            .unwrap();
        crate::state::storage::StorageManager::init_from_conn(conn)
    }

    fn insert_file(conn: &rusqlite::Connection, path: &str) -> i64 {
        conn.execute(
            "INSERT INTO project_files (file_path, language, content_hash, file_size, parse_status, last_indexed_at)
             VALUES (?1, 'Rust', 'h', 100, 'OK', '2026-05-01T00:00:00Z')",
            [path],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    fn insert_fn(
        conn: &rusqlite::Connection,
        file_id: i64,
        name: &str,
        metadata: Option<&str>,
    ) -> i64 {
        conn.execute(
            "INSERT INTO project_symbols (file_id, qualified_name, symbol_name, symbol_kind, is_public, confidence, last_indexed_at, metadata)
             VALUES (?1, ?2, ?3, 'Function', 1, 1.0, '2026-05-01T00:00:00Z', ?4)",
            rusqlite::params![file_id, name, name, metadata],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    fn extract_mapper(storage: &crate::state::storage::StorageManager) -> TestMappingStats {
        TestMapper::new(storage, PathBuf::from("."))
            .extract()
            .expect("extract")
    }

    fn same_file_rows(
        storage: &crate::state::storage::StorageManager,
    ) -> Vec<(String, f64, String, String)> {
        let conn = storage.get_connection();
        let mut stmt = conn
            .prepare(
                "SELECT tm.mapping_kind, tm.confidence, t.symbol_name, p.symbol_name
                 FROM test_mapping tm
                 JOIN project_symbols t ON tm.test_symbol_id = t.id
                 JOIN project_symbols p ON tm.tested_symbol_id = p.id
                 WHERE tm.mapping_kind = 'SAME_FILE'
                 ORDER BY t.symbol_name, p.symbol_name",
            )
            .unwrap();
        stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, f64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
    }

    #[test]
    fn test_same_file_maps_in_file_unit_and_skips_default() {
        let storage = mapper_storage();
        let conn = storage.get_connection();
        let file_id = insert_file(conn, "src/foo.rs");
        insert_fn(conn, file_id, "execute", None);
        insert_fn(conn, file_id, "default", None);
        insert_fn(conn, file_id, "test_basic_execution", None);

        let stats = extract_mapper(&storage);
        assert!(stats.same_file_mappings >= 1, "stats={stats:?}");

        let rows = same_file_rows(&storage);
        assert!(
            rows.iter()
                .any(|(_, _, test, prod)| test == "test_basic_execution" && prod == "execute"),
            "expected SAME_FILE test_basic_execution -> execute, got {rows:?}"
        );
        assert!(
            rows.iter().all(|(_, _, _, prod)| prod != "default"),
            "Function default must not be a tested_symbol_id: {rows:?}"
        );
        let conn = storage.get_connection();
        let tested_file: i64 = conn
            .query_row(
                "SELECT tested_file_id FROM test_mapping WHERE mapping_kind = 'SAME_FILE' LIMIT 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(tested_file, file_id);
        let null_tested: i64 = conn
            .query_row(
                "SELECT count(*) FROM test_mapping \
                 WHERE mapping_kind = 'SAME_FILE' AND tested_symbol_id IS NULL",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            null_tested, 0,
            "SAME_FILE must never store NULL tested_symbol_id"
        );
    }

    #[test]
    fn test_same_file_skipped_for_tests_path_tests_rs_stem_and_vendor() {
        let storage = mapper_storage();
        let conn = storage.get_connection();

        let tests_id = insert_file(conn, "tests/foo.rs");
        insert_fn(conn, tests_id, "execute", None);
        insert_fn(conn, tests_id, "test_basic_execution", None);

        let tests_rs_id = insert_file(conn, "src/verify/plan/tests.rs");
        insert_fn(conn, tests_rs_id, "execute", None);
        insert_fn(conn, tests_rs_id, "test_basic_execution", None);

        let vendor_id = insert_file(conn, "vendor/sqlite3-src/source/sqlite3.c");
        insert_fn(conn, vendor_id, "test_addop_breakpoint", None);
        insert_fn(conn, vendor_id, "test_trace_breakpoint", None);
        insert_fn(conn, vendor_id, "sqlite3_exec", None);
        insert_fn(conn, vendor_id, "sqlite3_close", None);
        insert_fn(conn, vendor_id, "sqlite3_open", None);

        let stats = extract_mapper(&storage);
        assert_eq!(
            stats.same_file_mappings, 0,
            "tests/ path, tests.rs stem, and vendor must not SAME_FILE; stats={stats:?}"
        );
        assert!(
            same_file_rows(&storage).is_empty(),
            "expected zero SAME_FILE rows"
        );
    }

    #[test]
    fn test_same_file_wins_over_naming_convention_for_test_execute() {
        let storage = mapper_storage();
        let conn = storage.get_connection();
        let file_id = insert_file(conn, "src/foo.rs");
        insert_fn(conn, file_id, "execute", None);
        insert_fn(conn, file_id, "test_execute", None);

        extract_mapper(&storage);

        let conn = storage.get_connection();
        let (kind, confidence): (String, f64) = conn
            .query_row(
                "SELECT tm.mapping_kind, tm.confidence
                 FROM test_mapping tm
                 JOIN project_symbols t ON tm.test_symbol_id = t.id
                 JOIN project_symbols p ON tm.tested_symbol_id = p.id
                 WHERE t.symbol_name = 'test_execute' AND p.symbol_name = 'execute'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("row test_execute -> execute");
        assert_eq!(kind, "SAME_FILE");
        assert!(
            (confidence - 0.7).abs() < f64::EPSILON,
            "confidence={confidence}"
        );
        let naming: i64 = conn
            .query_row(
                "SELECT count(*) FROM test_mapping WHERE mapping_kind = 'NAMING_CONVENTION'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            naming, 0,
            "NAMING must lose first-wins UNIQUE to SAME_FILE for the same pair"
        );
    }

    #[test]
    fn test_same_file_maps_metadata_test_without_test_prefix() {
        let storage = mapper_storage();
        let conn = storage.get_connection();
        let file_id = insert_file(conn, "src/dto.rs");
        insert_fn(conn, file_id, "run_report", None);
        insert_fn(
            conn,
            file_id,
            "covers_from_report",
            Some(r#"{"test":"true"}"#),
        );
        insert_fn(conn, file_id, "plain_helper", None);

        extract_mapper(&storage);

        let rows = same_file_rows(&storage);
        assert!(
            rows.iter()
                .any(|(_, _, test, prod)| test == "covers_from_report" && prod == "run_report"),
            "#[test] fn without test_ prefix must SAME_FILE-map; got {rows:?}"
        );
        assert!(
            rows.iter().all(|(_, _, test, _)| test != "plain_helper"),
            "unattributed counterpart must not be a test: {rows:?}"
        );
        assert!(
            rows.iter()
                .any(|(_, _, test, prod)| test == "covers_from_report" && prod == "plain_helper"),
            "plain sibling Function is an eligible SAME_FILE target; got {rows:?}"
        );
    }

    #[test]
    fn test_same_file_zero_when_only_test_functions() {
        let storage = mapper_storage();
        let conn = storage.get_connection();
        let file_id = insert_file(conn, "src/config/model.rs");
        insert_fn(conn, file_id, "test_schema_roundtrip", None);
        insert_fn(
            conn,
            file_id,
            "covers_from_report",
            Some(r#"{"test":"true"}"#),
        );

        let stats = extract_mapper(&storage);
        assert_eq!(
            stats.same_file_mappings, 0,
            "tests with zero eligible production Functions must not SAME_FILE; stats={stats:?}"
        );
        assert!(same_file_rows(&storage).is_empty());
        let null_tested: i64 = storage
            .get_connection()
            .query_row(
                "SELECT count(*) FROM test_mapping \
                 WHERE mapping_kind = 'SAME_FILE' AND tested_symbol_id IS NULL",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(null_tested, 0);
    }
}
