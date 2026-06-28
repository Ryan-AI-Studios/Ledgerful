use crate::index::languages;
use crate::state::storage::StorageManager;
use miette::{IntoDiagnostic, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tracing::info;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub enum LogLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

impl LogLevel {
    pub fn as_str(&self) -> &'static str {
        match self {
            LogLevel::Trace => "trace",
            LogLevel::Debug => "debug",
            LogLevel::Info => "info",
            LogLevel::Warn => "warn",
            LogLevel::Error => "error",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoggingPattern {
    pub line_start: i32,
    pub level: Option<LogLevel>,
    pub framework: String,
    pub in_test: bool,
    pub confidence: f64,
    pub evidence: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorHandlingPattern {
    pub line_start: i32,
    pub level: Option<LogLevel>,
    pub framework: String,
    pub in_test: bool,
    pub confidence: f64,
    pub evidence: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelemetryPattern {
    pub line_start: i32,
    pub level: Option<LogLevel>,
    pub framework: String,
    pub in_test: bool,
    pub confidence: f64,
    pub evidence: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObservabilityStats {
    pub total_patterns: usize,
    pub error_handling_patterns: usize,
    pub telemetry_patterns: usize,
    pub files_processed: usize,
}

pub struct ObservabilityExtractor<'a> {
    storage: &'a StorageManager,
    repo_path: PathBuf,
}

const OBSERVABILITY_BATCH_SIZE: usize = 500;

impl<'a> ObservabilityExtractor<'a> {
    pub fn new(storage: &'a StorageManager, repo_path: PathBuf) -> Self {
        Self { storage, repo_path }
    }

    pub fn extract(&self) -> Result<ObservabilityStats> {
        let conn = self.storage.get_connection();

        // 1. Query project_files for all indexed source files
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

        // 2. Iterate over source files, extract logging, error handling, and telemetry patterns
        let mut total_patterns = 0usize;
        let mut error_handling_patterns = 0usize;
        let mut telemetry_patterns = 0usize;
        let mut files_processed = 0usize;
        let mut pattern_batch: Vec<ObservabilityRow> = Vec::new();

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

            // Extract logging patterns
            let log_patterns = match languages::extract_logging_patterns(&path, &content) {
                Ok(p) => p,
                Err(_) => continue,
            };

            // Extract error handling patterns
            let eh_patterns =
                languages::extract_error_handling(&path, &content).unwrap_or_default();

            // Cap at 1000 total patterns per file (LOG + ERROR_HANDLE combined)
            let log_cap = log_patterns.len().min(1000);
            let eh_cap = eh_patterns.len().min(1000usize.saturating_sub(log_cap));

            // Delete existing LOG patterns for this file before inserting
            {
                let conn = self.storage.get_connection();
                conn.execute(
                    "DELETE FROM observability_patterns WHERE file_id = ?1 AND pattern_kind = 'LOG'",
                    [file_id],
                )
                .into_diagnostic()?;
            }

            // Delete existing ERROR_HANDLE patterns for this file before inserting
            {
                let conn = self.storage.get_connection();
                conn.execute(
                    "DELETE FROM observability_patterns WHERE file_id = ?1 AND pattern_kind = 'ERROR_HANDLE'",
                    [file_id],
                )
                .into_diagnostic()?;
            }

            for pattern in log_patterns.into_iter().take(log_cap) {
                pattern_batch.push(ObservabilityRow {
                    file_id: *file_id,
                    line_start: pattern.line_start,
                    level: pattern.level.as_ref().map(|l| l.as_str().to_string()),
                    framework: pattern.framework.clone(),
                    confidence: pattern.confidence,
                    evidence: Some(pattern.evidence.clone()),
                    in_test: pattern.in_test,
                    pattern_kind: "LOG".to_string(),
                });

                // Batch inserts
                if pattern_batch.len() >= OBSERVABILITY_BATCH_SIZE {
                    total_patterns += pattern_batch.len();
                    self.insert_pattern_batch(&pattern_batch)?;
                    pattern_batch.clear();
                }
            }

            for pattern in eh_patterns.into_iter().take(eh_cap) {
                pattern_batch.push(ObservabilityRow {
                    file_id: *file_id,
                    line_start: pattern.line_start,
                    level: pattern.level.as_ref().map(|l| l.as_str().to_string()),
                    framework: pattern.framework.clone(),
                    confidence: pattern.confidence,
                    evidence: Some(pattern.evidence.clone()),
                    in_test: pattern.in_test,
                    pattern_kind: "ERROR_HANDLE".to_string(),
                });

                // Batch inserts
                if pattern_batch.len() >= OBSERVABILITY_BATCH_SIZE {
                    total_patterns += pattern_batch.len();
                    self.insert_pattern_batch(&pattern_batch)?;
                    pattern_batch.clear();
                }
            }

            error_handling_patterns += eh_cap;

            // Extract telemetry patterns
            let tel_patterns =
                languages::extract_telemetry_patterns(&path, &content).unwrap_or_default();

            // Cap telemetry at remaining budget (up to 1000 total per file)
            let tel_cap = tel_patterns
                .len()
                .min(1000usize.saturating_sub(log_cap).saturating_sub(eh_cap));

            // Delete existing TRACE patterns for this file before inserting
            {
                let conn = self.storage.get_connection();
                conn.execute(
                    "DELETE FROM observability_patterns WHERE file_id = ?1 AND pattern_kind = 'TRACE'",
                    [file_id],
                )
                .into_diagnostic()?;
            }

            for pattern in tel_patterns.into_iter().take(tel_cap) {
                pattern_batch.push(ObservabilityRow {
                    file_id: *file_id,
                    line_start: pattern.line_start,
                    level: pattern.level.as_ref().map(|l| l.as_str().to_string()),
                    framework: pattern.framework.clone(),
                    confidence: pattern.confidence,
                    evidence: Some(pattern.evidence.clone()),
                    in_test: pattern.in_test,
                    pattern_kind: "TRACE".to_string(),
                });

                // Batch inserts
                if pattern_batch.len() >= OBSERVABILITY_BATCH_SIZE {
                    total_patterns += pattern_batch.len();
                    self.insert_pattern_batch(&pattern_batch)?;
                    pattern_batch.clear();
                }
            }

            telemetry_patterns += tel_cap;
            files_processed += 1;
        }

        // Flush remaining patterns
        if !pattern_batch.is_empty() {
            total_patterns += pattern_batch.len();
            self.insert_pattern_batch(&pattern_batch)?;
        }

        info!(
            "Observability extraction complete: {} patterns ({} error handling, {} telemetry) from {} files",
            total_patterns, error_handling_patterns, telemetry_patterns, files_processed
        );

        Ok(ObservabilityStats {
            total_patterns,
            error_handling_patterns,
            telemetry_patterns,
            files_processed,
        })
    }

    fn insert_pattern_batch(&self, patterns: &[ObservabilityRow]) -> Result<()> {
        let conn = self.storage.get_connection();
        let tx = conn.unchecked_transaction().into_diagnostic()?;
        let now = chrono::Utc::now().to_rfc3339();

        for pattern in patterns {
            tx.execute(
                "INSERT INTO observability_patterns \
                 (file_id, line_start, pattern_kind, level, framework, confidence, evidence, in_test, last_indexed_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                rusqlite::params![
                    pattern.file_id,
                    pattern.line_start,
                    pattern.pattern_kind,
                    pattern.level,
                    pattern.framework,
                    pattern.confidence,
                    pattern.evidence,
                    pattern.in_test as i32,
                    now,
                ],
            )
            .into_diagnostic()?;
        }

        tx.commit().into_diagnostic()?;
        Ok(())
    }
}

struct ObservabilityRow {
    file_id: i64,
    line_start: i32,
    level: Option<String>,
    framework: String,
    confidence: f64,
    evidence: Option<String>,
    in_test: bool,
    pattern_kind: String,
}

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
    fn test_log_level_as_str() {
        assert_eq!(LogLevel::Trace.as_str(), "trace");
        assert_eq!(LogLevel::Debug.as_str(), "debug");
        assert_eq!(LogLevel::Info.as_str(), "info");
        assert_eq!(LogLevel::Warn.as_str(), "warn");
        assert_eq!(LogLevel::Error.as_str(), "error");
    }

    #[test]
    fn test_observability_stats_serialization() {
        let stats = ObservabilityStats {
            total_patterns: 42,
            error_handling_patterns: 15,
            telemetry_patterns: 8,
            files_processed: 10,
        };
        let json = serde_json::to_string(&stats).unwrap();
        assert!(json.contains("total_patterns"));
        assert!(json.contains("error_handling_patterns"));
        assert!(json.contains("telemetry_patterns"));
        assert!(json.contains("files_processed"));
    }

    #[test]
    fn test_logging_pattern_fields() {
        let pattern = LoggingPattern {
            line_start: 10,
            level: Some(LogLevel::Info),
            framework: "tracing".to_string(),
            in_test: false,
            confidence: 1.0,
            evidence: "tracing::info!(\"message\")".to_string(),
        };
        assert_eq!(pattern.line_start, 10);
        assert_eq!(pattern.level.as_ref().unwrap().as_str(), "info");
        assert_eq!(pattern.framework, "tracing");
        assert!(!pattern.in_test);
    }

    #[test]
    fn test_clear_and_insert_observability_patterns() {
        let storage = in_memory_storage();
        let conn = storage.get_connection();

        // Insert a project_files row
        conn.execute(
            "INSERT INTO project_files (file_path, language, content_hash, file_size, last_indexed_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            ("src/lib.rs", "Rust", "hash_obs", 1024, "2026-05-01T00:00:00Z"),
        ).unwrap();
        let file_id = conn.last_insert_rowid();

        // Insert a pattern
        conn.execute(
            "INSERT INTO observability_patterns (file_id, line_start, pattern_kind, level, framework, confidence, evidence, in_test, last_indexed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            rusqlite::params![file_id, 42i64, "LOG", "info", "tracing", 1.0_f64, Some("tracing::info! call"), 0i64, "2026-05-01T00:00:00Z"],
        ).unwrap();

        // Verify pattern exists
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM observability_patterns", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 1);

        // Delete pattern for this file
        conn.execute(
            "DELETE FROM observability_patterns WHERE file_id = ?1 AND pattern_kind = 'LOG'",
            [file_id],
        )
        .unwrap();

        // Verify pattern was deleted
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM observability_patterns", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn test_error_handling_pattern_fields() {
        let pattern = ErrorHandlingPattern {
            line_start: 15,
            level: Some(LogLevel::Error),
            framework: "unwrap".to_string(),
            in_test: false,
            confidence: 1.0,
            evidence: "syntactic: unwrap call".to_string(),
        };
        assert_eq!(pattern.line_start, 15);
        assert_eq!(pattern.level.as_ref().unwrap().as_str(), "error");
        assert_eq!(pattern.framework, "unwrap");
        assert!(!pattern.in_test);
    }

    #[test]
    fn test_clear_and_insert_error_handle_patterns() {
        let storage = in_memory_storage();
        let conn = storage.get_connection();

        // Insert a project_files row
        conn.execute(
            "INSERT INTO project_files (file_path, language, content_hash, file_size, last_indexed_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            ("src/lib.rs", "Rust", "hash_eh", 1024, "2026-05-01T00:00:00Z"),
        ).unwrap();
        let file_id = conn.last_insert_rowid();

        // Insert an ERROR_HANDLE pattern
        conn.execute(
            "INSERT INTO observability_patterns (file_id, line_start, pattern_kind, level, framework, confidence, evidence, in_test, last_indexed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            rusqlite::params![file_id, 10i64, "ERROR_HANDLE", "error", "unwrap", 1.0_f64, Some("syntactic: unwrap call"), 0i64, "2026-05-01T00:00:00Z"],
        ).unwrap();

        // Verify pattern exists
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM observability_patterns", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 1);

        // Delete ERROR_HANDLE patterns for this file
        conn.execute(
            "DELETE FROM observability_patterns WHERE file_id = ?1 AND pattern_kind = 'ERROR_HANDLE'",
            [file_id],
        )
        .unwrap();

        // Verify pattern was deleted
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM observability_patterns", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 0);
    }
}
