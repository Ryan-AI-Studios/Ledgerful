use super::common::{extract_receiver_type, is_exported, node_text};
use crate::index::symbols::{Symbol, SymbolKind};
use miette::{IntoDiagnostic, Result};
use tree_sitter::{Parser, Query, QueryCursor, StreamingIterator};

pub fn extract_symbols(content: &str) -> Result<Option<Vec<Symbol>>> {
    let mut parser = Parser::new();
    let language = tree_sitter_go::LANGUAGE;
    parser.set_language(&language.into()).into_diagnostic()?;

    let tree = parser
        .parse(content, None)
        .ok_or_else(|| miette::miette!("Failed to parse Go content"))?;

    let mut symbols = Vec::new();

    // Functions
    extract_via_query(
        &language.into(),
        &tree,
        content,
        r#"(function_declaration name: (identifier) @name)"#,
        SymbolKind::Function,
        &mut symbols,
    )?;

    // Methods with receiver qualification
    extract_methods(&language.into(), &tree, content, &mut symbols)?;

    // Type specs: struct / interface / other
    extract_type_specs(&language.into(), &tree, content, &mut symbols)?;

    // Top-level const
    extract_via_query(
        &language.into(),
        &tree,
        content,
        r#"(const_declaration (const_spec name: (identifier) @name))"#,
        SymbolKind::Constant,
        &mut symbols,
    )?;

    // Top-level var
    extract_via_query(
        &language.into(),
        &tree,
        content,
        r#"(var_declaration (var_spec name: (identifier) @name))"#,
        SymbolKind::Variable,
        &mut symbols,
    )?;

    // Deterministic order for stable index output
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

fn extract_via_query(
    language: &tree_sitter::Language,
    tree: &tree_sitter::Tree,
    content: &str,
    query_str: &str,
    kind: SymbolKind,
    symbols: &mut Vec<Symbol>,
) -> Result<()> {
    let query = Query::new(language, query_str).into_diagnostic()?;
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&query, tree.root_node(), content.as_bytes());

    while let Some(m) = matches.next() {
        for capture in m.captures {
            let capture_name = query.capture_names()[capture.index as usize];
            if capture_name != "name" {
                continue;
            }
            let name = node_text(capture.node, content);
            if name.is_empty() {
                continue;
            }
            let decl = capture.node.parent().unwrap_or(capture.node);

            // Prefer the enclosing declaration node for line/byte ranges
            let span_node = find_declaration_ancestor(capture.node).unwrap_or(decl);

            symbols.push(Symbol {
                name: name.clone(),
                kind: kind.clone(),
                is_public: is_exported(&name),
                cognitive_complexity: None,
                cyclomatic_complexity: None,
                line_start: Some(span_node.start_position().row as i32 + 1),
                line_end: Some(span_node.end_position().row as i32 + 1),
                qualified_name: None,
                byte_start: Some(span_node.start_byte() as i32),
                byte_end: Some(span_node.end_byte() as i32),
                entrypoint_kind: None,
                metadata: std::collections::BTreeMap::new(),
            });
        }
    }
    Ok(())
}

fn find_declaration_ancestor(node: tree_sitter::Node) -> Option<tree_sitter::Node> {
    let mut current = Some(node);
    while let Some(n) = current {
        match n.kind() {
            "function_declaration"
            | "method_declaration"
            | "type_declaration"
            | "const_declaration"
            | "var_declaration"
            | "type_spec"
            | "const_spec"
            | "var_spec" => return Some(n),
            _ => current = n.parent(),
        }
    }
    None
}

fn extract_methods(
    language: &tree_sitter::Language,
    tree: &tree_sitter::Tree,
    content: &str,
    symbols: &mut Vec<Symbol>,
) -> Result<()> {
    let query = Query::new(
        language,
        r#"(method_declaration name: (field_identifier) @name)"#,
    )
    .into_diagnostic()?;
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&query, tree.root_node(), content.as_bytes());

    while let Some(m) = matches.next() {
        for capture in m.captures {
            let capture_name = query.capture_names()[capture.index as usize];
            if capture_name != "name" {
                continue;
            }
            let name = node_text(capture.node, content);
            if name.is_empty() {
                continue;
            }

            let method_node = capture
                .node
                .parent()
                .filter(|p| p.kind() == "method_declaration")
                .unwrap_or(capture.node);

            let receiver_type = method_node
                .child_by_field_name("receiver")
                .and_then(|r| extract_receiver_type(r, content));

            let qualified_name = receiver_type
                .as_ref()
                .map(|t| format!("{t}.{name}"));

            symbols.push(Symbol {
                name: name.clone(),
                kind: SymbolKind::Method,
                is_public: is_exported(&name),
                cognitive_complexity: None,
                cyclomatic_complexity: None,
                line_start: Some(method_node.start_position().row as i32 + 1),
                line_end: Some(method_node.end_position().row as i32 + 1),
                qualified_name,
                byte_start: Some(method_node.start_byte() as i32),
                byte_end: Some(method_node.end_byte() as i32),
                entrypoint_kind: None,
                metadata: std::collections::BTreeMap::new(),
            });
        }
    }
    Ok(())
}

