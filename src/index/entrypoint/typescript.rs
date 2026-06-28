use std::collections::HashMap;

use crate::index::entrypoint::{EntrypointKind, SymbolClassification};
use crate::index::symbols::Symbol;

/// Detect entry points in TypeScript/JavaScript source code.
pub fn detect_typescript_entrypoints(
    content: &str,
    symbols: &[Symbol],
    file_path: &str,
) -> Vec<SymbolClassification> {
    let mut results = Vec::new();
    let file_name = file_path
        .rsplit('/')
        .next()
        .unwrap_or(file_path)
        .to_string();

    let entry_file_names = [
        "main.ts",
        "main.tsx",
        "main.js",
        "main.jsx",
        "index.ts",
        "index.tsx",
        "index.js",
        "index.jsx",
        "server.ts",
        "server.tsx",
        "server.js",
        "server.jsx",
        "app.ts",
        "app.tsx",
        "app.js",
        "app.jsx",
    ];
    let is_entry_file = entry_file_names.contains(&file_name.as_str());

    // Parse route handlers and test blocks via tree-sitter
    let handler_routes = parse_typescript_handlers(content);
    let test_names = parse_typescript_tests(content);
    let has_default_export = content.contains("export default");

    for symbol in symbols {
        // Check if this is a test symbol
        if test_names.contains(&symbol.name) {
            results.push(SymbolClassification {
                symbol_name: symbol.name.clone(),
                kind: EntrypointKind::Test,
                confidence: 0.9,
                evidence: "TypeScript test function".to_string(),
            });
            continue;
        }

        // Check if this is a route handler
        if let Some(route) = handler_routes.get(&symbol.name) {
            results.push(SymbolClassification {
                symbol_name: symbol.name.clone(),
                kind: EntrypointKind::Handler,
                confidence: 0.9,
                evidence: format!("Route handler: {}", route),
            });
            continue;
        }

        // Check if this is an entry point (from entry file name or default export)
        if is_entry_file && symbol.is_public {
            results.push(SymbolClassification {
                symbol_name: symbol.name.clone(),
                kind: EntrypointKind::Entrypoint,
                confidence: 0.8,
                evidence: format!("Entry point file: {}", file_name),
            });
            continue;
        }

        if has_default_export && symbol.is_public {
            results.push(SymbolClassification {
                symbol_name: symbol.name.clone(),
                kind: EntrypointKind::Entrypoint,
                confidence: 0.7,
                evidence: "Default export".to_string(),
            });
            continue;
        }

        // Public function in non-entry file
        if symbol.is_public {
            results.push(SymbolClassification {
                symbol_name: symbol.name.clone(),
                kind: EntrypointKind::PublicApi,
                confidence: 0.7,
                evidence: "Public export".to_string(),
            });
            continue;
        }

        results.push(SymbolClassification {
            symbol_name: symbol.name.clone(),
            kind: EntrypointKind::Internal,
            confidence: 1.0,
            evidence: "Internal symbol".to_string(),
        });
    }

    results
}

pub fn parse_typescript_handlers(content: &str) -> HashMap<String, String> {
    use tree_sitter::{Parser, Query, QueryCursor, StreamingIterator};

    let mut handlers: HashMap<String, String> = HashMap::new();
    let mut parser = Parser::new();
    let language = tree_sitter_typescript::LANGUAGE_TYPESCRIPT;
    if parser.set_language(&language.into()).is_err() {
        return handlers;
    }

    let tree = match parser.parse(content, None) {
        Some(t) => t,
        None => return handlers,
    };

    // Look for call expressions like app.get("/path", handler) or router.post(...)
    let query_str = r#"
        (call_expression
          function: (member_expression
            object: (identifier) @obj
            property: (property_identifier) @method)
          arguments: (arguments . (string) @route)) @call
    "#;

    let query = match Query::new(&language.into(), query_str) {
        Ok(q) => q,
        Err(_) => return handlers,
    };

    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&query, tree.root_node(), content.as_bytes());

    let http_methods = ["get", "post", "put", "delete", "patch"];
    let handler_objects = ["app", "router"];

    while let Some(m) = matches.next() {
        let mut obj_name = String::new();
        let mut method_name = String::new();
        let mut route = String::new();

        for capture in m.captures {
            let name = query.capture_names()[capture.index as usize];
            match name {
                "obj" => {
                    obj_name = capture
                        .node
                        .utf8_text(content.as_bytes())
                        .unwrap_or("")
                        .to_string();
                }
                "method" => {
                    method_name = capture
                        .node
                        .utf8_text(content.as_bytes())
                        .unwrap_or("")
                        .to_string();
                }
                "route" => {
                    route = capture
                        .node
                        .utf8_text(content.as_bytes())
                        .unwrap_or("")
                        .to_string();
                }
                _ => {}
            }
        }

        if handler_objects.contains(&obj_name.as_str())
            && http_methods.contains(&method_name.as_str())
        {
            let handler_name = format!("{} {}", method_name.to_uppercase(), route);
            handlers.insert(format!("{}_{}", obj_name, method_name), handler_name);
        }
    }

    handlers
}

pub fn parse_typescript_tests(content: &str) -> Vec<String> {
    use tree_sitter::{Parser, Query, QueryCursor, StreamingIterator};

    let mut test_names = Vec::new();
    let mut parser = Parser::new();
    let language = tree_sitter_typescript::LANGUAGE_TYPESCRIPT;
    if parser.set_language(&language.into()).is_err() {
        return test_names;
    }

    let tree = match parser.parse(content, None) {
        Some(t) => t,
        None => return test_names,
    };

    // Find describe() and it() and test() calls
    let query_str = r#"
        (call_expression
          function: (identifier) @func_name
          arguments: (arguments (string) @test_desc)) @call
    "#;

    let query = match Query::new(&language.into(), query_str) {
        Ok(q) => q,
        Err(_) => return test_names,
    };

    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&query, tree.root_node(), content.as_bytes());

    let test_funcs = ["describe", "it", "test"];

    while let Some(m) = matches.next() {
        for capture in m.captures {
            let name = query.capture_names()[capture.index as usize];
            if name == "func_name" {
                let func_name = capture.node.utf8_text(content.as_bytes()).unwrap_or("");
                if test_funcs.contains(&func_name) {
                    test_names.push(func_name.to_string());
                }
            }
        }
    }

    test_names
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::index::entrypoint::EntrypointKind;
    use crate::index::symbols::{Symbol, SymbolKind};

    #[test]
    fn test_typescript_entry_file() {
        let content = r#"
export function handler() { }
"#;
        let symbols = vec![Symbol {
            name: "handler".to_string(),
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
        let results = detect_typescript_entrypoints(content, &symbols, "server.ts");
        assert!(results.iter().any(|r| r.kind == EntrypointKind::Entrypoint));
    }
}
