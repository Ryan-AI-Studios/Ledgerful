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

/// Unknown `source_kind` from SQLite / CLI input. Fail closed (0216-B2): do
/// not skip the row and do not silently relabel as `Config`.
#[derive(Debug, thiserror::Error, miette::Diagnostic)]
#[error(
    "unknown env source_kind {found:?}; expected DOTENV_EXAMPLE, CONFIG, or DOCS (or camelCase dotenvExample/config/docs)"
)]
pub struct UnknownEnvSourceKind {
    pub found: String,
}

impl std::str::FromStr for EnvSourceKind {
    type Err = UnknownEnvSourceKind;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "DOTENV_EXAMPLE" | "dotenvExample" => Ok(EnvSourceKind::DotenvExample),
            "CONFIG" | "config" => Ok(EnvSourceKind::Config),
            "DOCS" | "docs" => Ok(EnvSourceKind::Docs),
            _ => Err(UnknownEnvSourceKind {
                found: s.to_string(),
            }),
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

/// Strip inline `#[cfg(test)] mod ident { ... }` bodies from Rust source
/// before env-ref regex (0216-D). Brace matching is lexical (strings,
/// comments, raw strings); a naive `{`/`}` counter is not used.
fn strip_inline_cfg_test_modules(content: &str) -> String {
    cfg_test_modules::strip(content)
}

mod cfg_test_modules {
    const ATTR: &str = "#[cfg(test)]";

    pub(super) fn strip(content: &str) -> String {
        let mut ranges = Vec::new();
        let mut i = 0;
        while i < content.len() {
            if content[i..].starts_with(ATTR)
                && let Some(end) = try_inline_cfg_test_module(content, i)
            {
                ranges.push((i, end));
                i = end;
                continue;
            }
            if let Some(end) = skip_non_code(content, i) {
                i = end;
                continue;
            }
            i = next_char_end(content, i);
        }
        if ranges.is_empty() {
            return content.to_string();
        }
        let mut out = String::with_capacity(content.len());
        let mut last = 0;
        for (start, end) in ranges {
            out.push_str(&content[last..start]);
            out.push(' ');
            last = end;
        }
        out.push_str(&content[last..]);
        out
    }

    fn try_inline_cfg_test_module(s: &str, i: usize) -> Option<usize> {
        if !s[i..].starts_with(ATTR) {
            return None;
        }
        let mut j = i + ATTR.len();
        j = skip_ws(s, j);
        if keyword_at(s, j, "pub") {
            j += 3;
            j = skip_ws(s, j);
        }
        if !keyword_at(s, j, "mod") {
            return None;
        }
        j += 3;
        j = skip_ws(s, j);
        if j >= s.len() || !is_ident_start_at(s, j) {
            return None;
        }
        j = skip_ident(s, j);
        j = skip_ws(s, j);
        if j >= s.len() || s.as_bytes()[j] != b'{' {
            return None;
        }
        let close = matching_brace_end(s, j)?;
        Some(close + 1)
    }

