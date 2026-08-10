mod calls;
mod common;
mod models;
mod observability;
mod routes;
mod symbols;

pub use calls::extract_calls;
pub use common::{cpp_declarator_name, strip_include_delimiters};
pub use models::extract_data_models;
pub use observability::{
    extract_error_handling, extract_logging_patterns, extract_telemetry_patterns,
};
pub use routes::extract_routes;
pub use symbols::extract_symbols;

// Multi-file fixture coverage lives below.

#[cfg(test)]
mod fixture_tests {
    use super::*;
    use crate::index::call_graph::{CallKind, ResolutionStatus};
    use crate::index::languages::Language;
    use crate::index::metrics::{ComplexityScorer, NativeComplexityScorer};
    use crate::index::symbols::SymbolKind;
    use camino::Utf8Path;
    use std::path::{Path, PathBuf};

    fn cpp_sample_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/cpp_sample")
    }

    fn read_fixture(rel: &str) -> String {
        let path = cpp_sample_root().join(rel);
        std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read fixture {}: {e}", path.display()))
    }

    #[test]
    fn multi_file_cpp_fixture_covers_symbols_calls_models_complexity() {
        let widget_src = read_fixture("src/widget.cpp");
        let header_src = read_fixture("include/widget.hpp");

        let symbols = extract_symbols(&widget_src)
            .expect("parse widget.cpp")
            .expect("symbols present");

        // Free function, non-anonymous
        assert!(
            symbols
                .iter()
                .any(|s| s.name == "add" && s.kind == SymbolKind::Function),
            "expected free fn add; got {:?}",
            symbols
                .iter()
                .map(|s| (&s.name, s.kind.as_str()))
                .collect::<Vec<_>>()
        );

        // Class method
        assert!(
            symbols
                .iter()
                .any(|s| s.name == "value" && s.kind == SymbolKind::Method),
            "expected method value"
        );

        // ctor / dtor / operator — at least one named
        let has_special = symbols.iter().any(|s| {
            s.kind == SymbolKind::Method
                && (s.name == "Widget" || s.name.starts_with('~') || s.name.contains("operator"))
        });
        assert!(
            has_special,
            "expected ctor/dtor/operator method; methods={:?}",
            symbols
                .iter()
                .filter(|s| s.kind == SymbolKind::Method)
                .map(|s| &s.name)
                .collect::<Vec<_>>()
        );

        // Models from class/struct (class lives in the header for this fixture)
        let models =
            extract_data_models(&header_src, "include/widget.hpp", &symbols).expect("models");
        assert!(
            models.iter().any(|m| m.model_name == "Widget"),
            "expected Widget model; {models:?}"
        );

        // Calls: same-file resolved + member unwrap
        let edges =
            extract_calls(Path::new("src/widget.cpp"), &widget_src, &symbols).expect("calls");
        assert!(
            edges.iter().any(|e| {
                e.callee_name == "add"
                    && e.call_kind == CallKind::Direct
                    && e.resolution_status == ResolutionStatus::Resolved
            }),
            "expected Resolved same-file add; edges={edges:?}"
        );
        assert!(
            edges
                .iter()
                .any(|e| { e.callee_name == "value" && e.call_kind == CallKind::MethodCall }),
            "expected member call unwrap to value; edges={edges:?}"
        );

        // routes / obs empty
        assert!(extract_routes(&widget_src, &symbols).unwrap().is_empty());
        assert!(extract_logging_patterns(&widget_src).unwrap().is_empty());

        // Complexity: branched function > 1; lambda counted
        let scorer = NativeComplexityScorer::new();
        let scored = scorer
            .score_file(Utf8Path::new("src/widget.cpp"), &widget_src, Language::Cpp)
            .expect("score");
        let branched = scored
            .functions
            .iter()
            .find(|f| f.name == "branched_score")
            .expect("branched_score should be complexity-scored");
        assert!(
            branched.cyclomatic > 1,
            "branched_score cyclomatic must be > 1; got {branched:?}"
        );
        assert!(
            scored.functions.iter().any(|f| f.name == "lambda"),
            "lambda_expression should be scored; got {:?}",
            scored.functions
        );
        assert!(
            scored.functions.iter().any(|f| f.name == "add"),
            "add must not be anonymous; got {:?}",
            scored.functions
        );

        // Header also yields symbols (parse headers)
        let hdr_symbols = extract_symbols(&header_src)
            .expect("parse header")
            .expect("header symbols");
        assert!(
            hdr_symbols.iter().any(|s| s.name == "Widget"),
            "header Widget; {hdr_symbols:?}"
        );
    }
}
