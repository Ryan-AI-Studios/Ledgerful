use crate::index::symbols::Symbol;
use serde::{Deserialize, Serialize};

// --- Domain types mirroring project_files table ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectFile {
    pub id: Option<i64>,
    pub file_path: String,
    pub language: Option<String>,
    pub content_hash: Option<String>,
    pub git_blob_oid: Option<String>,
    pub file_size: Option<i64>,
    pub mtime_ns: Option<i64>,
    pub parser_version: String,
    pub parse_status: String,
    pub last_indexed_at: String,
}

// --- Domain types mirroring project_symbols table ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectSymbol {
    pub id: Option<i64>,
    pub file_id: i64,
    pub qualified_name: String,
    pub symbol_name: String,
    pub symbol_kind: String,
    pub visibility: Option<String>,
    pub entrypoint_kind: String,
    pub is_public: bool,
    pub cognitive_complexity: Option<i32>,
    pub cyclomatic_complexity: Option<i32>,
    pub line_start: Option<i32>,
    pub line_end: Option<i32>,
    pub byte_start: Option<i32>,
    pub byte_end: Option<i32>,
    pub signature_hash: Option<String>,
    pub metadata: Option<String>,
    pub confidence: f64,
    pub evidence: Option<String>,
    pub last_indexed_at: String,
}

pub fn symbol_to_project_symbol(s: &Symbol, file_id: i64, now: &str) -> ProjectSymbol {
    let qualified_name = s.qualified_name.clone().unwrap_or_else(|| s.name.clone());
    let visibility = if s.is_public {
        Some("public".to_string())
    } else {
        Some("private".to_string())
    };

    let metadata = if s.metadata.is_empty() {
        None
    } else {
        serde_json::to_string(&s.metadata).ok()
    };

    ProjectSymbol {
        id: None,
        file_id,
        qualified_name,
        symbol_name: s.name.clone(),
        symbol_kind: format!("{:?}", s.kind),
        visibility,
        entrypoint_kind: "INTERNAL".to_string(),
        is_public: s.is_public,
        cognitive_complexity: s.cognitive_complexity,
        cyclomatic_complexity: s.cyclomatic_complexity,
        line_start: s.line_start,
        line_end: s.line_end,
        byte_start: s.byte_start,
        byte_end: s.byte_end,
        signature_hash: None,
        metadata,
        confidence: 1.0,
        evidence: None,
        last_indexed_at: now.to_string(),
    }
}
