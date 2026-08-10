use super::common::{
    cpp_declarator_name, enclosing_type_name, is_nested_in_function_scope, is_public_name,
    node_text,
};
use crate::index::symbols::{Symbol, SymbolKind};
use miette::{IntoDiagnostic, Result};
use tree_sitter::{Node, Parser};

pub fn extract_symbols(content: &str) -> Result<Option<Vec<Symbol>>> {
    let mut parser = Parser::new();
    let language = tree_sitter_cpp::LANGUAGE;
    parser.set_language(&language.into()).into_diagnostic()?;

    let tree = parser
        .parse(content, None)
        .ok_or_else(|| miette::miette!("Failed to parse C/C++ content"))?;

    let mut symbols = Vec::new();
    collect_cpp_symbols(tree.root_node(), content, &mut symbols);

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

fn collect_cpp_symbols(node: Node, content: &str, symbols: &mut Vec<Symbol>) {
    match node.kind() {
        "function_definition" => {
            push_function_like(node, content, symbols, /*prefer_method_if_type*/ true);
        }
        // In-class method prototypes (no body) are declaration / field_declaration,
        // not function_definition — see tree-sitter-cpp grammar dump.
        "declaration" | "field_declaration" => {
            if let Some(decl) = node.child_by_field_name("declarator")
                && is_function_like_declarator(decl)
            {
                push_function_like(node, content, symbols, /*prefer_method_if_type*/ true);
            }
        }
        "class_specifier" | "struct_specifier" | "union_specifier" | "enum_specifier" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, content);
                if !name.is_empty() {
                    let kind = match node.kind() {
                        "class_specifier" => SymbolKind::Class,
                        "struct_specifier" => SymbolKind::Struct,
                        "union_specifier" => SymbolKind::Type,
                        "enum_specifier" => SymbolKind::Enum,
                        _ => SymbolKind::Type,
                    };
                    symbols.push(Symbol {
                        name: name.clone(),
                        kind,
                        is_public: is_public_name(&name),
                        cognitive_complexity: None,
                        cyclomatic_complexity: None,
                        line_start: Some(node.start_position().row as i32 + 1),
                        line_end: Some(node.end_position().row as i32 + 1),
                        qualified_name: None,
                        byte_start: Some(node.start_byte() as i32),
                        byte_end: Some(node.end_byte() as i32),
                        entrypoint_kind: None,
                        metadata: std::collections::BTreeMap::new(),
                    });
                }
            }
            // Continue into body for methods — fall through to children walk below.
        }
        "type_definition" => {
            // typedef — also no `name` field; reuse declarator helper (0165-D).
            if !is_nested_in_function_scope(node)
                && let Some(decl) = node.child_by_field_name("declarator")
                && let Some(name) = cpp_declarator_name(decl, content)
            {
                symbols.push(Symbol {
                    name: name.clone(),
                    kind: SymbolKind::Type,
                    is_public: is_public_name(&name),
                    cognitive_complexity: None,
                    cyclomatic_complexity: None,
                    line_start: Some(node.start_position().row as i32 + 1),
                    line_end: Some(node.end_position().row as i32 + 1),
                    qualified_name: None,
                    byte_start: Some(node.start_byte() as i32),
                    byte_end: Some(node.end_byte() as i32),
                    entrypoint_kind: None,
                    metadata: std::collections::BTreeMap::new(),
                });
            }
        }
        "namespace_definition" => {
            // No SymbolKind::Namespace — map named namespaces to Module (spec §7.2).
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, content);
                if !name.is_empty() {
                    symbols.push(Symbol {
                        name: name.clone(),
                        kind: SymbolKind::Module,
                        is_public: is_public_name(&name),
                        cognitive_complexity: None,
                        cyclomatic_complexity: None,
                        line_start: Some(node.start_position().row as i32 + 1),
                        line_end: Some(node.end_position().row as i32 + 1),
                        qualified_name: None,
                        byte_start: Some(node.start_byte() as i32),
                        byte_end: Some(node.end_byte() as i32),
                        entrypoint_kind: None,
                        metadata: std::collections::BTreeMap::new(),
                    });
                }
            }
        }
        _ => {}
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_cpp_symbols(child, content, symbols);
    }
}

