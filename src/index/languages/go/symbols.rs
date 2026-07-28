use super::common::{extract_receiver_type, is_exported, node_text};
use crate::index::signature::{
    SignatureParam, SymbolSignatureParts, build_symbol_signature, write_signature_metadata,
};
use crate::index::symbols::{Symbol, SymbolKind};
use miette::{IntoDiagnostic, Result};
use tree_sitter::{Node, Parser, Query, QueryCursor, StreamingIterator};

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
        false,
        &mut symbols,
    )?;

    // Methods with receiver qualification
    extract_methods(&language.into(), &tree, content, &mut symbols)?;

    // Type specs: struct / interface / other
    extract_type_specs(&language.into(), &tree, content, &mut symbols)?;

    // Top-level const only (skip nested inside funcs/methods/closures)
    extract_via_query(
        &language.into(),
        &tree,
        content,
        r#"(const_declaration (const_spec name: (identifier) @name))"#,
        SymbolKind::Constant,
        true,
        &mut symbols,
    )?;

    // Top-level var only (skip nested inside funcs/methods/closures)
    extract_via_query(
        &language.into(),
        &tree,
        content,
        r#"(var_declaration (var_spec name: (identifier) @name))"#,
        SymbolKind::Variable,
        true,
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
    top_level_only: bool,
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
            if top_level_only && is_nested_in_function_scope(capture.node) {
                continue;
            }
            let decl = capture.node.parent().unwrap_or(capture.node);

            // Prefer the enclosing declaration node for line/byte ranges
            let span_node = find_declaration_ancestor(capture.node).unwrap_or(decl);

            let mut metadata = std::collections::BTreeMap::new();
            if matches!(
                span_node.kind(),
                "function_declaration" | "method_declaration"
            ) && let Some(sig) = extract_go_signature(span_node, content, &name)
            {
                write_signature_metadata(&mut metadata, &sig);
            }

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
                metadata,
            });
        }
    }
    Ok(())
}