fn extract_type_specs(
    language: &tree_sitter::Language,
    tree: &tree_sitter::Tree,
    content: &str,
    symbols: &mut Vec<Symbol>,
) -> Result<()> {
    let query = Query::new(
        language,
        r#"(type_declaration (type_spec name: (type_identifier) @name) @spec)"#,
    )
    .into_diagnostic()?;
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&query, tree.root_node(), content.as_bytes());

    while let Some(m) = matches.next() {
        let mut name = String::new();
        let mut spec_node: Option<tree_sitter::Node> = None;

        for capture in m.captures {
            let capture_name = query.capture_names()[capture.index as usize];
            match capture_name {
                "name" => name = node_text(capture.node, content),
                "spec" => spec_node = Some(capture.node),
                _ => {}
            }
        }

        if name.is_empty() {
            continue;
        }

        let kind = spec_node
            .and_then(|spec| {
                // type_spec has field "type"
                let type_node = spec.child_by_field_name("type")?;
                match type_node.kind() {
                    "struct_type" => Some(SymbolKind::Struct),
                    "interface_type" => Some(SymbolKind::Interface),
                    _ => Some(SymbolKind::Type),
                }
            })
            .unwrap_or(SymbolKind::Type);

        let span = spec_node.unwrap_or_else(|| tree.root_node());

        symbols.push(Symbol {
            name: name.clone(),
            kind,
            is_public: is_exported(&name),
            cognitive_complexity: None,
            cyclomatic_complexity: None,
            line_start: Some(span.start_position().row as i32 + 1),
            line_end: Some(span.end_position().row as i32 + 1),
            qualified_name: None,
            byte_start: Some(span.start_byte() as i32),
            byte_end: Some(span.end_byte() as i32),
            entrypoint_kind: None,
            metadata: std::collections::BTreeMap::new(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::symbols::SymbolKind;

    const FIXTURE: &str = r#"
package demo

import "fmt"

const MaxSize = 100
var DefaultName = "x"

type User struct {
    Name string
}

type Reader interface {
    Read() error
}

type Alias = int

func PublicFn() {}

func privateFn() {}

func (u *User) Greet() string {
    return "hi"
}

func (u User) NameLen() int {
    return len(u.Name)
}

func (r *Reader) unused() {}
"#;

    #[test]
    fn extracts_functions_with_export_visibility() {
        let symbols = extract_symbols(FIXTURE).unwrap().unwrap();
        assert!(
            symbols
                .iter()
                .any(|s| s.name == "PublicFn" && s.kind == SymbolKind::Function && s.is_public)
        );
        assert!(
            symbols
                .iter()
                .any(|s| s.name == "privateFn" && s.kind == SymbolKind::Function && !s.is_public)
        );
    }

    #[test]
    fn extracts_methods_with_qualified_name() {
        let symbols = extract_symbols(FIXTURE).unwrap().unwrap();
        let greet = symbols
            .iter()
            .find(|s| s.name == "Greet" && s.kind == SymbolKind::Method)
            .expect("Greet method");
        assert_eq!(greet.qualified_name.as_deref(), Some("User.Greet"));
        assert!(greet.is_public);

        let name_len = symbols
            .iter()
            .find(|s| s.name == "NameLen" && s.kind == SymbolKind::Method)
            .expect("NameLen method");
        assert_eq!(name_len.qualified_name.as_deref(), Some("User.NameLen"));
    }

    #[test]
    fn extracts_struct_and_interface() {
        let symbols = extract_symbols(FIXTURE).unwrap().unwrap();
        assert!(
            symbols
                .iter()
                .any(|s| s.name == "User" && s.kind == SymbolKind::Struct && s.is_public)
        );
        assert!(
            symbols
                .iter()
                .any(|s| s.name == "Reader" && s.kind == SymbolKind::Interface && s.is_public)
        );
    }

    #[test]
    fn extracts_const_and_var() {
        let symbols = extract_symbols(FIXTURE).unwrap().unwrap();
        assert!(
            symbols
                .iter()
                .any(|s| s.name == "MaxSize" && s.kind == SymbolKind::Constant && s.is_public)
        );
        assert!(
            symbols
                .iter()
                .any(|s| s.name == "DefaultName" && s.kind == SymbolKind::Variable && s.is_public)
        );
    }

    #[test]
    fn populates_line_spans() {
        let symbols = extract_symbols(FIXTURE).unwrap().unwrap();
        let public_fn = symbols
            .iter()
            .find(|s| s.name == "PublicFn")
            .expect("PublicFn");
        assert!(public_fn.line_start.is_some());
        assert!(public_fn.line_end.is_some());
        assert!(public_fn.byte_start.is_some());
        assert!(public_fn.byte_end.is_some());
    }
}
