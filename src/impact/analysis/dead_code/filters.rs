use crate::index::symbols::Symbol;
use std::path::Path;

pub(super) fn is_entrypoint(symbol: &Symbol) -> bool {
    matches!(
        symbol.entrypoint_kind.as_deref(),
        Some("ENTRYPOINT")
            | Some("HANDLER")
            | Some("PUBLIC_API")
            | Some("TEST")
            | Some("FFI")
            | Some("MACRO")
    )
}

/// Name-based confidence penalty (-0.20) applied to symbols whose names match
/// common patterns for dynamically dispatched or serialized types.
///
/// Such types are typically invoked through trait objects, serde, or dependency
/// injection frameworks and therefore produce false positives when no static
/// call edges exist.
const NAME_PENALTY_SUFFIXES: &[&str] = &["Provider", "Result", "Chunk", "Record"];
const NAME_PENALTY: f64 = 0.20;

/// Returns the penalty to subtract from a symbol's confidence score based on its name.
pub(super) fn name_penalty(symbol_name: &str) -> f64 {
    for suffix in NAME_PENALTY_SUFFIXES {
        if symbol_name.ends_with(suffix) {
            return NAME_PENALTY;
        }
    }
    0.0
}

/// Bin scripts and crate `main` entrypoints that the extractor does not
/// classify as `ENTRYPOINT` (JS bin `main`, `src/main.rs`).
pub(super) fn is_bin_or_crate_main(symbol: &Symbol, file_path: &Path) -> bool {
    if symbol.name != "main" {
        return false;
    }
    let normalized = file_path.to_string_lossy().replace('\\', "/");
    let parts: Vec<&str> = normalized.split('/').filter(|p| !p.is_empty()).collect();
    if parts.len() > 1 && parts[..parts.len() - 1].contains(&"bin") {
        return true;
    }
    matches!(
        parts.last().copied(),
        Some("main.rs" | "main.js" | "main.ts" | "main.jsx" | "main.tsx")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::symbols::{Symbol, SymbolKind};
    use std::collections::BTreeMap;

    fn make_symbol(name: &str, kind: SymbolKind) -> Symbol {
        Symbol {
            name: name.to_string(),
            kind,
            is_public: false,
            cognitive_complexity: None,
            cyclomatic_complexity: None,
            line_start: None,
            line_end: None,
            qualified_name: None,
            byte_start: None,
            byte_end: None,
            entrypoint_kind: None,
            metadata: BTreeMap::new(),
        }
    }

    #[test]
    fn test_name_penalty_provider_suffix() {
        assert!((name_penalty("CIPredictorProvider") - NAME_PENALTY).abs() < f64::EPSILON);
        assert!((name_penalty("SomeProvider") - NAME_PENALTY).abs() < f64::EPSILON);
    }

    #[test]
    fn test_name_penalty_chunk_suffix() {
        assert!((name_penalty("RetrievedChunk") - NAME_PENALTY).abs() < f64::EPSILON);
    }

    #[test]
    fn test_name_penalty_record_suffix() {
        assert!((name_penalty("BridgeRecord") - NAME_PENALTY).abs() < f64::EPSILON);
    }

    #[test]
    fn test_name_penalty_result_suffix() {
        assert!((name_penalty("SearchResult") - NAME_PENALTY).abs() < f64::EPSILON);
    }

    #[test]
    fn test_name_penalty_no_match() {
        assert_eq!(name_penalty("execute_scan"), 0.0);
        assert_eq!(name_penalty("Config"), 0.0);
        assert_eq!(name_penalty("MyStruct"), 0.0);
    }

    #[test]
    fn is_bin_or_crate_main_slash_and_backslash() {
        let main_fn = make_symbol("main", SymbolKind::Function);
        assert!(is_bin_or_crate_main(
            &main_fn,
            Path::new("mcp-server/bin/x.js")
        ));
        assert!(is_bin_or_crate_main(
            &main_fn,
            Path::new("mcp-server\\bin\\x.js")
        ));
        assert!(is_bin_or_crate_main(&main_fn, Path::new("src/main.rs")));
        assert!(!is_bin_or_crate_main(&main_fn, Path::new("src/lib.rs")));
        let helper = make_symbol("helper", SymbolKind::Function);
        assert!(!is_bin_or_crate_main(
            &helper,
            Path::new("mcp-server/bin/x.js")
        ));
    }
}
