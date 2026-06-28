use std::collections::HashMap;

use crate::index::entrypoint::{EntrypointKind, SymbolClassification};
use crate::index::symbols::Symbol;

/// Detect entry points in Rust source code.
pub fn detect_rust_entrypoints(content: &str, symbols: &[Symbol]) -> Vec<SymbolClassification> {
    let mut results = Vec::new();
    let mut has_entrypoint = false;
    let mut has_handler = false;

    // First pass: detect attributes and main function via tree-sitter
    let attr_map = parse_rust_attributes(content);

    for symbol in symbols {
        // Check for #[test] or #[tokio::test]
        if attr_map
            .get(&symbol.name)
            .is_some_and(|attrs| attrs.iter().any(|a| a == "test" || a == "tokio::test"))
        {
            results.push(SymbolClassification {
                symbol_name: symbol.name.clone(),
                kind: EntrypointKind::Test,
                confidence: 1.0,
                evidence: "Rust test function".to_string(),
            });
            continue;
        }

        // Check for extern "C"
        if symbol
            .metadata
            .get("abi")
            .is_some_and(|abi| abi.contains("\"C\""))
        {
            results.push(SymbolClassification {
                symbol_name: symbol.name.clone(),
                kind: EntrypointKind::Ffi,
                confidence: 1.0,
                evidence: "Extern C function".to_string(),
            });
            continue;
        }

        // Check for proc-macros
        if symbol.metadata.contains_key("macro") {
            results.push(SymbolClassification {
                symbol_name: symbol.name.clone(),
                kind: EntrypointKind::Macro,
                confidence: 1.0,
                evidence: "Rust procedural macro".to_string(),
            });
            continue;
        }

        // Check for FFI (extern "C")
        if let Some(abi) = symbol.metadata.get("abi")
            && (abi.contains("\"C\"") || abi.contains("\"system\""))
        {
            results.push(SymbolClassification {
                symbol_name: symbol.name.clone(),
                kind: EntrypointKind::Ffi,
                confidence: 1.0,
                evidence: format!("FFI export ({})", abi),
            });
            continue;
        }

        // Check for feature gates
        if let Some(cfg) = symbol.metadata.get("cfg") {
            results.push(SymbolClassification {
                symbol_name: symbol.name.clone(),
                kind: EntrypointKind::PublicApi,
                confidence: 0.8,
                evidence: format!("Feature gated: {}", cfg),
            });
            continue;
        }

        // Check for re-exports
        if symbol.metadata.contains_key("reexport") {
            results.push(SymbolClassification {
                symbol_name: symbol.name.clone(),
                kind: EntrypointKind::PublicApi,
                confidence: 1.0,
                evidence: "Public re-export".to_string(),
            });
            continue;
        }

        if symbol.kind != crate::index::symbols::SymbolKind::Function {
            continue;
        }

        // Check for #[tokio::main] or #[actix_web::main] — these are ENTRYPOINT
        if attr_map.get(&symbol.name).is_some_and(|attrs| {
            attrs
                .iter()
                .any(|a| a == "tokio::main" || a == "actix_web::main")
        }) {
            results.push(SymbolClassification {
                symbol_name: symbol.name.clone(),
                kind: EntrypointKind::Entrypoint,
                confidence: 1.0,
                evidence: "Async entry point attribute".to_string(),
            });
            has_entrypoint = true;
            continue;
        }

        // Check for HTTP handler attributes
        if attr_map
            .get(&symbol.name)
            .is_some_and(|attrs| attrs.iter().any(|a| is_rust_handler_attr(a)))
        {
            results.push(SymbolClassification {
                symbol_name: symbol.name.clone(),
                kind: EntrypointKind::Handler,
                confidence: 1.0,
                evidence: "HTTP handler attribute".to_string(),
            });
            has_handler = true;
            continue;
        }

        // Check for fn main()
        if symbol.name == "main" && !symbol.is_public {
            results.push(SymbolClassification {
                symbol_name: symbol.name.clone(),
                kind: EntrypointKind::Entrypoint,
                confidence: 1.0,
                evidence: "Rust main function".to_string(),
            });
            has_entrypoint = true;
            continue;
        }

        // Public function — will be classified later based on whether entrypoints exist
        if symbol.is_public {
            results.push(SymbolClassification {
                symbol_name: symbol.name.clone(),
                kind: EntrypointKind::PublicApi,
                confidence: 0.7,
                evidence: "Public function".to_string(),
            });
            continue;
        }

        // Internal function
        results.push(SymbolClassification {
            symbol_name: symbol.name.clone(),
            kind: EntrypointKind::Internal,
            confidence: 1.0,
            evidence: "Internal function".to_string(),
        });
    }

    // If no entrypoints or handlers found, all PublicApi classifications are correct.
    // If entrypoints exist, PublicApi should stay as-is (they are public API surface).
    // If no entrypoints and no handlers, all public fns should be PUBLIC_API with
    // updated evidence.
    if !has_entrypoint && !has_handler {
        for r in &mut results {
            if r.kind == EntrypointKind::PublicApi {
                r.evidence =
                    "no entry point found; all public symbols labeled as public API".to_string();
            }
        }
    }

    results
}

