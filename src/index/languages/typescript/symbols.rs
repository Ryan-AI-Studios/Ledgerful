use crate::index::symbols::{Symbol, SymbolKind};
use miette::{IntoDiagnostic, Result};
use tree_sitter::{Node, Parser, Query, QueryCursor, StreamingIterator};

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
        (method_definition name: (property_identifier) @name) @symbol
    "#;

    let query = Query::new(&language.into(), query_str).into_diagnostic()?;
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&query, tree.root_node(), content.as_bytes());

    let mut symbols = Vec::new();

    while let Some(m) = matches.next() {
        let mut name = String::new();
        let mut is_public = false;
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
                let node = capture.node;
                symbol_node = Some(node);
                match node.kind() {
                    "function_declaration" => kind = SymbolKind::Function,
                    "class_declaration" => kind = SymbolKind::Class,
                    "interface_declaration" => kind = SymbolKind::Interface,
                    "type_alias_declaration" => kind = SymbolKind::Type,
                    "enum_declaration" => kind = SymbolKind::Enum,
                    "method_definition" => kind = SymbolKind::Method,
                    _ => {}
                }

                // Check if exported (functions/classes/etc.)
                if let Some(parent) = node.parent()
                    && parent.kind() == "export_statement"
                {
                    is_public = true;
                }
            }
        }

        if name.is_empty() {
            continue;
        }

        // Class methods: qualify as Class.method. Non-class method_definition
        // (object-literal methods, etc.) still emit as Method with QN None so
        // still index as Method so the symbol graph is complete (R1-06).
        let mut qualified_name = None;
        if kind == SymbolKind::Method {
            let Some(node) = symbol_node else {
                continue;
            };
            if let Some(class_name) = enclosing_class_name(node, content) {
                qualified_name = Some(format!("{class_name}.{name}"));
                // Method visibility: exported class ⇒ treat methods as public.
                if class_is_exported(node) {
                    is_public = true;
                }
            }
            // else: no enclosing class — keep Method, qualified_name: None
        }

        // Free functions keep qualified_name: None (match Rust free functions).
        if kind == SymbolKind::Function {
            qualified_name = None;
        }

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

fn enclosing_class_name(method_node: Node<'_>, content: &str) -> Option<String> {
    let mut current = method_node.parent();
    while let Some(parent) = current {
        if parent.kind() == "class_declaration" {
            // Prefer type_identifier name field.
            if let Some(name_node) = parent.child_by_field_name("name") {
                let n = name_node.utf8_text(content.as_bytes()).ok()?.to_string();
                if !n.is_empty() {
                    return Some(n);
                }
            }
            let mut cursor = parent.walk();
            for child in parent.children(&mut cursor) {
                if child.kind() == "type_identifier" || child.kind() == "identifier" {
                    let n = child.utf8_text(content.as_bytes()).ok()?.to_string();
                    if !n.is_empty() {
                        return Some(n);
                    }
                }
            }
            return None;
        }
        current = parent.parent();
    }
    None
}

fn class_is_exported(method_node: Node<'_>) -> bool {
    let mut current = method_node.parent();
    while let Some(parent) = current {
        if parent.kind() == "class_declaration" {
            if let Some(gp) = parent.parent() {
                return gp.kind() == "export_statement";
            }
            return false;
        }
        current = parent.parent();
    }
    false
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

    #[test]
    fn test_typescript_method_qualified_name() {
        let content = r#"
            export class Service {
                process(): void {}
                helper(): number { return 1; }
            }
            class Other {
                process(): void {}
            }
            function freeFn(): void {}
        "#;

        let symbols = extract_symbols(content).unwrap().unwrap();

        let process = symbols
            .iter()
            .find(|s| s.qualified_name.as_deref() == Some("Service.process"))
            .expect("Service.process");
        assert_eq!(process.kind, SymbolKind::Method);
        assert_eq!(process.name, "process");
        assert!(process.is_public, "method on exported class is public");

        let other = symbols
            .iter()
            .find(|s| s.qualified_name.as_deref() == Some("Other.process"))
            .expect("Other.process");
        assert_eq!(other.kind, SymbolKind::Method);

        let free = symbols.iter().find(|s| s.name == "freeFn").expect("freeFn");
        assert_eq!(free.kind, SymbolKind::Function);
        assert_eq!(free.qualified_name, None);

        let helper = symbols
            .iter()
            .find(|s| s.qualified_name.as_deref() == Some("Service.helper"))
            .expect("Service.helper");
        assert_eq!(helper.kind, SymbolKind::Method);
    }

    #[test]
    fn test_typescript_object_literal_method_emitted_as_method() {
        // R1-06: method_definition outside class must not be dropped.
        let content = r#"
            const api = {
                fetchData(): number { return 1; },
                get(): string { return "x"; }
            };
            export class Service {
                get(): void {}
            }
        "#;

        let symbols = extract_symbols(content).unwrap().unwrap();

        let object_fetch = symbols
            .iter()
            .find(|s| s.name == "fetchData" && s.kind == SymbolKind::Method)
            .expect("object-literal fetchData as Method");
        assert_eq!(
            object_fetch.qualified_name, None,
            "non-class method keeps QN None"
        );

        let object_get = symbols
            .iter()
            .filter(|s| s.name == "get" && s.kind == SymbolKind::Method)
            .collect::<Vec<_>>();
        assert!(
            object_get.len() >= 2,
            "both object-literal get and Service.get should be present, got: {:?}",
            symbols
                .iter()
                .map(|s| format!("{}:{:?}:{:?}", s.name, s.kind, s.qualified_name))
                .collect::<Vec<_>>()
        );
        assert!(
            object_get
                .iter()
                .any(|s| s.qualified_name.as_deref() == Some("Service.get")),
            "class method keeps Class.method QN"
        );
        assert!(
            object_get.iter().any(|s| s.qualified_name.is_none()),
            "object-literal get has no QN"
        );
    }
}
