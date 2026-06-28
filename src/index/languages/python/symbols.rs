use crate::index::symbols::{Symbol, SymbolKind};
use miette::{IntoDiagnostic, Result};
use tree_sitter::{Parser, Query, QueryCursor, StreamingIterator};

pub fn extract_symbols(content: &str) -> Result<Option<Vec<Symbol>>> {
    let mut parser = Parser::new();
    let language = tree_sitter_python::LANGUAGE;
    parser.set_language(&language.into()).into_diagnostic()?;

    let tree = parser
        .parse(content, None)
        .ok_or_else(|| miette::miette!("Failed to parse Python content"))?;

    let query_str = r#"
        (function_definition name: (identifier) @name)
        (class_definition name: (identifier) @name)
    "#;

    let query = Query::new(&language.into(), query_str).into_diagnostic()?;
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&query, tree.root_node(), content.as_bytes());

    let mut symbols = Vec::new();

    while let Some(m) = matches.next() {
        let mut name = String::new();
        let mut kind = SymbolKind::Function;

        for capture in m.captures {
            let capture_name = query.capture_names()[capture.index as usize];
            if capture_name == "name" {
                name = capture
                    .node
                    .utf8_text(content.as_bytes())
                    .into_diagnostic()?
                    .to_string();

                if let Some(parent) = capture.node.parent() {
                    match parent.kind() {
                        "function_definition" => kind = SymbolKind::Function,
                        "class_definition" => kind = SymbolKind::Class,
                        _ => {}
                    }
                }
            }
        }

        if !name.is_empty() {
            // In Python, leading underscore usually means private
            let is_public = !name.starts_with('_');

            symbols.push(Symbol {
                name,
                kind,
                is_public,
                cognitive_complexity: None,
                cyclomatic_complexity: None,
                line_start: None,
                line_end: None,
                qualified_name: None,
                byte_start: None,
                byte_end: None,
                entrypoint_kind: None,
                metadata: std::collections::BTreeMap::new(),
            });
        }
    }

    Ok(Some(symbols))
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
}