pub fn is_rust_handler_attr(attr: &str) -> bool {
    let handler_prefixes = [
        "actix_web::get",
        "actix_web::post",
        "actix_web::put",
        "actix_web::delete",
        "actix_web::patch",
        "axum::routing::get",
        "axum::routing::post",
        "axum::routing::put",
        "axum::routing::delete",
        "axum::routing::patch",
        "rocket::get",
        "rocket::post",
        "rocket::put",
        "rocket::delete",
        "rocket::patch",
    ];
    handler_prefixes.iter().any(|p| attr.starts_with(p))
}

/// Parse Rust source to extract a map of function name -> list of attribute paths.
pub fn parse_rust_attributes(content: &str) -> HashMap<String, Vec<String>> {
    use tree_sitter::{Parser, Query, QueryCursor, StreamingIterator};

    let mut parser = Parser::new();
    let language = tree_sitter_rust::LANGUAGE;
    if parser.set_language(&language.into()).is_err() {
        return HashMap::new();
    }

    let tree = match parser.parse(content, None) {
        Some(t) => t,
        None => return HashMap::new(),
    };

    // Query for function_item nodes and their preceding attributes
    let query_str = r#"
        (function_item
          name: (identifier) @func_name)
        (attribute_item) @attr
    "#;

    let query = match Query::new(&language.into(), query_str) {
        Ok(q) => q,
        Err(_) => return HashMap::new(),
    };

    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&query, tree.root_node(), content.as_bytes());

    let mut attr_map: HashMap<String, Vec<String>> = HashMap::new();

    // Walk the tree manually to find function_items with their attribute items
    fn walk_tree(
        node: tree_sitter::Node,
        content: &str,
        attr_map: &mut HashMap<String, Vec<String>>,
    ) {
        let mut pending_attrs: Vec<String> = Vec::new();

        for i in 0..node.child_count() {
            let child = node.child(i as u32).unwrap();
            match child.kind() {
                "attribute_item" => {
                    if let Ok(attr_text) = child.utf8_text(content.as_bytes()) {
                        // Extract attribute path from #[attr_path] or #[attr_path::nested]
                        let path = extract_rust_attr_path(attr_text);
                        pending_attrs.push(path);
                    }
                }
                "inner_attribute_item" => {
                    // Inner attributes (#![]) don't apply to functions, skip
                }
                "function_item" => {
                    // Get function name
                    for j in 0..child.child_count() {
                        let fc = child.child(j as u32).unwrap();
                        if fc.kind() == "identifier"
                            && let Ok(name) = fc.utf8_text(content.as_bytes())
                        {
                            attr_map.insert(name.to_string(), pending_attrs.clone());
                        }
                    }
                    pending_attrs.clear();
                    // Also recurse into function body for nested functions
                    walk_tree(child, content, attr_map);
                }
                _ => {
                    // Reset pending attrs for non-attribute, non-function siblings
                    // unless it's a block that might contain both
                    if child.kind() != "declaration_list"
                        && child.kind() != "block"
                        && child.kind() != "mod_item"
                    {
                        pending_attrs.clear();
                    }
                    walk_tree(child, content, attr_map);
                }
            }
        }
    }

    walk_tree(tree.root_node(), content, &mut attr_map);

    // Also consume the query cursor to avoid unused variable warning
    while matches.next().is_some() {}

    attr_map
}

