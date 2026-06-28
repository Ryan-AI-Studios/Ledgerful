use crate::index::symbols::{Symbol, SymbolKind};
use miette::{IntoDiagnostic, Result};
use tree_sitter::{Parser, Query, QueryCursor, StreamingIterator};

pub fn extract_symbols(content: &str) -> Result<Option<Vec<Symbol>>> {
    let mut parser = Parser::new();
    let language = tree_sitter_typescript::LANGUAGE_TYPESCRIPT;
    parser.set_language(&language.into()).into_diagnostic()?;

    let tree = parser
        .parse(content, None)
        .ok_or_else(|| miette::miette!("Failed to parse TypeScript content"))?;

    let query_str = r#"
        (function_declaration name: (identifier) @name) @symbol
        (class_declaration name: (type_identifier) @name) @symbol
        (interface_declaration name: (type_identifier) @name) @symbol
        (type_alias_declaration name: (type_identifier) @name) @symbol
        (enum_declaration name: (identifier) @name) @symbol
    "#;

    let query = Query::new(&language.into(), query_str).into_diagnostic()?;
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&query, tree.root_node(), content.as_bytes());

    let mut symbols = Vec::new();

    while let Some(m) = matches.next() {
        let mut name = String::new();
        let mut is_public = false;
        let mut kind = SymbolKind::Function;

        for capture in m.captures {
            let capture_name = query.capture_names()[capture.index as usize];
            if capture_name == "name" {
                name = capture
                    .node
                    .utf8_text(content.as_bytes())
                    .into_diagnostic()?
                    .to_string();
            } else if capture_name == "symbol" {
                let node = capture.node;
                match node.kind() {
                    "function_declaration" => kind = SymbolKind::Function,
                    "class_declaration" => kind = SymbolKind::Class,
                    "interface_declaration" => kind = SymbolKind::Interface,
                    "type_alias_declaration" => kind = SymbolKind::Type,
                    "enum_declaration" => kind = SymbolKind::Enum,
                    _ => {}
                }

                // Check if exported
                if let Some(parent) = node.parent()
                    && parent.kind() == "export_statement"
                {
                    is_public = true;
                }
            }
        }

        if !name.is_empty() {
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
    fn test_extract_typescript_symbols() {
        let content = r#"
            export function publicFn() {}
            function privateFn() {}
            export class PublicClass {}
            class PrivateClass {}
            export interface PublicInterface {}
            export type PublicType = string;
            export enum PublicEnum { A }
        "#;

        let symbols = extract_symbols(content).unwrap().unwrap();

        assert!(
            symbols
                .iter()
                .any(|s| s.name == "publicFn" && s.kind == SymbolKind::Function && s.is_public)
        );
        assert!(
            symbols
                .iter()
                .any(|s| s.name == "privateFn" && s.kind == SymbolKind::Function && !s.is_public)
        );
        assert!(
            symbols
                .iter()
                .any(|s| s.name == "PublicClass" && s.kind == SymbolKind::Class && s.is_public)
        );
        assert!(
            symbols
                .iter()
                .any(|s| s.name == "PrivateClass" && s.kind == SymbolKind::Class && !s.is_public)
        );
        assert!(symbols.iter().any(|s| s.name == "PublicInterface"
            && s.kind == SymbolKind::Interface
            && s.is_public));
        assert!(
            symbols
                .iter()
                .any(|s| s.name == "PublicType" && s.kind == SymbolKind::Type && s.is_public)
        );
        assert!(
            symbols
                .iter()
                .any(|s| s.name == "PublicEnum" && s.kind == SymbolKind::Enum && s.is_public)
        );
    }
}
