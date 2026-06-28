use crate::index::env_patterns::*;
use crate::state::storage::StorageManager;
use miette::{IntoDiagnostic, Result};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::path::Path;

// --- Redacted value markers ---
pub const HAS_DEFAULT: &str = "HAS_DEFAULT";
pub const EMPTY_DEFAULT: &str = "EMPTY_DEFAULT";
pub const PLACEHOLDER_DEFAULT: &str = "PLACEHOLDER_DEFAULT";
pub const POSSIBLE_SECRET_REDACTED: &str = "POSSIBLE_SECRET_REDACTED";

/// Secret name patterns that indicate sensitive values.
const SECRET_PATTERNS: &[&str] = &[
    "SECRET",
    "KEY",
    "PASSWORD",
    "TOKEN",
    "API_KEY",
    "PRIVATE",
    "CREDENTIAL",
    "AUTH",
];

/// Placeholder-like values that indicate a non-real default.
const PLACEHOLDER_VALUES: &[&str] = &[
    "your-",
    "xxx",
    "change_me",
    "changeme",
    "replace",
    "placeholder",
    "example",
    "todo",
    "fixme",
    "<",
    "{{",
    "{your",
    "insert",
    "fill",
    "put_your",
    "default",
];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "camelCase")]
pub enum EnvSourceKind {
    DotenvExample,
    Config,
    Docs,
}

impl std::fmt::Display for EnvSourceKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EnvSourceKind::DotenvExample => write!(f, "DOTENV_EXAMPLE"),
            EnvSourceKind::Config => write!(f, "CONFIG"),
            EnvSourceKind::Docs => write!(f, "DOCS"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "camelCase")]
pub enum EnvReferenceKind {
    Read,
    ReadWithDefault,
    Write,
}

