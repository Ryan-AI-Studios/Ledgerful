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

    // Derive signature_hash from the risk-bearing shape (one site, all languages).
    // SCIP path writes a different meaning and is intentionally left alone (0088 §4).
    let signature_hash = s
        .metadata
        .get(crate::index::signature::METADATA_SIGNATURE_SHAPE)
        .map(|shape| blake3::hash(shape.as_bytes()).to_hex().to_string());

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
        signature_hash,
        metadata,
        confidence: 1.0,
        evidence: None,
        last_indexed_at: now.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::signature::{
        SignatureParam, SymbolSignatureParts, build_symbol_signature, write_signature_metadata,
    };
    use crate::index::symbols::{Symbol, SymbolKind};
    use std::collections::BTreeMap;

    #[test]
    fn signature_hash_derived_from_shape_when_present() {
        let mut metadata = BTreeMap::new();
        let sig = build_symbol_signature(&SymbolSignatureParts {
            name: "foo".into(),
            modifiers: vec![],
            params: vec![SignatureParam {
                name: Some("a".into()),
                type_text: Some("u32".into()),
            }],
            return_type: Some("u64".into()),
        });
        write_signature_metadata(&mut metadata, &sig);

        let symbol = Symbol {
            name: "foo".into(),
            kind: SymbolKind::Function,
            is_public: true,
            cognitive_complexity: None,
            cyclomatic_complexity: None,
            line_start: Some(1),
            line_end: Some(1),
            qualified_name: None,
            byte_start: Some(0),
            byte_end: Some(10),
            entrypoint_kind: None,
            metadata,
        };

        let ps = symbol_to_project_symbol(&symbol, 1, "now");
        let hash = ps
            .signature_hash
            .as_ref()
            .expect("signature_hash must be Some when signatureShape is present");
        assert_eq!(hash.len(), 64, "blake3 hex digest is 64 chars");
        // Deterministic: same shape → same hash
        let ps2 = symbol_to_project_symbol(&symbol, 1, "now");
        assert_eq!(ps.signature_hash, ps2.signature_hash);
    }

    #[test]
    fn signature_hash_none_without_shape_metadata() {
        let symbol = Symbol {
            name: "foo".into(),
            kind: SymbolKind::Function,
            is_public: true,
            cognitive_complexity: None,
            cyclomatic_complexity: None,
            line_start: None,
            line_end: None,
            qualified_name: None,
            byte_start: None,
            byte_end: None,
            entrypoint_kind: None,
            metadata: BTreeMap::new(),
        };
        let ps = symbol_to_project_symbol(&symbol, 1, "now");
        assert!(ps.signature_hash.is_none());
    }
}
