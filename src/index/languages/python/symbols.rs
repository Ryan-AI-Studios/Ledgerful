use crate::index::symbols::{Symbol, SymbolKind};
use miette::{IntoDiagnostic, Result};
use tree_sitter::{Node, Parser, Query, QueryCursor, StreamingIterator};

pub fn extract_symbols(content: &str) -> Result<Option<Vec<Symbol>>> {
    let mut parser = Parser::new();
    let language = tree_sitter_python::LANGUAGE;
    parser.set_language(&language.into()).into_diagnostic()?;

    let tree = parser
        .parse(content, None)
        .ok_or_else(|| miette::miette!("Failed to parse Python content"))?;

    let query_str = r#"
        (function_definition name: (identifier) @name) @symbol
        (class_definition name: (identifier) @name) @symbol
    "#;

    let query = Query::new(&language.into(), query_str).into_diagnostic()?;
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&query, tree.root_node(), content.as_bytes());

    let mut symbols = Vec::new();

    while let Some(m) = matches.next() {
        let mut name = String::new();
        let mut kind = SymbolKind::Function;
        let mut symbol_node: Option<Node<'_>> = None;

        for capture in m.captures {
            let capture_name = query.capture_names()[capture.index as usize];
            if capture_name == "name" {
                name = capture
                    .node
                    .utf8_text(content.as_bytes())
                    .into_diagnostic()?
                    .to_string();
            } else if capture_name == "symbol" {
                symbol_node = Some(capture.node);
                match capture.node.kind() {
                    "function_definition" => kind = SymbolKind::Function,
                    "class_definition" => kind = SymbolKind::Class,
                    _ => {}
                }
            }
        }

        if name.is_empty() {
            continue;
        }

        // Methods: function_definition nested under class_definition without an
        // intervening function_definition (nested defs stay free Functions).
        let mut qualified_name = None;
        if kind == SymbolKind::Function
            && let Some(node) = symbol_node
            && let Some(class_name) = enclosing_class_name(node, content)
        {
            kind = SymbolKind::Method;
            qualified_name = Some(format!("{class_name}.{name}"));
        }

        let is_public = !name.starts_with('_');
        let (byte_start, byte_end, line_start, line_end) = if let Some(node) = symbol_node {
            (
                Some(node.start_byte() as i32),
                Some(node.end_byte() as i32),
                Some((node.start_position().row + 1) as i32),
                Some((node.end_position().row + 1) as i32),
            )
        } else {
            (None, None, None, None)
        };

        symbols.push(Symbol {
            name,
            kind,
            is_public,
            cognitive_complexity: None,
            cyclomatic_complexity: None,
            line_start,
            line_end,
            qualified_name,
            byte_start,
            byte_end,
            entrypoint_kind: None,
            metadata: std::collections::BTreeMap::new(),
        });
    }

    // Deterministic order
    symbols.sort_by(|a, b| {
        a.name
            .cmp(&b.name)
            .then(a.kind.as_str().cmp(b.kind.as_str()))
            .then(
                a.qualified_name
                    .as_deref()
                    .unwrap_or("")
                    .cmp(b.qualified_name.as_deref().unwrap_or("")),
            )
    });

    Ok(Some(symbols))
}

/// Walk parents of a `function_definition`. If a `class_definition` is found
/// before another `function_definition`, return the class name.
fn enclosing_class_name(function_node: Node<'_>, content: &str) -> Option<String> {
    let mut current = function_node.parent();
    while let Some(parent) = current {
        match parent.kind() {
            "function_definition" => return None,
            "class_definition" => {
                let mut cursor = parent.walk();
                for child in parent.children(&mut cursor) {
                    if child.kind() == "identifier" {
                        let n = child.utf8_text(content.as_bytes()).ok()?.to_string();
                        if !n.is_empty() {
                            return Some(n);
                        }
                    }
                }
                return None;
            }
            _ => {}
        }
        current = parent.parent();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::symbols::SymbolKind;

    #[test]
    fn test_extract_python_symbols() {
        let content = r#"
    def public_fn():
        pass

    def _private_fn():
        pass

    class PublicClass:
        pass

    class _PrivateClass:
        pass
    "#;

        let symbols = extract_symbols(content).unwrap().unwrap();

        assert!(
            symbols
                .iter()
                .any(|s| s.name == "public_fn" && s.kind == SymbolKind::Function && s.is_public)
        );
        assert!(
            symbols
                .iter()
                .any(|s| s.name == "_private_fn" && s.kind == SymbolKind::Function && !s.is_public)
        );
        assert!(
            symbols
                .iter()
                .any(|s| s.name == "PublicClass" && s.kind == SymbolKind::Class && s.is_public)
        );
        assert!(
            symbols
                .iter()
                .any(|s| s.name == "_PrivateClass" && s.kind == SymbolKind::Class && !s.is_public)
        );
    }

    #[test]
    fn test_python_method_qualified_name() {
        let content = r#"
class Service:
    def process(self):
        pass

    def _private(self):
        pass

def free_fn():
    pass

class Other:
    def process(self):
        def nested():
            pass
        pass
"#;

        let symbols = extract_symbols(content).unwrap().unwrap();

        let process = symbols
            .iter()
            .find(|s| s.qualified_name.as_deref() == Some("Service.process"))
            .expect("Service.process Method");
        assert_eq!(process.kind, SymbolKind::Method);
        assert_eq!(process.name, "process");
        assert!(process.is_public);

        let private = symbols
            .iter()
            .find(|s| s.qualified_name.as_deref() == Some("Service._private"))
            .expect("Service._private Method");
        assert_eq!(private.kind, SymbolKind::Method);
        assert!(!private.is_public);

        let free = symbols
            .iter()
            .find(|s| s.name == "free_fn")
            .expect("free_fn");
        assert_eq!(free.kind, SymbolKind::Function);
        assert_eq!(free.qualified_name, None);

        let other = symbols
            .iter()
            .find(|s| s.qualified_name.as_deref() == Some("Other.process"))
            .expect("Other.process");
        assert_eq!(other.kind, SymbolKind::Method);

        // Nested function inside method stays Function without QN.
        let nested = symbols.iter().find(|s| s.name == "nested").expect("nested");
        assert_eq!(nested.kind, SymbolKind::Function);
        assert_eq!(nested.qualified_name, None);

        // Two process methods are distinguished by QN (DoD-5 shape).
        let process_qns: Vec<_> = symbols
            .iter()
            .filter(|s| s.name == "process")
            .filter_map(|s| s.qualified_name.as_deref())
            .collect();
        assert!(process_qns.contains(&"Service.process"));
        assert!(process_qns.contains(&"Other.process"));
    }
}