fn push_function_like(
    node: Node,
    content: &str,
    symbols: &mut Vec<Symbol>,
    prefer_method_if_type: bool,
) {
    let Some(decl) = node.child_by_field_name("declarator") else {
        return;
    };
    let Some(name) = cpp_declarator_name(decl, content) else {
        return;
    };

    let type_enclosing = enclosing_type_name(node, content);
    let is_method =
        (prefer_method_if_type && type_enclosing.is_some()) || is_qualified_declarator(decl);
    let kind = if is_method {
        SymbolKind::Method
    } else {
        SymbolKind::Function
    };
    let qualified_name = type_enclosing
        .as_ref()
        .map(|t| format!("{t}.{name}"))
        .or_else(|| {
            if is_qualified_declarator(decl) {
                Some(qualified_display(decl, content, &name))
            } else {
                None
            }
        });

    symbols.push(Symbol {
        name: name.clone(),
        kind,
        is_public: is_public_name(&name),
        cognitive_complexity: None,
        cyclomatic_complexity: None,
        line_start: Some(node.start_position().row as i32 + 1),
        line_end: Some(node.end_position().row as i32 + 1),
        qualified_name,
        byte_start: Some(node.start_byte() as i32),
        byte_end: Some(node.end_byte() as i32),
        entrypoint_kind: None,
        metadata: std::collections::BTreeMap::new(),
    });
}

/// True when a declaration's declarator is a function / method / operator (not a data field).
fn is_function_like_declarator(node: Node) -> bool {
    let mut stack = vec![node];
    while let Some(n) = stack.pop() {
        if matches!(
            n.kind(),
            "function_declarator"
                | "function_field_declarator"
                | "abstract_function_declarator"
                | "operator_cast"
                | "destructor_name"
                | "operator_name"
        ) {
            return true;
        }
        // Stop descending into parameter lists to avoid false positives.
        if matches!(n.kind(), "parameter_list" | "argument_list") {
            continue;
        }
        let mut cursor = n.walk();
        for child in n.children(&mut cursor) {
            stack.push(child);
        }
    }
    false
}

fn is_qualified_declarator(node: Node) -> bool {
    let mut stack = vec![node];
    while let Some(n) = stack.pop() {
        if n.kind() == "qualified_identifier" {
            return true;
        }
        let mut cursor = n.walk();
        for child in n.children(&mut cursor) {
            stack.push(child);
        }
    }
    false
}