pub fn extract_rust_attr_path(attr_text: &str) -> String {
    // #[attr_path(args)] or #[attr::path(args)]
    // Strip #[ and trailing ] and any parenthesized arguments
    let trimmed = attr_text
        .trim()
        .trim_start_matches("#[")
        .trim_end_matches(']');

    // Find the ( and take everything before it
    if let Some(paren_pos) = trimmed.find('(') {
        trimmed[..paren_pos].to_string()
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::index::entrypoint::EntrypointKind;
    use crate::index::symbols::{Symbol, SymbolKind};

    #[test]
    fn test_rust_entrypoint_main() {
        let content = r#"
fn main() {
    println!("hello");
}
"#;
        let symbols = vec![Symbol {
            name: "main".to_string(),
            kind: SymbolKind::Function,
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
        }];
        let results = detect_rust_entrypoints(content, &symbols);
        assert!(
            results
                .iter()
                .any(|r| r.kind == EntrypointKind::Entrypoint && r.symbol_name == "main")
        );
    }

    #[test]
    fn test_rust_entrypoint_test() {
        let content = r#"
#[test]
fn test_foo() {
    assert_eq!(1, 1);
}
"#;
        let symbols = vec![Symbol {
            name: "test_foo".to_string(),
            kind: SymbolKind::Function,
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
        }];
        let results = detect_rust_entrypoints(content, &symbols);
        assert!(
            results
                .iter()
                .any(|r| r.kind == EntrypointKind::Test && r.symbol_name == "test_foo")
        );
    }

    #[test]
    fn test_rust_entrypoint_public_api_library() {
        let content = r#"
pub fn my_lib_fn() -> i32 { 42 }
pub fn another_pub() -> bool { true }
"#;
        let symbols = vec![
            Symbol {
                name: "my_lib_fn".to_string(),
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
            },
            Symbol {
                name: "another_pub".to_string(),
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
            },
        ];
        let results = detect_rust_entrypoints(content, &symbols);
        let pub_apis: Vec<_> = results
            .iter()
            .filter(|r| r.kind == EntrypointKind::PublicApi)
            .collect();
        assert_eq!(pub_apis.len(), 2);
        assert!(
            pub_apis
                .iter()
                .any(|r| r.evidence.contains("no entry point found"))
        );
    }

    #[test]
    fn test_rust_entrypoint_handler() {
        let content = r#"
#[actix_web::get("/users")]
async fn get_users() -> HttpResponse {
    HttpResponse::Ok().finish()
}
"#;
        let symbols = vec![Symbol {
            name: "get_users".to_string(),
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
        }];
        let results = detect_rust_entrypoints(content, &symbols);
        assert!(
            results
                .iter()
                .any(|r| r.kind == EntrypointKind::Handler && r.symbol_name == "get_users")
        );
    }

    #[test]
    fn test_extract_rust_attr_path() {
        assert_eq!(extract_rust_attr_path("#[test]"), "test");
        assert_eq!(extract_rust_attr_path("#[tokio::test]"), "tokio::test");
        assert_eq!(
            extract_rust_attr_path("#[actix_web::get(\"/users\")]"),
            "actix_web::get"
        );
        assert_eq!(extract_rust_attr_path("#[derive(Debug)]"), "derive");
    }
}
