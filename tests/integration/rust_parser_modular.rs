use ledgerful::index::languages::rust::extract_calls;
use ledgerful::index::languages::rust::extract_routes;
use ledgerful::index::languages::rust::extract_symbols;
use ledgerful::index::symbols::SymbolKind;
use std::path::Path;

#[test]
fn test_rust_symbol_extraction() {
    let content = r#"
        pub fn foo() {}
        struct Bar {}
        enum Baz { A, B }
        trait Qux {}
        mod internal {}
        pub type MyType = i32;
    "#;
    let symbols = extract_symbols(content).unwrap().unwrap();
    assert_eq!(symbols.len(), 6);
    assert!(
        symbols
            .iter()
            .any(|s| s.name == "foo" && s.kind == SymbolKind::Function && s.is_public)
    );
    assert!(
        symbols
            .iter()
            .any(|s| s.name == "Bar" && s.kind == SymbolKind::Struct)
    );
    assert!(
        symbols
            .iter()
            .any(|s| s.name == "Baz" && s.kind == SymbolKind::Enum)
    );
    assert!(symbols.iter().any(|s| s.name == "MyType" && s.is_public));
}

#[test]
fn test_rust_route_extraction_actix() {
    let content = r#"
        use actix_web::get;
        #[get("/users")]
        pub async fn list_users() {}
    "#;
    let routes = extract_routes(content, &[]).unwrap();
    assert_eq!(routes.len(), 1);
    assert_eq!(routes[0].path_pattern, "/users");
    assert_eq!(routes[0].method, "GET");
    assert_eq!(routes[0].handler_name, "list_users");
    assert_eq!(routes[0].framework, "Actix");
}

#[test]
fn test_rust_route_extraction_axum() {
    let content = r#"
        let app = Router::new().route("/health", get(health_check));
    "#;
    let routes = extract_routes(content, &[]).unwrap();
    assert_eq!(routes.len(), 1);
    assert_eq!(routes[0].path_pattern, "/health");
    assert_eq!(routes[0].method, "GET");
    assert_eq!(routes[0].handler_name, "health_check");
    assert_eq!(routes[0].framework, "Axum");
}

#[test]
fn test_rust_route_extraction_axum_qualified() {
    let content = r#"
        let app = Router::new()
            .route("/hotspots/trend", get(api::hotspots_trend_handler));
    "#;
    let routes = extract_routes(content, &[]).unwrap();
    assert_eq!(routes.len(), 1);
    assert_eq!(routes[0].path_pattern, "/hotspots/trend");
    assert_eq!(routes[0].method, "GET");
    assert_eq!(routes[0].handler_name, "hotspots_trend_handler");
    assert_eq!(routes[0].framework, "Axum");
}

#[test]
fn test_rust_call_extraction() {
    let content = r#"
        fn main() {
            foo();
            let b = Bar::new();
            b.process();
        }
        fn foo() {}
        impl Bar { fn new() -> Self { Bar {} } fn process(&self) {} }
    "#;
    let path = Path::new("src/main.rs");
    let edges = extract_calls(path, content, &[]).unwrap();

    assert!(
        edges
            .iter()
            .any(|e| e.caller_name == "main" && e.callee_name == "foo")
    );
    assert!(
        edges
            .iter()
            .any(|e| e.caller_name == "main" && e.callee_name == "new")
    );
    assert!(
        edges
            .iter()
            .any(|e| e.caller_name == "main" && e.callee_name == "process")
    );
}