fn qualified_display(decl: Node, content: &str, leaf: &str) -> String {
    // Prefer Class.method style from qualified_identifier text.
    let mut stack = vec![decl];
    while let Some(n) = stack.pop() {
        if n.kind() == "qualified_identifier" {
            let text = node_text(n, content).replace("::", ".");
            let collapsed = text.split_whitespace().collect::<Vec<_>>().join("");
            if !collapsed.is_empty() {
                return collapsed;
            }
        }
        let mut cursor = n.walk();
        for child in n.children(&mut cursor) {
            stack.push(child);
        }
    }
    leaf.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::symbols::SymbolKind;

    const FIXTURE: &str = r#"
#include "module.hpp"
#include <vector>

typedef int Count;

namespace demo {
class Widget {
public:
    Widget();
    ~Widget();
    int value() const;
    Widget& operator+=(int n);
    operator bool() const;
private:
    int x_;
};

struct Point {
    int x;
    int y;
};

enum Color { Red, Green, Blue };

union Pack {
    int i;
    float f;
};

int add(int a, int b) {
    return a + b;
}

void use_add() {
    (void)add(1, 2);
}
}  // namespace demo
"#;

    #[test]
    fn extracts_free_function_non_anonymous() {
        let symbols = extract_symbols(FIXTURE).unwrap().unwrap();
        let add = symbols
            .iter()
            .find(|s| s.name == "add" && s.kind == SymbolKind::Function)
            .expect("add free function");
        assert!(add.is_public);
        assert!(add.line_start.is_some());
    }

    #[test]
    fn extracts_class_struct_enum_union() {
        let symbols = extract_symbols(FIXTURE).unwrap().unwrap();
        assert!(
            symbols
                .iter()
                .any(|s| s.name == "Widget" && s.kind == SymbolKind::Class)
        );
        assert!(
            symbols
                .iter()
                .any(|s| s.name == "Point" && s.kind == SymbolKind::Struct)
        );
        assert!(
            symbols
                .iter()
                .any(|s| s.name == "Color" && s.kind == SymbolKind::Enum)
        );
        assert!(
            symbols
                .iter()
                .any(|s| s.name == "Pack" && s.kind == SymbolKind::Type)
        );
    }

    #[test]
    fn extracts_typedef_via_declarator_helper() {
        let symbols = extract_symbols(FIXTURE).unwrap().unwrap();
        assert!(
            symbols
                .iter()
                .any(|s| s.name == "Count" && s.kind == SymbolKind::Type),
            "typedef Count; got {:?}",
            symbols
                .iter()
                .map(|s| (&s.name, s.kind.as_str()))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn extracts_methods_ctor_dtor_operator() {
        let symbols = extract_symbols(FIXTURE).unwrap().unwrap();
        // Methods inside class body
        assert!(
            symbols
                .iter()
                .any(|s| s.name == "value" && s.kind == SymbolKind::Method),
            "value method; symbols={:?}",
            symbols
                .iter()
                .filter(|s| s.kind == SymbolKind::Method)
                .map(|s| &s.name)
                .collect::<Vec<_>>()
        );
        // ctor shares class name
        assert!(
            symbols
                .iter()
                .any(|s| s.name == "Widget" && s.kind == SymbolKind::Method),
            "ctor Widget method"
        );
        // dtor
        assert!(
            symbols
                .iter()
                .any(|s| s.name.starts_with('~') && s.kind == SymbolKind::Method),
            "destructor; got {:?}",
            symbols.iter().map(|s| &s.name).collect::<Vec<_>>()
        );
        // operator
        assert!(
            symbols.iter().any(|s| {
                s.kind == SymbolKind::Method
                    && (s.name.contains("operator") || s.name.starts_with("operator"))
            }),
            "operator method; got {:?}",
            symbols
                .iter()
                .filter(|s| s.kind == SymbolKind::Method)
                .map(|s| &s.name)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn namespace_mapped_to_module() {
        let symbols = extract_symbols(FIXTURE).unwrap().unwrap();
        assert!(
            symbols
                .iter()
                .any(|s| s.name == "demo" && s.kind == SymbolKind::Module)
        );
    }

    #[test]
    fn symbols_are_deterministically_sorted() {
        let a = extract_symbols(FIXTURE).unwrap().unwrap();
        let b = extract_symbols(FIXTURE).unwrap().unwrap();
        let names_a: Vec<_> = a
            .iter()
            .map(|s| (s.name.clone(), s.kind.as_str().to_string()))
            .collect();
        let names_b: Vec<_> = b
            .iter()
            .map(|s| (s.name.clone(), s.kind.as_str().to_string()))
            .collect();
        assert_eq!(names_a, names_b);
        // Sorted by name then kind
        for w in names_a.windows(2) {
            assert!(
                w[0].0 < w[1].0 || (w[0].0 == w[1].0 && w[0].1 <= w[1].1),
                "not sorted: {:?} then {:?}",
                w[0],
                w[1]
            );
        }
    }

    #[test]
    fn no_anonymous_named_functions() {
        let symbols = extract_symbols(FIXTURE).unwrap().unwrap();
        for s in symbols
            .iter()
            .filter(|s| matches!(s.kind, SymbolKind::Function | SymbolKind::Method))
        {
            assert_ne!(s.name, "anonymous", "unexpected anonymous: {s:?}");
            assert!(!s.name.is_empty());
        }
    }
}
