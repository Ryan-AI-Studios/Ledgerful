use tree_sitter::Node;

/// Extract a UTF-8 string from a node, returning empty on failure.
pub fn node_text(node: Node, content: &str) -> String {
    node.utf8_text(content.as_bytes()).unwrap_or("").to_string()
}

/// Whether a C++ identifier is "public" for index purposes.
///
/// C/C++ has no language-level export rule like Go. Treat names that do not
/// start with `_` as public (stdlib / user convention floor).
pub fn is_public_name(name: &str) -> bool {
    !name.is_empty() && !name.starts_with('_')
}

/// Truncate evidence strings to a reasonable length.
pub fn truncate_evidence(text: &str, max: usize) -> String {
    if text.len() > max {
        format!("{}...", &text[..max.saturating_sub(3)])
    } else {
        text.to_string()
    }
}

/// Walk a C/C++ declarator (or any nested shape) for the leaf name.
///
/// **Load-bearing:** `function_definition` and `type_definition` have **no**
/// `name` field in tree-sitter-cpp — only `declarator` / `type` / `body`.
/// Call sites must use this helper instead of `child_by_field_name("name")`.
///
/// Handles: `identifier`, `field_identifier`, `qualified_identifier`,
/// `destructor_name`, `operator_name`, `template_function`, `operator_cast`,
/// and wrappers (`function_declarator`, `pointer_declarator`,
/// `reference_declarator`, `parenthesized_declarator`, etc.).
pub fn cpp_declarator_name(node: Node, content: &str) -> Option<String> {
    match node.kind() {
        "identifier" | "field_identifier" | "type_identifier" => {
            let name = node_text(node, content);
            if name.is_empty() { None } else { Some(name) }
        }
        "destructor_name" => {
            // ~Foo → "~Foo" (stable, searchable).
            let text = node_text(node, content).replace(' ', "");
            if text.is_empty() { None } else { Some(text) }
        }
        "operator_name" => {
            // operator+, operator bool, …
            let text = node_text(node, content);
            let collapsed = collapse_ws(&text);
            if collapsed.is_empty() {
                None
            } else {
                Some(collapsed)
            }
        }
        "operator_cast" => {
            // operator int / operator bool — full text is stable.
            let text = node_text(node, content);
            let collapsed = collapse_ws(&text);
            if collapsed.is_empty() {
                None
            } else {
                Some(collapsed)
            }
        }
        "qualified_identifier" => {
            // A::B::foo → prefer trailing name segment.
            if let Some(name_field) = node.child_by_field_name("name") {
                return cpp_declarator_name(name_field, content);
            }
            trailing_identifier(node, content)
        }
        "template_function" => {
            // foo<T> or A::foo<T> — name field or nested identifier.
            if let Some(name_field) = node.child_by_field_name("name") {
                return cpp_declarator_name(name_field, content);
            }
            trailing_identifier(node, content)
        }
        "function_declarator"
        | "pointer_declarator"
        | "reference_declarator"
        | "parenthesized_declarator"
        | "array_declarator"
        | "abstract_function_declarator"
        | "abstract_pointer_declarator"
        | "abstract_reference_declarator"
        | "abstract_parenthesized_declarator"
        | "abstract_array_declarator"
        | "field_declarator"
        | "pointer_field_declarator"
        | "reference_field_declarator"
        | "array_field_declarator"
        | "function_field_declarator"
        | "attributed_declarator" => {
            // Prefer explicit `declarator` field, then walk children.
            if let Some(inner) = node.child_by_field_name("declarator") {
                return cpp_declarator_name(inner, content);
            }
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if let Some(name) = cpp_declarator_name(child, content) {
                    return Some(name);
                }
            }
            None
        }
        _ => {
            // Nested search for known name-bearing kinds (depth-first, first hit).
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if matches!(
                    child.kind(),
                    "identifier"
                        | "field_identifier"
                        | "type_identifier"
                        | "qualified_identifier"
                        | "destructor_name"
                        | "operator_name"
                        | "operator_cast"
                        | "template_function"
                        | "function_declarator"
                        | "pointer_declarator"
                        | "reference_declarator"
                        | "parenthesized_declarator"
                        | "field_declarator"
                        | "function_field_declarator"
                        | "pointer_field_declarator"
                        | "reference_field_declarator"
                ) && let Some(name) = cpp_declarator_name(child, content)
                {
                    return Some(name);
                }
            }
            None
        }
    }
}

fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn trailing_identifier(node: Node, content: &str) -> Option<String> {
    let mut last = None;
    let mut stack = vec![node];
    while let Some(n) = stack.pop() {
        if matches!(
            n.kind(),
            "identifier" | "field_identifier" | "type_identifier" | "destructor_name"
        ) {
            let t = node_text(n, content);
            if !t.is_empty() {
                last = Some(t);
            }
        }
        if n.kind() == "operator_name" || n.kind() == "operator_cast" {
            let t = collapse_ws(&node_text(n, content));
            if !t.is_empty() {
                last = Some(t);
            }
        }
        let mut cursor = n.walk();
        // Preserve left-to-right: push reversed so first children pop last.
        let children: Vec<_> = n.children(&mut cursor).collect();
        for child in children.into_iter().rev() {
            stack.push(child);
        }
    }
    last
}