/// True when `node` is nested inside a function, method, or func literal body.
fn is_nested_in_function_scope(node: tree_sitter::Node) -> bool {
    let mut current = node.parent();
    while let Some(n) = current {
        match n.kind() {
            "function_declaration" | "method_declaration" | "func_literal" => return true,
            "source_file" => return false,
            _ => current = n.parent(),
        }
    }
    false
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

            let qualified_name = receiver_type.as_ref().map(|t| format!("{t}.{name}"));

            let mut metadata = std::collections::BTreeMap::new();
            if let Some(sig) = extract_go_signature(method_node, content, &name) {
                write_signature_metadata(&mut metadata, &sig);
            }

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
                metadata,
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

        let type_node = spec_node.and_then(|spec| spec.child_by_field_name("type"));
        let kind = type_node
            .map(|type_node| match type_node.kind() {
                "struct_type" => SymbolKind::Struct,
                "interface_type" => SymbolKind::Interface,
                _ => SymbolKind::Type,
            })
            .unwrap_or(SymbolKind::Type);

        let span = spec_node.unwrap_or_else(|| tree.root_node());

        symbols.push(Symbol {
            name: name.clone(),
            kind: kind.clone(),
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

        // §2.8b: descend into interface method_spec members so signature changes
        // on the contract surface are visible.
        if kind == SymbolKind::Interface
            && let Some(iface) = type_node
        {
            extract_interface_methods(iface, content, &name, symbols);
        }
    }
    Ok(())
}

/// Extract `method_elem` children of an `interface_type`, qualified as `Iface.Method`.
///
/// Note (0088 Phase 0 P3): the pinned `tree-sitter-go 0.25.0` grammar names this
/// node `method_elem`, not `method_spec` (the name used in some older grammars /
/// web examples). Fields are still `name` / `parameters` / `result`.
fn extract_interface_methods(
    interface_node: Node<'_>,
    content: &str,
    interface_name: &str,
    symbols: &mut Vec<Symbol>,
) {
    let mut cursor = interface_node.walk();
    for child in interface_node.children(&mut cursor) {
        if child.kind() != "method_elem" {
            continue;
        }
        let method_name = child
            .child_by_field_name("name")
            .map(|n| node_text(n, content))
            .filter(|s| !s.is_empty());
        let Some(method_name) = method_name else {
            continue;
        };

        let mut metadata = std::collections::BTreeMap::new();
        if let Some(sig) = extract_go_signature(child, content, &method_name) {
            write_signature_metadata(&mut metadata, &sig);
        }

        symbols.push(Symbol {
            name: method_name.clone(),
            kind: SymbolKind::Method,
            is_public: is_exported(&method_name),
            cognitive_complexity: None,
            cyclomatic_complexity: None,
            line_start: Some(child.start_position().row as i32 + 1),
            line_end: Some(child.end_position().row as i32 + 1),
            qualified_name: Some(format!("{interface_name}.{method_name}")),
            byte_start: Some(child.start_byte() as i32),
            byte_end: Some(child.end_byte() as i32),
            entrypoint_kind: None,
            metadata,
        });
    }
}

/// Build a normalized signature from a Go function/method/method_spec node.
///
/// Go has no function-level behavioural modifiers. Multi-return `result` may be
/// a single type node or a `parameter_list`.
fn extract_go_signature(
    node: Node<'_>,
    content: &str,
    name: &str,
) -> Option<crate::index::signature::SymbolSignature> {
    let params = extract_go_params(node.child_by_field_name("parameters"), content);
    let return_type = extract_go_result(node.child_by_field_name("result"), content);

    let parts = SymbolSignatureParts {
        name: name.to_string(),
        modifiers: Vec::new(),
        params,
        return_type,
    };
    Some(build_symbol_signature(&parts))
}

fn extract_go_params(params_node: Option<Node<'_>>, content: &str) -> Vec<SignatureParam> {
    let Some(params_node) = params_node else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut cursor = params_node.walk();
    for child in params_node.children(&mut cursor) {
        if child.kind() != "parameter_declaration" {
            continue;
        }
        // parameter_declaration may list multiple names sharing one type:
        // `a, b int` → two params with the same type.
        let type_text = child
            .child_by_field_name("type")
            .map(|t| node_text(t, content))
            .filter(|s| !s.is_empty());

        let mut names: Vec<String> = Vec::new();
        let mut c2 = child.walk();
        for grand in child.children(&mut c2) {
            if grand.kind() == "identifier" {
                let n = node_text(grand, content);
                if !n.is_empty() {
                    names.push(n);
                }
            }
        }

        if names.is_empty() {
            // Unnamed parameter (e.g. interface method `Read([]byte) (int, error)`).
            out.push(SignatureParam {
                name: None,
                type_text,
            });
        } else {
            for n in names {
                out.push(SignatureParam {
                    name: Some(n),
                    type_text: type_text.clone(),
                });
            }
        }
    }
    out
}

fn extract_go_result(result_node: Option<Node<'_>>, content: &str) -> Option<String> {
    let result_node = result_node?;
    match result_node.kind() {
        "parameter_list" => {
            // Multi-return: flatten types, preserve order.
            let mut types = Vec::new();
            let mut cursor = result_node.walk();
            for child in result_node.children(&mut cursor) {
                if child.kind() != "parameter_declaration" {
                    continue;
                }
                if let Some(t) = child.child_by_field_name("type") {
                    let text = node_text(t, content);
                    if !text.is_empty() {
                        // One type entry per name (or one if unnamed).
                        let name_count = {
                            let mut c2 = child.walk();
                            let n = child
                                .children(&mut c2)
                                .filter(|g| g.kind() == "identifier")
                                .count();
                            n.max(1)
                        };
                        for _ in 0..name_count {
                            types.push(text.clone());
                        }
                    }
                }
            }
            if types.is_empty() {
                None
            } else {
                Some(types.join(", "))
            }
        }
        _ => {
            let text = node_text(result_node, content);
            if text.is_empty() { None } else { Some(text) }
        }
    }
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

    #[test]
    fn skips_nested_const_and_var_inside_functions() {
        let content = r#"
package demo

const Top = 1
var Global = 2

func outer() {
    const nestedConst = 3
    var nestedVar = 4
    _ = nestedConst
    _ = nestedVar
}

func (u *User) method() {
    const methodConst = 5
    var methodVar = 6
    _ = methodConst
    _ = methodVar
}

type User struct{}
"#;
        let symbols = extract_symbols(content).unwrap().unwrap();
        assert!(
            symbols
                .iter()
                .any(|s| s.name == "Top" && s.kind == SymbolKind::Constant)
        );
        assert!(
            symbols
                .iter()
                .any(|s| s.name == "Global" && s.kind == SymbolKind::Variable)
        );
        assert!(
            symbols.iter().all(|s| s.name != "nestedConst"),
            "nested const inside function must not be emitted"
        );
        assert!(
            symbols.iter().all(|s| s.name != "nestedVar"),
            "nested var inside function must not be emitted"
        );
        assert!(
            symbols.iter().all(|s| s.name != "methodConst"),
            "const inside method must not be emitted"
        );
        assert!(
            symbols.iter().all(|s| s.name != "methodVar"),
            "var inside method must not be emitted"
        );
    }

    #[test]
    fn extracts_function_signature_with_multi_return() {
        let content = r#"
package demo

func Split(s string, sep string) (string, error) {
    return s, nil
}
"#;
        let symbols = extract_symbols(content).unwrap().unwrap();
        let split = symbols
            .iter()
            .find(|s| s.name == "Split" && s.kind == SymbolKind::Function)
            .expect("Split");
        let sig = split.metadata.get("signature").expect("signature");
        let shape = split
            .metadata
            .get("signatureShape")
            .expect("signatureShape");
        assert!(sig.contains("s: string") || sig.contains("string"), "{sig}");
        assert!(
            shape.contains("params=string,string"),
            "shape params: {shape}"
        );
        assert!(
            shape.contains("ret=string, error") || shape.contains("ret=string,error"),
            "multi-return in shape: {shape}"
        );
    }

    #[test]
    fn extracts_interface_method_specs() {
        let content = r#"
package demo

type Reader interface {
    Read(p []byte) (n int, err error)
    Close() error
}
"#;
        let symbols = extract_symbols(content).unwrap().unwrap();
        assert!(
            symbols
                .iter()
                .any(|s| s.name == "Reader" && s.kind == SymbolKind::Interface)
        );
        let read = symbols
            .iter()
            .find(|s| s.name == "Read" && s.kind == SymbolKind::Method)
            .expect("interface method Read must be indexed");
        assert_eq!(read.qualified_name.as_deref(), Some("Reader.Read"));
        assert!(read.metadata.contains_key("signatureShape"));
        let close = symbols
            .iter()
            .find(|s| s.name == "Close" && s.kind == SymbolKind::Method)
            .expect("Close");
        assert_eq!(close.qualified_name.as_deref(), Some("Reader.Close"));
    }

    #[test]
    fn method_signature_includes_params() {
        let content = r#"
package demo

type User struct{}

func (u *User) Greet(name string) string {
    return name
}
"#;
        let symbols = extract_symbols(content).unwrap().unwrap();
        let greet = symbols
            .iter()
            .find(|s| s.name == "Greet" && s.kind == SymbolKind::Method)
            .expect("Greet");
        let shape = greet
            .metadata
            .get("signatureShape")
            .expect("signatureShape");
        assert!(shape.contains("params=string"), "{shape}");
        assert!(shape.contains("ret=string"), "{shape}");
    }

    #[test]
    fn signature_hash_non_null_via_project_symbol() {
        use crate::index::types::symbol_to_project_symbol;
        let content = r#"
package demo
func Add(a int, b int) int { return a + b }
"#;
        let symbols = extract_symbols(content).unwrap().unwrap();
        let add = symbols.iter().find(|s| s.name == "Add").expect("Add");
        let ps = symbol_to_project_symbol(add, 1, "now");
        assert!(
            ps.signature_hash.is_some(),
            "Go function must yield non-null signature_hash"
        );
    }

    /// Phase 0 P1/P3: confirm field names against the pinned tree-sitter-go grammar.
    #[test]
    fn phase0_go_definition_node_fields() {
        let content = r#"
package demo
func Foo(a int) (int, error) { return a, nil }
type R interface { Read([]byte) (int, error) }
"#;
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_go::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(content, None).unwrap();
        let root = tree.root_node();

        fn find_kind<'a>(node: tree_sitter::Node<'a>, kind: &str) -> Option<tree_sitter::Node<'a>> {
            if node.kind() == kind {
                return Some(node);
            }
            let mut c = node.walk();
            for child in node.children(&mut c) {
                if let Some(found) = find_kind(child, kind) {
                    return Some(found);
                }
            }
            None
        }

        let fd = find_kind(root, "function_declaration").expect("function_declaration");
        assert!(
            fd.child_by_field_name("parameters").is_some(),
            "function_declaration.parameters"
        );
        assert!(
            fd.child_by_field_name("result").is_some(),
            "function_declaration.result (not return_type)"
        );

        // Pinned grammar 0.25.0: method_elem (not method_spec).
        let ms = find_kind(root, "method_elem").expect("method_elem inside interface");
        assert!(
            ms.child_by_field_name("parameters").is_some(),
            "method_elem.parameters"
        );
        assert!(
            ms.child_by_field_name("result").is_some(),
            "method_elem.result"
        );
        assert!(
            find_kind(root, "method_spec").is_none(),
            "pinned grammar uses method_elem, not method_spec"
        );
    }
}
