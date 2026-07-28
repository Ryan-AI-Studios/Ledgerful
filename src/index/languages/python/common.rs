use tree_sitter::Node;

/// Extract the attribute name from a Python attribute node (e.g. obj.method -> "method").
/// Used by routes/observability which need the bare method name.
pub fn extract_py_attribute_name(node: Node, content: &str) -> String {
    let mut cursor = node.walk();
    let mut last_ident = String::new();
    for child in node.children(&mut cursor) {
        if child.kind() == "identifier" {
            last_ident = child
                .utf8_text(content.as_bytes())
                .unwrap_or("")
                .to_string();
        }
    }
    last_ident
}

/// Extract a dotted `receiver.field` form from a Python attribute node.
///
/// Mirrors Go's cross-package callee shape so external members like
/// `json.loads` are not collapsed to bare `loads` (0089 Part B / DoD-9).
/// Nested attributes use the full text with dots (e.g. `a.b.c`).
pub fn extract_py_attribute_qualified(node: Node, content: &str) -> String {
    let text = node.utf8_text(content.as_bytes()).unwrap_or("").trim();
    if text.is_empty() {
        return String::new();
    }
    // Prefer the source text when it is a simple dotted path; strip whitespace.
    if text
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.')
    {
        return text.to_string();
    }
    // Fallback: first identifier + last identifier.
    let mut cursor = node.walk();
    let mut idents = Vec::new();
    for child in node.children(&mut cursor) {
        if child.kind() == "identifier"
            && let Ok(t) = child.utf8_text(content.as_bytes())
        {
            idents.push(t.to_string());
        }
    }
    match idents.len() {
        0 => String::new(),
        1 => idents[0].clone(),
        _ => format!("{}.{}", idents[0], idents[idents.len() - 1]),
    }
}