/// Unwrap a call_expression callee to a leaf name for same-file resolution.
///
/// Walks through `template_function`, `qualified_identifier`, and
/// `field_expression` (`.` / `->`) to a leaf identifier.
pub fn unwrap_callee_name(node: Node, content: &str) -> Option<String> {
    match node.kind() {
        "identifier" | "field_identifier" => {
            let name = node_text(node, content);
            if name.is_empty() { None } else { Some(name) }
        }
        "qualified_identifier" => {
            if let Some(name_field) = node.child_by_field_name("name") {
                return unwrap_callee_name(name_field, content);
            }
            trailing_identifier(node, content)
        }
        "template_function" => {
            if let Some(name_field) = node.child_by_field_name("name") {
                return unwrap_callee_name(name_field, content);
            }
            trailing_identifier(node, content)
        }
        "field_expression" => {
            // obj.method / ptr->method — field is the callee name.
            if let Some(field) = node.child_by_field_name("field") {
                return unwrap_callee_name(field, content);
            }
            trailing_identifier(node, content)
        }
        "pointer_expression" | "parenthesized_expression" | "binary_expression" => {
            // Rare: (*fp)() etc. — walk children.
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if let Some(name) = unwrap_callee_name(child, content) {
                    return Some(name);
                }
            }
            None
        }
        _ => {
            // Fallback: try field name, then first identifier-ish descendant.
            if let Some(name_field) = node.child_by_field_name("name")
                && let Some(n) = unwrap_callee_name(name_field, content)
            {
                return Some(n);
            }
            if let Some(field) = node.child_by_field_name("field")
                && let Some(n) = unwrap_callee_name(field, content)
            {
                return Some(n);
            }
            trailing_identifier(node, content)
        }
    }
}

/// True when `node` is nested inside a function_definition or lambda body.
pub fn is_nested_in_function_scope(node: Node) -> bool {
    let mut current = node.parent();
    while let Some(n) = current {
        match n.kind() {
            "function_definition" | "lambda_expression" => return true,
            "translation_unit" => return false,
            _ => current = n.parent(),
        }
    }
    false
}

/// True when `node` is nested under a class/struct/union body (method floor).
pub fn enclosing_type_name(node: Node, content: &str) -> Option<String> {
    let mut current = node.parent();
    while let Some(n) = current {
        match n.kind() {
            "class_specifier" | "struct_specifier" | "union_specifier" => {
                if let Some(name_node) = n.child_by_field_name("name") {
                    let name = node_text(name_node, content);
                    if !name.is_empty() {
                        return Some(name);
                    }
                }
            }
            "translation_unit" => return None,
            _ => {}
        }
        current = n.parent();
    }
    None
}

/// Walk up looking for an enclosing function_definition / lambda and return a display name.
pub fn find_enclosing_function(node: Node, content: &str) -> String {
    let mut current = node.parent();
    while let Some(parent) = current {
        match parent.kind() {
            "function_definition" => {
                if let Some(decl) = parent.child_by_field_name("declarator")
                    && let Some(name) = cpp_declarator_name(decl, content)
                {
                    return name;
                }
                return "anonymous".to_string();
            }
            "lambda_expression" => return "lambda".to_string(),
            _ => {}
        }
        current = parent.parent();
    }
    "<tu>".to_string()
}

/// Strip surrounding `"` / `<>` from a `#include` path token (D12).
pub fn strip_include_delimiters(raw: &str) -> String {
    let t = raw.trim();
    if t.len() >= 2 {
        let bytes = t.as_bytes();
        if (bytes[0] == b'"' && bytes[t.len() - 1] == b'"')
            || (bytes[0] == b'<' && bytes[t.len() - 1] == b'>')
        {
            return t[1..t.len() - 1].to_string();
        }
    }
    t.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_include_quote_and_angle() {
        assert_eq!(strip_include_delimiters("\"module.hpp\""), "module.hpp");
        assert_eq!(strip_include_delimiters("<vector>"), "vector");
        assert_eq!(strip_include_delimiters("  <string>  "), "string");
        assert_eq!(strip_include_delimiters("bare.hpp"), "bare.hpp");
    }

    #[test]
    fn declarator_name_from_simple_function() {
        let content = "int add(int a, int b) { return a + b; }\n";
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_cpp::LANGUAGE.into())
            .expect("lang");
        let tree = parser.parse(content, None).expect("parse");
        let root = tree.root_node();
        let mut found = None;
        let mut stack = vec![root];
        while let Some(n) = stack.pop() {
            if n.kind() == "function_definition"
                && let Some(decl) = n.child_by_field_name("declarator")
            {
                found = cpp_declarator_name(decl, content);
                break;
            }
            let mut c = n.walk();
            for child in n.children(&mut c) {
                stack.push(child);
            }
        }
        assert_eq!(found.as_deref(), Some("add"));
    }
}