impl std::fmt::Display for EnvReferenceKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EnvReferenceKind::Read => write!(f, "READ"),
            EnvReferenceKind::ReadWithDefault => write!(f, "READ_WITH_DEFAULT"),
            EnvReferenceKind::Write => write!(f, "WRITE"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct EnvDeclaration {
    pub var_name: String,
    pub source_kind: EnvSourceKind,
    pub required: bool,
    pub is_secret: bool,
    pub default_value_redacted: Option<String>,
    pub description: Option<String>,
    pub owner: Option<String>,
    pub environment: Option<String>,
    pub confidence: f64,
}

impl Eq for EnvDeclaration {}

impl PartialOrd for EnvDeclaration {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for EnvDeclaration {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.var_name
            .cmp(&other.var_name)
            .then_with(|| self.source_kind.cmp(&other.source_kind))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct EnvReference {
    pub var_name: String,
    pub reference_kind: EnvReferenceKind,
    pub confidence: f64,
}

impl Eq for EnvReference {}

impl PartialOrd for EnvReference {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for EnvReference {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.var_name
            .cmp(&other.var_name)
            .then_with(|| self.reference_kind.cmp(&other.reference_kind))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "camelCase")]
pub struct EnvVarDep {
    pub var_name: String,
    pub declared: bool,
    pub evidence: String,
}

/// Determines whether a value looks like a placeholder.
fn is_placeholder_value(value: &str) -> bool {
    let lower = value.to_lowercase();
    PLACEHOLDER_VALUES.iter().any(|pat| lower.contains(pat))
}

/// Determines whether a variable name suggests it holds a secret.
fn is_secret_name(name: &str) -> bool {
    let upper = name.to_uppercase();
    SECRET_PATTERNS.iter().any(|pat| upper.contains(pat))
}

/// Redacts a default value according to security rules.
fn redact_default(var_name: &str, value: &str) -> String {
    if value.is_empty() {
        return EMPTY_DEFAULT.to_string();
    }
    if is_secret_name(var_name) {
        return POSSIBLE_SECRET_REDACTED.to_string();
    }
    if is_placeholder_value(value) {
        return PLACEHOLDER_DEFAULT.to_string();
    }
    HAS_DEFAULT.to_string()
}

fn collect_env_captures(
    regex: &Regex,
    content: &str,
    kind: EnvReferenceKind,
    out: &mut Vec<EnvReference>,
) {
    for capture in regex.captures_iter(content) {
        if let Some(m) = capture.get(1) {
            out.push(EnvReference {
                var_name: m.as_str().to_string(),
                reference_kind: kind.clone(),
                confidence: 1.0,
            });
        }
    }
}

pub struct EnvSchemaExtractor;

impl EnvSchemaExtractor {
    pub fn extract_from_dotenv(content: &str) -> Vec<EnvDeclaration> {
        let mut declarations = Vec::new();
        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            if let Some(eq_pos) = trimmed.find('=') {
                let key = trimmed[..eq_pos]
                    .trim()
                    .strip_prefix("export ")
                    .unwrap_or(trimmed[..eq_pos].trim())
                    .trim()
                    .to_string();
                let value = trimmed[eq_pos + 1..]
                    .trim()
                    .trim_matches('"')
                    .trim_matches('\'')
                    .to_string();
                if key.is_empty() {
                    continue;
                }
                let default_value_redacted = if value.is_empty() {
                    Some(EMPTY_DEFAULT.to_string())
                } else {
                    Some(redact_default(&key, &value))
                };
                declarations.push(EnvDeclaration {
                    var_name: key.clone(),
                    source_kind: EnvSourceKind::DotenvExample,
                    required: value.is_empty(),
                    is_secret: is_secret_name(&key),
                    default_value_redacted,
                    description: None,
                    owner: None,
                    environment: None,
                    confidence: 1.0,
                });
            }
        }
        declarations.sort_unstable();
        declarations.dedup();
        declarations
    }

    pub fn extract_from_toml(content: &str) -> Vec<EnvDeclaration> {
        let mut declarations = Vec::new();
        let mut in_env_section = false;
        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with('[') {
                let section = trimmed.trim_start_matches('[').trim_end_matches(']');
                in_env_section = section == "env" || section.starts_with("env.");
                continue;
            }
            if in_env_section && let Some(eq_pos) = trimmed.find('=') {
                let key = trimmed[..eq_pos].trim().to_string();
                let value = trimmed[eq_pos + 1..]
                    .trim()
                    .trim_matches('"')
                    .trim_matches('\'')
                    .to_string();
                declarations.push(EnvDeclaration {
                    var_name: key.clone(),
                    source_kind: EnvSourceKind::Config,
                    required: false,
                    is_secret: is_secret_name(&key),
                    default_value_redacted: Some(redact_default(&key, &value)),
                    description: None,
                    owner: None,
                    environment: None,
                    confidence: 0.85,
                });
            }
        }
        declarations
    }

    pub fn extract_from_json(content: &str) -> Vec<EnvDeclaration> {
        let mut declarations = Vec::new();
        if let Ok(val) = serde_json::from_str::<serde_json::Value>(content)
            && let Some(obj) = val.as_object()
        {
            for (key, _val) in obj {
                if key
                    .chars()
                    .all(|c| c.is_ascii_uppercase() || c == '_' || c.is_ascii_digit())
                {
                    declarations.push(EnvDeclaration {
                        var_name: key.clone(),
                        source_kind: EnvSourceKind::Config,
                        required: false,
                        is_secret: is_secret_name(key),
                        default_value_redacted: Some(HAS_DEFAULT.to_string()),
                        description: None,
                        owner: None,
                        environment: None,
                        confidence: 0.7,
                    });
                }
            }
        }
        declarations
    }

    pub fn extract_references_from_source(path: &Path, content: &str) -> Vec<EnvReference> {
        let mut result = Vec::new();
        let extension = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or_default();
        match extension {
            "rs" => {
                collect_env_captures(&RUST_ENV_VAR, content, EnvReferenceKind::Read, &mut result);
                collect_env_captures(
                    &RUST_ENV_VAR_OS,
                    content,
                    EnvReferenceKind::Read,
                    &mut result,
                );
                collect_env_captures(
                    &RUST_ENV_MACRO,
                    content,
                    EnvReferenceKind::Read,
                    &mut result,
                );
                collect_env_captures(
                    &RUST_OPTION_ENV,
                    content,
                    EnvReferenceKind::Read,
                    &mut result,
                );
                collect_env_captures(
                    &RUST_ENV_VAR_DEFAULT,
                    content,
                    EnvReferenceKind::ReadWithDefault,
                    &mut result,
                );
                collect_env_captures(&RUST_SET_ENV, content, EnvReferenceKind::Write, &mut result);
            }
            "ts" | "js" | "tsx" | "jsx" => {
                collect_env_captures(&TS_ENV_DOT, content, EnvReferenceKind::Read, &mut result);
                collect_env_captures(
                    &TS_ENV_INDEXED,
                    content,
                    EnvReferenceKind::Read,
                    &mut result,
                );
                collect_env_captures(
                    &TS_IMPORT_META_ENV,
                    content,
                    EnvReferenceKind::Read,
                    &mut result,
                );
                collect_env_captures(
                    &TS_ENV_DESTRUCTURING,
                    content,
                    EnvReferenceKind::Read,
                    &mut result,
                );
                collect_env_captures(
                    &TS_ENV_DEFAULT,
                    content,
                    EnvReferenceKind::ReadWithDefault,
                    &mut result,
                );
                collect_env_captures(&TS_SET_ENV, content, EnvReferenceKind::Write, &mut result);
            }
            "py" => {
                collect_env_captures(&PY_ENV_GET, content, EnvReferenceKind::Read, &mut result);
                collect_env_captures(
                    &PY_ENVIRON_GET,
                    content,
                    EnvReferenceKind::Read,
                    &mut result,
                );
                collect_env_captures(
                    &PY_ENV_INDEXED,
                    content,
                    EnvReferenceKind::Read,
                    &mut result,
                );
                collect_env_captures(
                    &PY_ENV_GET_DEFAULT,
                    content,
                    EnvReferenceKind::ReadWithDefault,
                    &mut result,
                );
            }
            _ => {}
        }
        result
    }

    pub fn find_undeclared(
        references: &[EnvReference],
        declarations: &[EnvDeclaration],
    ) -> Vec<EnvVarDep> {
        let declared: std::collections::HashSet<_> =
            declarations.iter().map(|d| &d.var_name).collect();
        references
            .iter()
            .filter(|r| !declared.contains(&r.var_name))
            .map(|r| EnvVarDep {
                var_name: r.var_name.clone(),
                declared: false,
                evidence: format!("Referenced as {:?}", r.reference_kind),
            })
            .collect()
    }
}

pub struct EnvSchemaIndexer<'a> {
    storage: &'a StorageManager,
    repo_path: std::path::PathBuf,
}

