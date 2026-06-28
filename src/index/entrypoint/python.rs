use std::collections::HashMap;

use crate::index::entrypoint::{EntrypointKind, SymbolClassification};
use crate::index::symbols::Symbol;

/// Detect entry points in Python source code.
pub fn detect_python_entrypoints(
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

    let has_main_block = content.contains("__name__") && content.contains("__main__");
    let has_fastapi = content.contains("FastAPI(") || content.contains("FastAPI (");
    let has_flask = content.contains("Flask(") || content.contains("Flask (");

    // Parse decorators via tree-sitter
    let decorator_map = parse_python_decorators(content);

    for symbol in symbols {
        // Check if function is a test
        if symbol.name.starts_with("test_") || symbol.name.ends_with("_test") {
            results.push(SymbolClassification {
                symbol_name: symbol.name.clone(),
                kind: EntrypointKind::Test,
                confidence: 0.9,
                evidence: "Python test function".to_string(),
            });
            continue;
        }

        // Check for handler decorators
        if let Some(decorators) = decorator_map.get(&symbol.name)
            && decorators.iter().any(|d| is_python_handler_decorator(d))
        {
            results.push(SymbolClassification {
                symbol_name: symbol.name.clone(),
                kind: EntrypointKind::Handler,
                confidence: 0.9,
                evidence: "HTTP handler decorator".to_string(),
            });
            continue;
        }

        // Check for entrypoint indicators
        if has_main_block && symbol.name == "main" {
            results.push(SymbolClassification {
                symbol_name: symbol.name.clone(),
                kind: EntrypointKind::Entrypoint,
                confidence: 0.9,
                evidence: "Python __main__ block with main()".to_string(),
            });
            continue;
        }

        // FastAPI/Flask app creation
        if (has_fastapi || has_flask) && symbol.is_public {
            results.push(SymbolClassification {
                symbol_name: symbol.name.clone(),
                kind: EntrypointKind::Entrypoint,
                confidence: 0.8,
                evidence: if has_fastapi {
                    "FastAPI application".to_string()
                } else {
                    "Flask application".to_string()
                },
            });
            continue;
        }

        // Public function in a file with __main__ block
        if symbol.is_public && has_main_block {
            results.push(SymbolClassification {
                symbol_name: symbol.name.clone(),
                kind: EntrypointKind::PublicApi,
                confidence: 0.7,
                evidence: "Public function in module with __main__".to_string(),
            });
            continue;
        }

        // Public function without entry point context
        if symbol.is_public {
            results.push(SymbolClassification {
                symbol_name: symbol.name.clone(),
                kind: EntrypointKind::PublicApi,
                confidence: 0.7,
                evidence: "Public function".to_string(),
            });
            continue;
        }

        results.push(SymbolClassification {
            symbol_name: symbol.name.clone(),
            kind: EntrypointKind::Internal,
            confidence: 1.0,
            evidence: "Internal function".to_string(),
        });
    }

    // If __main__ block exists but no main() function was found,
    // add a module-level entrypoint marker
    if has_main_block && !results.iter().any(|r| r.kind == EntrypointKind::Entrypoint) {
        results.push(SymbolClassification {
            symbol_name: format!("{}.__main__", file_name.trim_end_matches(".py")),
            kind: EntrypointKind::Entrypoint,
            confidence: 0.8,
            evidence: "Python __main__ block".to_string(),
        });
    }

    // If FastAPI/Flask app exists but no handlers found, mark public functions as public API
    if (has_fastapi || has_flask) && !results.iter().any(|r| r.kind == EntrypointKind::Handler) {
        for r in &mut results {
            if r.kind == EntrypointKind::PublicApi {
                r.evidence = if has_fastapi {
                    "FastAPI public endpoint".to_string()
                } else {
                    "Flask public endpoint".to_string()
                };
            }
        }
    }

    results
}

pub fn is_python_handler_decorator(decorator: &str) -> bool {
    let patterns = [
        "@app.get",
        "@app.post",
        "@app.put",
        "@app.delete",
        "@app.patch",
        "@app.route",
        "@router.get",
        "@router.post",
        "@router.put",
        "@router.delete",
        "@router.patch",
    ];
    patterns.iter().any(|p| decorator.starts_with(p))
}

pub fn parse_python_decorators(content: &str) -> HashMap<String, Vec<String>> {
    use tree_sitter::{Parser, Query, QueryCursor, StreamingIterator};

    let mut dec_map: HashMap<String, Vec<String>> = HashMap::new();
    let mut parser = Parser::new();
    let language = tree_sitter_python::LANGUAGE;
    if parser.set_language(&language.into()).is_err() {
        return dec_map;
    }

    let tree = match parser.parse(content, None) {
        Some(t) => t,
        None => return dec_map,
    };

    // Find decorated definitions
    let query_str = r#"
        (decorated_definition
          (decorator) @deco
          definition: (function_definition name: (identifier) @func_name)) @def
    "#;

    let query = match Query::new(&language.into(), query_str) {
        Ok(q) => q,
        Err(_) => return dec_map,
    };

    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&query, tree.root_node(), content.as_bytes());

    while let Some(m) = matches.next() {
        let mut func_name = String::new();
        let mut decorator = String::new();

        for capture in m.captures {
            let name = query.capture_names()[capture.index as usize];
            match name {
                "func_name" => {
                    func_name = capture
                        .node
                        .utf8_text(content.as_bytes())
                        .unwrap_or("")
                        .to_string();
                }
                "deco" => {
                    decorator = capture
                        .node
                        .utf8_text(content.as_bytes())
                        .unwrap_or("")
                        .to_string();
                }
                _ => {}
            }
        }

        if !func_name.is_empty() && !decorator.is_empty() {
            dec_map.entry(func_name).or_default().push(decorator);
        }
    }

    dec_map
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::index::entrypoint::EntrypointKind;
    use crate::index::symbols::{Symbol, SymbolKind};

    #[test]
    fn test_python_main_block() {
        let content = r#"
def run():
    pass

if __name__ == "__main__":
    run()
"#;
        let symbols = vec![Symbol {
            name: "run".to_string(),
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
        let results = detect_python_entrypoints(content, &symbols, "main.py");
        assert!(results.iter().any(|r| r.kind == EntrypointKind::Entrypoint));
    }

    #[test]
    fn test_python_test_function() {
        let symbols = vec![Symbol {
            name: "test_something".to_string(),
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
        let results = detect_python_entrypoints("", &symbols, "test_module.py");
        assert!(
            results
                .iter()
                .any(|r| r.kind == EntrypointKind::Test && r.symbol_name == "test_something")
        );
    }
}