    fn matching_brace_end(s: &str, open: usize) -> Option<usize> {
        let mut i = open;
        let mut depth = 0;
        while i < s.len() {
            if let Some(end) = skip_non_code(s, i) {
                i = end;
                continue;
            }
            match s.as_bytes()[i] {
                b'{' => {
                    depth += 1;
                    i += 1;
                }
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(i);
                    }
                    i += 1;
                }
                _ => i = next_char_end(s, i),
            }
        }
        None
    }

    fn skip_non_code(s: &str, i: usize) -> Option<usize> {
        let bytes = s.as_bytes();
        if i + 1 < s.len() && bytes[i] == b'/' {
            if bytes[i + 1] == b'/' {
                return Some(skip_line_comment(s, i));
            }
            if bytes[i + 1] == b'*' {
                return Some(skip_block_comment(s, i));
            }
        }
        if let Some(end) = try_skip_raw_string(s, i) {
            return Some(end);
        }
        if let Some(q) = string_start_quote(s, i) {
            return Some(skip_quoted_string(s, q));
        }
        if let Some(q) = char_start_quote(s, i) {
            return Some(skip_char_or_lifetime(s, q));
        }
        None
    }

    fn skip_line_comment(s: &str, mut i: usize) -> usize {
        i += 2;
        let bytes = s.as_bytes();
        while i < s.len() && bytes[i] != b'\n' {
            i += 1;
        }
        i
    }

    fn skip_block_comment(s: &str, mut i: usize) -> usize {
        i += 2;
        let bytes = s.as_bytes();
        let mut depth = 1;
        while i + 1 < s.len() && depth > 0 {
            if bytes[i] == b'/' && bytes[i + 1] == b'*' {
                depth += 1;
                i += 2;
            } else if bytes[i] == b'*' && bytes[i + 1] == b'/' {
                depth -= 1;
                i += 2;
            } else {
                i = next_char_end(s, i);
            }
        }
        if depth > 0 { s.len() } else { i }
    }

    fn try_skip_raw_string(s: &str, i: usize) -> Option<usize> {
        let bytes = s.as_bytes();
        if i >= s.len() || !is_token_start(s, i) {
            return None;
        }
        let mut j = i;
        if bytes[j] == b'b' || bytes[j] == b'c' {
            j += 1;
            if j >= s.len() {
                return None;
            }
        }
        if bytes[j] != b'r' {
            return None;
        }
        j += 1;
        let hash_start = j;
        while j < s.len() && bytes[j] == b'#' {
            j += 1;
        }
        let n_hashes = j - hash_start;
        if j >= s.len() || bytes[j] != b'"' {
            return None;
        }
        j += 1;
        while j < s.len() {
            if bytes[j] == b'"' {
                let after = j + 1;
                if after + n_hashes <= s.len()
                    && bytes[after..after + n_hashes].iter().all(|&b| b == b'#')
                {
                    return Some(after + n_hashes);
                }
            }
            j += 1;
        }
        Some(s.len())
    }

    fn string_start_quote(s: &str, i: usize) -> Option<usize> {
        let bytes = s.as_bytes();
        if i >= s.len() {
            return None;
        }
        if bytes[i] == b'"' {
            return Some(i);
        }
        if is_token_start(s, i)
            && i + 1 < s.len()
            && (bytes[i] == b'b' || bytes[i] == b'c')
            && bytes[i + 1] == b'"'
        {
            return Some(i + 1);
        }
        None
    }

    fn skip_quoted_string(s: &str, mut i: usize) -> usize {
        i += 1;
        let bytes = s.as_bytes();
        while i < s.len() {
            match bytes[i] {
                b'\\' => {
                    i += 1;
                    if i < s.len() {
                        i = next_char_end(s, i);
                    }
                }
                b'"' => return i + 1,
                _ => i = next_char_end(s, i),
            }
        }
        s.len()
    }

    fn char_start_quote(s: &str, i: usize) -> Option<usize> {
        let bytes = s.as_bytes();
        if i >= s.len() {
            return None;
        }
        if bytes[i] == b'\'' {
            return Some(i);
        }
        if is_token_start(s, i) && i + 1 < s.len() && bytes[i] == b'b' && bytes[i + 1] == b'\'' {
            return Some(i + 1);
        }
        None
    }

    fn skip_char_or_lifetime(s: &str, mut i: usize) -> usize {
        i += 1;
        if i >= s.len() {
            return i;
        }
        let bytes = s.as_bytes();
        if bytes[i] == b'\\' {
            i += 1;
            if i < s.len() && bytes[i] == b'u' && i + 1 < s.len() && bytes[i + 1] == b'{' {
                i += 2;
                while i < s.len() && bytes[i] != b'}' {
                    i += 1;
                }
                if i < s.len() {
                    i += 1;
                }
            } else if i < s.len() {
                i = next_char_end(s, i);
            }
            if i < s.len() && bytes[i] == b'\'' {
                i += 1;
            }
            return i;
        }
        if is_ident_start_at(s, i) {
            let after_ident = skip_ident(s, i);
            if after_ident < s.len() && s.as_bytes()[after_ident] == b'\'' {
                return after_ident + 1;
            }
            return after_ident;
        }
        i = next_char_end(s, i);
        if i < s.len() && s.as_bytes()[i] == b'\'' {
            i += 1;
        }
        i
    }

    fn keyword_at(s: &str, i: usize, kw: &str) -> bool {
        if !s[i..].starts_with(kw) {
            return false;
        }
        let after = i + kw.len();
        after == s.len() || !is_ident_continue_at(s, after)
    }

    fn skip_ws(s: &str, mut i: usize) -> usize {
        let bytes = s.as_bytes();
        while i < s.len() && matches!(bytes[i], b' ' | b'\t' | b'\n' | b'\r') {
            i += 1;
        }
        i
    }

    fn skip_ident(s: &str, mut i: usize) -> usize {
        if i >= s.len() || !is_ident_start_at(s, i) {
            return i;
        }
        i = next_char_end(s, i);
        while i < s.len() && is_ident_continue_at(s, i) {
            i = next_char_end(s, i);
        }
        i
    }

    fn is_token_start(s: &str, i: usize) -> bool {
        match s[..i].chars().next_back() {
            None => true,
            Some(c) => !is_ident_continue(c),
        }
    }

    fn is_ident_start_at(s: &str, i: usize) -> bool {
        s[i..].chars().next().is_some_and(is_ident_start)
    }

    fn is_ident_continue_at(s: &str, i: usize) -> bool {
        s[i..].chars().next().is_some_and(is_ident_continue)
    }

    fn is_ident_start(c: char) -> bool {
        c.is_ascii_alphabetic() || c == '_'
    }

    fn is_ident_continue(c: char) -> bool {
        c.is_ascii_alphanumeric() || c == '_'
    }

    fn next_char_end(s: &str, i: usize) -> usize {
        match s[i..].chars().next() {
            Some(c) => i + c.len_utf8(),
            None => s.len(),
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
                    required: false,
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
                let content = strip_inline_cfg_test_modules(content);
                collect_env_captures(&RUST_ENV_VAR, &content, EnvReferenceKind::Read, &mut result);
                collect_env_captures(
                    &RUST_ENV_VAR_OS,
                    &content,
                    EnvReferenceKind::Read,
                    &mut result,
                );
                collect_env_captures(
                    &RUST_ENV_MACRO,
                    &content,
                    EnvReferenceKind::Read,
                    &mut result,
                );
                collect_env_captures(
                    &RUST_OPTION_ENV,
                    &content,
                    EnvReferenceKind::Read,
                    &mut result,
                );
                collect_env_captures(
                    &RUST_ENV_VAR_DEFAULT,
                    &content,
                    EnvReferenceKind::ReadWithDefault,
                    &mut result,
                );
                collect_env_captures(
                    &RUST_SET_ENV,
                    &content,
                    EnvReferenceKind::Write,
                    &mut result,
                );
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn committed_env_example() -> String {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".env.example");
        std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("committed .env.example must be readable at {path:?}: {e}"))
    }

    fn ref_names(refs: &[EnvReference]) -> Vec<&str> {
        refs.iter().map(|r| r.var_name.as_str()).collect()
    }

    #[test]
    fn committed_env_example_has_frozen_v1_keys() {
        let decls = EnvSchemaExtractor::extract_from_dotenv(&committed_env_example());
        let names: Vec<&str> = decls.iter().map(|d| d.var_name.as_str()).collect();
        // Spec §3.2 listed block is 21 KEY= lines (4+2+3+3+2+7). Planning
        // prose said "19" — that was a miscount of the same frozen set.
        assert_eq!(
            names,
            [
                "GEMINI_API_KEY",
                "LEDGERFUL_ASK_MODEL_1",
                "LEDGERFUL_ASK_PROVIDER_1",
                "LEDGERFUL_BRIDGE",
                "LEDGERFUL_CLOUD_POLICY",
                "LEDGERFUL_CONFIG_HOME",
                "LEDGERFUL_DEFAULT_CONFIG",
                "LEDGERFUL_NON_INTERACTIVE",
                "LEDGERFUL_NO_NETWORK",
                "LEDGERFUL_NO_TUI",
                "LEDGERFUL_PARENT_PID",
                "LEDGERFUL_QUIET",
                "LEDGERFUL_STATE_DIR",
                "LEDGERFUL_STRICT_OBSERVE_SIGNAL",
                "LEDGERFUL_SYNC_SECRET",
                "LEDGERFUL_TABLE_STYLE",
                "LEDGERFUL_WEB_PEER_ALLOWLIST",
                "LEDGERFUL_WEB_TOKEN",
                "OLLAMA_API_KEY",
                "OLLAMA_CLOUD_API_KEY",
                "OPENROUTER_API_KEY",
            ]
        );
    }

    #[test]
    fn committed_env_example_secrets_match_all_eight_patterns_and_stay_empty() {
        let content = committed_env_example();
        let decls = EnvSchemaExtractor::extract_from_dotenv(&content);
        assert_eq!(
            SECRET_PATTERNS,
            &[
                "SECRET",
                "KEY",
                "PASSWORD",
                "TOKEN",
                "API_KEY",
                "PRIVATE",
                "CREDENTIAL",
                "AUTH",
            ]
        );

        for decl in &decls {
            if is_secret_name(&decl.var_name) {
                assert!(
                    decl.is_secret,
                    "{} matches is_secret_name but is_secret=false",
                    decl.var_name
                );
                assert_eq!(
                    decl.default_value_redacted.as_deref(),
                    Some(EMPTY_DEFAULT),
                    "secret {} must stay empty (not a live token)",
                    decl.var_name
                );
                assert!(
                    !decl.required,
                    "empty secret {} is not required",
                    decl.var_name
                );
            }
        }

        // Every SECRET_PATTERNS entry is recognized by is_secret_name.
        for pat in SECRET_PATTERNS {
            let sample = format!("LEDGERFUL_{pat}_PROBE");
            assert!(
                is_secret_name(&sample),
                "is_secret_name must match SECRET_PATTERNS entry {pat} via {sample}"
            );
        }

        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            if let Some((_, value)) = trimmed.split_once('=') {
                let v = value.trim();
                assert!(
                    v.is_empty()
                        || PLACEHOLDER_VALUES
                            .iter()
                            .any(|p| v.to_lowercase().contains(p)),
                    "committed .env.example value must be empty or placeholder, got {trimmed}"
                );
                for needle in ["sk-", "AIza", "xox", "ghp_", "github_pat_"] {
                    assert!(
                        !v.contains(needle),
                        "committed .env.example must not contain live-looking token {needle}: {trimmed}"
                    );
                }
            }
        }
    }

    #[test]
    fn extract_from_dotenv_empty_value_is_not_required() {
        let decls = EnvSchemaExtractor::extract_from_dotenv("FOO=\nAPI_TOKEN=\n");
        let foo = decls
            .iter()
            .find(|d| d.var_name == "FOO")
            .expect("FOO declaration");
        assert!(!foo.required);
        assert_eq!(foo.default_value_redacted.as_deref(), Some(EMPTY_DEFAULT));
        assert!(!foo.is_secret);

        let token = decls
            .iter()
            .find(|d| d.var_name == "API_TOKEN")
            .expect("API_TOKEN declaration");
        assert!(token.is_secret);
        assert!(!token.required);
        assert_eq!(token.default_value_redacted.as_deref(), Some(EMPTY_DEFAULT));
    }

    #[test]
    fn env_source_kind_from_str_accepts_display_and_camel_case() {
        assert_eq!(
            "DOTENV_EXAMPLE".parse::<EnvSourceKind>().unwrap(),
            EnvSourceKind::DotenvExample
        );
        assert_eq!(
            "dotenvExample".parse::<EnvSourceKind>().unwrap(),
            EnvSourceKind::DotenvExample
        );
        assert_eq!(
            "CONFIG".parse::<EnvSourceKind>().unwrap(),
            EnvSourceKind::Config
        );
        assert_eq!(
            "config".parse::<EnvSourceKind>().unwrap(),
            EnvSourceKind::Config
        );
        assert_eq!(
            "DOCS".parse::<EnvSourceKind>().unwrap(),
            EnvSourceKind::Docs
        );
        assert_eq!(
            "docs".parse::<EnvSourceKind>().unwrap(),
            EnvSourceKind::Docs
        );
    }

    #[test]
    fn env_source_kind_from_str_unknown_is_err_not_config() {
        let err = "NOT_A_KIND"
            .parse::<EnvSourceKind>()
            .expect_err("unknown source_kind must fail closed");
        assert_eq!(err.found, "NOT_A_KIND");
        assert!(
            "Config".parse::<EnvSourceKind>().is_err(),
            "PascalCase Config must not silently map to Config"
        );
        assert!(
            "dotenv_example".parse::<EnvSourceKind>().is_err(),
            "snake_case must not map to DotenvExample"
        );
    }

    #[test]
    fn env_source_kind_serde_serializes_camel_case() {
        let json = serde_json::to_string(&EnvSourceKind::DotenvExample).unwrap();
        assert_eq!(json, "\"dotenvExample\"");
    }

    #[test]
    fn extract_references_from_source_skips_cfg_test_module_env_literals() {
        let content = r#"
use std::env;

pub fn load() {
    let _ = env::var("GEMINI_FAST_MODEL");
}

#[cfg(test)]
mod tests {
    let _ = std::env::var("DATABASE_URL");
    let _ = env!("API_TOKEN");
}
"#;
        let refs = EnvSchemaExtractor::extract_references_from_source(
            std::path::Path::new("src/index/runtime_usage.rs"),
            content,
        );
        let names = ref_names(&refs);
        assert!(
            names.contains(&"GEMINI_FAST_MODEL"),
            "production env::var must still extract, got {names:?}"
        );
        assert!(
            !names.contains(&"DATABASE_URL"),
            "cfg(test) module DATABASE_URL must not be a production ref, got {names:?}"
        );
        assert!(
            !names.contains(&"API_TOKEN"),
            "cfg(test) module API_TOKEN must not be a production ref, got {names:?}"
        );
    }

    #[test]
    fn extract_references_from_source_cfg_test_raw_string_brace_does_not_eat_production() {
        let content = r##"
use std::env;

#[cfg(test)]
mod tests {
    let _ = r#"{ not a rust brace }"#;
    let _ = r#"{"#;
    let _ = std::env::var("DATABASE_URL");
}

pub fn load() {
    let _ = env::var("GEMINI_FAST_MODEL");
}
"##;
        let refs = EnvSchemaExtractor::extract_references_from_source(
            std::path::Path::new("src/index/runtime_usage.rs"),
            content,
        );
        let names = ref_names(&refs);
        assert!(
            names.contains(&"GEMINI_FAST_MODEL"),
            "production env::var after cfg(test) module must still extract (naive brace count would eat it), got {names:?}"
        );
        assert!(
            !names.contains(&"DATABASE_URL"),
            "cfg(test) module DATABASE_URL must not appear, got {names:?}"
        );
    }
}