struct EnvDeclarationRow {
    source_file_id: i64,
    var_name: String,
    source_kind: String,
    required: bool,
    is_secret: bool,
    default_value_redacted: Option<String>,
    description: Option<String>,
    owner: Option<String>,
    environment: Option<String>,
    confidence: f64,
}

#[allow(dead_code)]
struct EnvReferenceRow {
    file_id: i64,
    symbol_id: Option<i64>,
    var_name: String,
    reference_kind: String,
    confidence: f64,
    line_start: Option<i64>,
}

impl<'a> EnvSchemaIndexer<'a> {
    pub fn new(storage: &'a StorageManager, repo_path: std::path::PathBuf) -> Self {
        Self { storage, repo_path }
    }

    pub fn extract(&self) -> Result<EnvSchemaStats> {
        let now = chrono::Utc::now().to_rfc3339();
        let conn = self.storage.get_connection();

        // Use a transaction for atomic replacement (Phase 3)
        let tx = conn
            .unchecked_transaction()
            .map_err(|e| miette::miette!("Failed to start transaction: {}", e))?;

        // 1. Resolve .env.example file ID if it exists
        let example_file_id: Option<i64> = tx
            .query_row(
                "SELECT id FROM project_files WHERE file_path = '.env.example' OR file_path = '.env.dist'",
                [],
                |row| row.get(0),
            )
            .ok();

        // 2. Extract declarations from .env.example
        let mut decls = Vec::new();
        let mut dotenv_count = 0;
        let example_path = self.repo_path.join(".env.example");
        if example_path.exists() {
            let content =
                crate::util::fs::read_to_string_with_encoding(&example_path).into_diagnostic()?;
            let file_decls = EnvSchemaExtractor::extract_from_dotenv(&content);
            dotenv_count = file_decls.len();

            let file_id = if let Some(id) = example_file_id {
                id
            } else {
                // Ensure .env.example is in project_files to satisfy FK constraints
                tx.execute(
                    "INSERT OR IGNORE INTO project_files (file_path, language, last_indexed_at) \
                     VALUES (?1, ?2, ?3)",
                    rusqlite::params![".env.example", "Dotenv", now],
                )
                .into_diagnostic()?;
                tx.query_row(
                    "SELECT id FROM project_files WHERE file_path = '.env.example'",
                    [],
                    |row| row.get(0),
                )
                .into_diagnostic()?
            };

            // Clear existing declarations for this file ID to ensure idempotency
            tx.execute(
                "DELETE FROM env_declarations WHERE source_file_id = ?",
                [file_id],
            )
            .into_diagnostic()?;

            let rows: Vec<EnvDeclarationRow> = file_decls
                .into_iter()
                .map(|d| EnvDeclarationRow {
                    source_file_id: file_id,
                    var_name: d.var_name,
                    source_kind: d.source_kind.to_string(),
                    required: d.required,
                    is_secret: d.is_secret,
                    default_value_redacted: d.default_value_redacted,
                    description: d.description,
                    owner: d.owner,
                    environment: d.environment,
                    confidence: d.confidence,
                })
                .collect();
            self.insert_declaration_batch(&tx, &rows, &now)?;
            decls.extend(rows);
        }

        // 3. Extract references from all source files
        let files: Vec<(i64, String)> = {
            let mut file_stmt = tx
                .prepare("SELECT id, file_path FROM project_files WHERE parse_status != 'DELETED'")
                .into_diagnostic()?;

            file_stmt
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
                .into_diagnostic()?
                .collect::<Result<Vec<_>, _>>()
                .into_diagnostic()?
        };

        let mut all_refs = Vec::new();
        let mut files_processed = 0;

        for (file_id, file_path) in files {
            let full_path = self.repo_path.join(&file_path);
            if let Ok(content) = crate::util::fs::read_to_string_with_encoding(&full_path) {
                let refs = EnvSchemaExtractor::extract_references_from_source(
                    std::path::Path::new(&file_path),
                    &content,
                );

                // Always clear existing references for this file to avoid duplicates
                tx.execute("DELETE FROM env_references WHERE file_id = ?", [file_id])
                    .into_diagnostic()?;

                if !refs.is_empty() {
                    let ref_rows: Vec<EnvReferenceRow> = refs
                        .into_iter()
                        .map(|r| EnvReferenceRow {
                            file_id,
                            symbol_id: None,
                            var_name: r.var_name,
                            reference_kind: r.reference_kind.to_string(),
                            confidence: r.confidence,
                            line_start: None,
                        })
                        .collect();
                    self.insert_reference_batch(&tx, &ref_rows, &now)?;
                    all_refs.extend(ref_rows);
                }
                files_processed += 1;
            }
        }

        // 4. Prune references for files that are no longer tracked
        tx.execute(
            "DELETE FROM env_references WHERE file_id NOT IN (SELECT id FROM project_files WHERE parse_status != 'DELETED')",
            [],
        ).into_diagnostic()?;

        tx.commit().into_diagnostic()?;

        let stats = EnvSchemaStats {
            total_declarations: decls.len(),
            total_references: all_refs.len(),
            dotenv_declarations: dotenv_count,
            config_declarations: decls.len() - dotenv_count,
            files_processed,
        };

        Ok(stats)
    }

