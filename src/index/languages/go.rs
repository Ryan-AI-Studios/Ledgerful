mod calls;
mod common;
mod models;
mod observability;
mod routes;
mod symbols;
pub use calls::extract_calls;
pub use models::extract_data_models;
pub use observability::{
    extract_error_handling, extract_logging_patterns, extract_telemetry_patterns,
};
pub use routes::extract_routes;
pub use symbols::extract_symbols;

// Tests are co-located in their respective sub-modules.
// Multi-file fixture coverage lives below.

#[cfg(test)]
mod fixture_tests {
    use super::*;
    use crate::index::call_graph::{CallKind, ResolutionStatus};
    use crate::index::data_models::ModelKind;
    use crate::index::languages::Language;
    use crate::index::metrics::{ComplexityScorer, NativeComplexityScorer};
    use crate::index::symbols::SymbolKind;
    use camino::Utf8Path;
    use std::path::{Path, PathBuf};

    fn go_sample_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/go_sample")
    }

    fn read_fixture(rel: &str) -> String {
        let path = go_sample_root().join(rel);
        std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read fixture {}: {e}", path.display()))
    }

    #[test]
    fn multi_file_go_fixture_covers_symbols_calls_routes_models_obs() {
        let user_src = read_fixture("pkg/user.go");
        let handlers_src = read_fixture("pkg/handlers.go");

        // --- symbols: Method + qualified_name from receiver methods ---
        let user_symbols = extract_symbols(&user_src)
            .expect("parse user.go")
            .expect("symbols present");
        let display = user_symbols
            .iter()
            .find(|s| s.name == "DisplayName" && s.kind == SymbolKind::Method)
            .expect("DisplayName method");
        assert_eq!(display.qualified_name.as_deref(), Some("User.DisplayName"));
        assert!(display.is_public);

        let is_valid = user_symbols
            .iter()
            .find(|s| s.name == "IsValid" && s.kind == SymbolKind::Method)
            .expect("IsValid method");
        assert_eq!(is_valid.qualified_name.as_deref(), Some("User.IsValid"));

        // Top-level const/var only
        assert!(
            user_symbols
                .iter()
                .any(|s| s.name == "MaxUsers" && s.kind == SymbolKind::Constant)
        );
        assert!(
            user_symbols
                .iter()
                .any(|s| s.name == "DefaultUser" && s.kind == SymbolKind::Variable)
        );

        // --- models: struct with json tags ---
        let models = extract_data_models(&user_src, "pkg/user.go", &user_symbols).expect("models");
        let user_model = models
            .iter()
            .find(|m| m.model_name == "User")
            .expect("User model");
        assert_eq!(user_model.language, "go");
        assert_eq!(user_model.model_kind, ModelKind::Struct);
        assert!(user_model.evidence.contains("id"));
        assert!(user_model.evidence.contains("name"));

        // --- calls: Unresolved cross-package + Resolved local ---
        let handler_symbols = extract_symbols(&handlers_src)
            .expect("parse handlers.go")
            .expect("symbols present");
        let edges = extract_calls(
            Path::new("pkg/handlers.go"),
            &handlers_src,
            &handler_symbols,
        )
        .expect("calls");
        assert!(
            edges.iter().any(|e| {
                e.resolution_status == ResolutionStatus::Unresolved
                    && (e.callee_name == "fmt.Println"
                        || (e.callee_name == "Println" && e.evidence.contains("fmt")))
            }),
            "expected Unresolved fmt.Println; edges={edges:?}"
        );
        assert!(
            edges.iter().any(|e| {
                e.callee_name == "localHelper"
                    && e.call_kind == CallKind::Direct
                    && e.resolution_status == ResolutionStatus::Resolved
            }),
            "expected Resolved localHelper call"
        );

        // --- routes: net/http and/or gin ---
        let routes = extract_routes(&handlers_src, &handler_symbols).expect("routes");
        assert!(
            routes.iter().any(|r| r.framework == "nethttp"),
            "expected nethttp route; routes={routes:?}"
        );
        assert!(
            routes.iter().any(|r| r.framework == "gin"),
            "expected gin route; routes={routes:?}"
        );
        assert!(
            routes
                .iter()
                .any(|r| r.path_pattern == "/users/{id}" && r.method == "GET"),
            "expected GET /users/{{id}}"
        );

        // --- observability: slog + errors.Is + fmt.Errorf wrap ---
        let logging = extract_logging_patterns(&handlers_src).expect("logging");
        assert!(
            logging.iter().any(|p| p.framework == "slog"),
            "expected slog logging"
        );
        let errors = extract_error_handling(&handlers_src).expect("errors");
        assert!(
            errors
                .iter()
                .any(|p| p.framework == "errors" && p.evidence.contains("errors.Is"))
        );
        assert!(
            errors
                .iter()
                .any(|p| p.framework == "fmt" && p.evidence.contains("%w"))
        );

        // --- complexity on a fixture .go path ---
        let scorer = NativeComplexityScorer::new();
        let scored = scorer
            .score_file(Utf8Path::new("pkg/user.go"), &user_src, Language::Go)
            .expect("score user.go");
        assert!(
            scored
                .functions
                .iter()
                .any(|f| f.name == "DisplayName" && f.cyclomatic >= 1),
            "DisplayName should be complexity-scored; got {:?}",
            scored.functions
        );
    }
}
