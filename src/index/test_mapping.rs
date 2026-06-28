use crate::state::storage::StorageManager;
use miette::{IntoDiagnostic, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use tracing::info;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TestMappingStats {
    pub total_mappings: usize,
    pub import_mappings: usize,
    pub naming_convention_mappings: usize,
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

const TEST_MAPPING_BATCH_SIZE: usize = 500;

pub struct TestMapper<'a> {
    storage: &'a StorageManager,
    repo_path: PathBuf,
}

impl<'a> TestMapper<'a> {
    pub fn new(storage: &'a StorageManager, repo_path: PathBuf) -> Self {
        Self { storage, repo_path }
    }

    pub fn extract(&self) -> Result<TestMappingStats> {
        let conn = self.storage.get_connection();

        // 1. Query all test symbols
        let mut test_stmt = conn
            .prepare(
                "SELECT ps.id, ps.symbol_name, ps.qualified_name, ps.file_id, pf.file_path, pf.language
                 FROM project_symbols ps
                 JOIN project_files pf ON ps.file_id = pf.id
                 WHERE ps.symbol_kind = 'Function'
                 AND pf.parse_status != 'DELETED'",
            )
            .into_diagnostic()?;

        let all_function_rows: Vec<(i64, String, String, i64, String, Option<String>)> = test_stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, Option<String>>(5)?,
                ))
            })
            .into_diagnostic()?
            .collect::<Result<Vec<_>, _>>()
            .into_diagnostic()?;

        drop(test_stmt);

        let test_functions: Vec<(i64, String, String, i64, String, Option<String>)> =
            all_function_rows
                .iter()
                .filter(|(_, name, _, _, path, language)| {
                    is_test_function(name, path, language.as_deref())
                })
                .cloned()
                .collect();

        // 3. Build lookup
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
        let mut batch: Vec<TestMappingRow> = Vec::new();
        let mut processed_test_files: std::collections::HashSet<i64> =
            std::collections::HashSet::new();

        for (test_sym_id, test_name, _qualified, test_file_id, test_file_path, _test_lang) in
            &test_functions
        {
            processed_test_files.insert(*test_file_id);
            let full_path = self.repo_path.join(test_file_path.replace('\\', "/"));
            let content = match std::fs::read_to_string(&full_path) {
                Ok(c) => c,
                Err(_) => continue,
            };

            let imported_names = extract_imported_names(&content, test_file_path);
            for imported in &imported_names {
                if let Some(candidates) = symbol_lookup.get(imported) {
                    for (tested_sym_id, tested_file_id, _qn) in candidates {
                        if *tested_sym_id == *test_sym_id {
                            continue;
                        }
                        batch.push(TestMappingRow {
                            test_symbol_id: Some(*test_sym_id),
                            test_file_id: *test_file_id,
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

            let stripped_name = strip_test_prefix(test_name);
            if stripped_name != test_name.as_str()
                && let Some(candidates) = symbol_lookup.get(stripped_name)
            {
                for (tested_sym_id, tested_file_id, _qn) in candidates {
                    if *tested_sym_id == *test_sym_id {
                        continue;
                    }
                    batch.push(TestMappingRow {
                        test_symbol_id: Some(*test_sym_id),
                        test_file_id: *test_file_id,
                        tested_symbol_id: Some(*tested_sym_id),
                        tested_file_id: Some(*tested_file_id),
                        confidence: 0.5,
                        mapping_kind: "NAMING_CONVENTION".to_string(),
                        evidence: Some(format!("naming: {} -> {}", test_name, stripped_name)),
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

fn is_test_function(name: &str, file_path: &str, language: Option<&str>) -> bool {
    if name.starts_with("test_") || name.ends_with("_test") {
        return true;
    }
    let normalized_path = file_path.replace('\\', "/");
    if language == Some("Rust")
        && (normalized_path.starts_with("tests/")
            || normalized_path.contains("/tests/")
            || normalized_path.ends_with("_test.rs")
            || normalized_path.ends_with("_tests.rs"))
    {
        return true;
    }
    if normalized_path.starts_with("tests/")
        || normalized_path.starts_with("test/")
        || normalized_path.contains("/tests/")
        || normalized_path.contains("/test/")
        || normalized_path.contains("/__tests__/")
        || normalized_path.contains("/spec/")
    {
        return true;
    }
    if (language == Some("TypeScript") || language == Some("JavaScript"))
        && (normalized_path.ends_with(".test.ts")
            || normalized_path.ends_with(".test.tsx")
            || normalized_path.ends_with(".test.js")
            || normalized_path.ends_with(".test.jsx")
            || normalized_path.ends_with(".spec.ts")
            || normalized_path.ends_with(".spec.tsx")
            || normalized_path.ends_with(".spec.js")
            || normalized_path.ends_with(".spec.jsx"))
    {
        return true;
    }
    if language == Some("Python")
        && (normalized_path.starts_with("tests/")
            || normalized_path.starts_with("test/")
            || normalized_path.contains("/tests/")
            || normalized_path.contains("/test/")
            || normalized_path.ends_with("_test.py")
            || normalized_path.starts_with("test_"))
    {
        return true;
    }
    false
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
    fn test_strip_test_prefix() {
        assert_eq!(strip_test_prefix("test_foo"), "foo");
    }
}
