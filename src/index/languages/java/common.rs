use std::cmp::Ordering;
use tree_sitter::Node;

use crate::index::symbols::Symbol;

pub fn node_text(node: Node, content: &str) -> String {
    node.utf8_text(content.as_bytes())
        .unwrap_or("")
        .trim()
        .to_string()
}

pub fn field_text(node: Node, field: &str, content: &str) -> String {
    node.child_by_field_name(field)
        .map(|n| node_text(n, content))
        .unwrap_or_default()
}

pub fn join_qualified(package: &str, enclosing: &[String], name: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    if !package.is_empty() {
        parts.push(package);
    }
    for segment in enclosing {
        if !segment.is_empty() {
            parts.push(segment.as_str());
        }
    }
    if !name.is_empty() {
        parts.push(name);
    }
    parts.join(".")
}

pub fn package_name(root: Node, content: &str) -> String {
    let mut cursor = root.walk();
    for child in root.children(&mut cursor) {
        if child.kind() == "package_declaration" {
            return dotted_name(child, content);
        }
    }
    String::new()
}

/// Inlined `_name` child: `identifier` or `scoped_identifier`.
pub fn dotted_name(node: Node, content: &str) -> String {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if matches!(child.kind(), "identifier" | "scoped_identifier") {
            return node_text(child, content);
        }
    }
    String::new()
}

pub fn has_literal_public(node: Node) -> bool {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() != "modifiers" {
            continue;
        }
        let mut inner = child.walk();
        for modifier in child.children(&mut inner) {
            if modifier.kind() == "public" {
                return true;
            }
        }
    }
    false
}

pub fn nearest_enclosing_type_kind(node: Node) -> Option<&'static str> {
    let mut current = node.parent();
    while let Some(parent) = current {
        match parent.kind() {
            "class_declaration"
            | "record_declaration"
            | "interface_declaration"
            | "enum_declaration"
            | "annotation_type_declaration" => return Some(parent.kind()),
            _ => current = parent.parent(),
        }
    }
    None
}

pub fn is_public_declaration(node: Node, kind: &str) -> bool {
    if kind == "enum_constant" {
        return true;
    }
    if has_literal_public(node) {
        return true;
    }
    matches!(
        nearest_enclosing_type_kind(node),
        Some("interface_declaration" | "annotation_type_declaration")
    )
}

pub fn simple_type_name(node: Node, content: &str) -> String {
    match node.kind() {
        "type_identifier" | "identifier" => node_text(node, content),
        "generic_type" => generic_base_simple_name(node, content),
        "scoped_type_identifier" => last_type_identifier(node, content),
        _ => last_type_identifier(node, content),
    }
}

/// `generic_type` is `type_identifier | scoped_type_identifier` then `type_arguments`.
/// Walk the constructed type only so `new Box<Inner>()` yields `Box`, not `Inner`.
fn generic_base_simple_name(node: Node, content: &str) -> String {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "type_arguments" {
            continue;
        }
        let name = simple_type_name(child, content);
        if !name.is_empty() {
            return name;
        }
    }
    String::new()
}

fn last_type_identifier(node: Node, content: &str) -> String {
    let mut last = String::new();
    let mut stack = vec![node];
    while let Some(current) = stack.pop() {
        if matches!(current.kind(), "type_identifier" | "identifier") {
            let text = node_text(current, content);
            if !text.is_empty() {
                last = text;
            }
        }
        let mut cursor = current.walk();
        let children: Vec<Node> = current.children(&mut cursor).collect();
        for child in children.into_iter().rev() {
            stack.push(child);
        }
    }
    last
}

pub fn sort_symbols(symbols: &mut [Symbol]) {
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
            .then(cmp_opt_i32_none_last(a.line_start, b.line_start))
            .then(cmp_opt_i32_none_last(a.byte_start, b.byte_start))
    });
}

fn cmp_opt_i32_none_last(a: Option<i32>, b: Option<i32>) -> Ordering {
    match (a, b) {
        (Some(x), Some(y)) => x.cmp(&y),
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        (None, None) => Ordering::Equal,
    }
}

pub fn is_type_kind(kind: &str) -> bool {
    matches!(
        kind,
        "class_declaration"
            | "record_declaration"
            | "interface_declaration"
            | "enum_declaration"
            | "annotation_type_declaration"
    )
}

pub fn skip_anonymous_or_enum_constant_body(child: Node) -> bool {
    if child.kind() != "class_body" {
        return false;
    }
    child.parent().is_some_and(|parent| {
        matches!(
            parent.kind(),
            "object_creation_expression" | "enum_constant"
        )
    })
}