    fn insert_declaration_batch(
        &self,
        tx: &rusqlite::Transaction,
        rows: &[EnvDeclarationRow],
        now: &str,
    ) -> Result<()> {
        for row in rows {
            tx.execute(
                "INSERT OR IGNORE INTO env_declarations (var_name, source_file_id, source_kind, required, is_secret, default_value_redacted, description, owner, environment, confidence, last_indexed_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                rusqlite::params![row.var_name, row.source_file_id, row.source_kind, row.required as i32, row.is_secret as i32, row.default_value_redacted, row.description, row.owner, row.environment, row.confidence, now],
            ).into_diagnostic()?;
        }
        Ok(())
    }

    fn insert_reference_batch(
        &self,
        tx: &rusqlite::Transaction,
        rows: &[EnvReferenceRow],
        now: &str,
    ) -> Result<()> {
        for row in rows {
            tx.execute(
                "INSERT OR IGNORE INTO env_references (file_id, symbol_id, var_name, reference_kind, confidence, line_start, last_indexed_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                rusqlite::params![row.file_id, row.symbol_id, row.var_name, row.reference_kind, row.confidence, row.line_start, now],
            ).into_diagnostic()?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct EnvSchemaStats {
    pub total_declarations: usize,
    pub total_references: usize,
    pub dotenv_declarations: usize,
    pub config_declarations: usize,
    pub files_processed: usize,
}
