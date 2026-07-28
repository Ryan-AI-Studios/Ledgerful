use tree_sitter::Node;

/// Extract the method name from a member_expression (e.g. obj.method -> "method").
/// Used by routes/observability which need the bare method name.
pub fn extract_ts_member_name(node: Node, content: &str) -> String {
    let mut cursor = node.walk();
    let mut last_ident = String::new();
    for child in node.children(&mut cursor) {
        if child.kind() == "property_identifier" || child.kind() == "identifier" {
            last_ident = child
                .utf8_text(content.as_bytes())
                .unwrap_or("")
                .to_string();
        }
    }
    last_ident
}

/// Extract a dotted `receiver.field` form from a TS member_expression.
///
/// Mirrors Go's cross-package callee shape so external members like
/// `axios.get` are not collapsed to bare `get` (0089 Part B / DoD-9).
pub fn extract_ts_member_qualified(node: Node, content: &str) -> String {
    let text = node.utf8_text(content.as_bytes()).unwrap_or("").trim();
    if text.is_empty() {
        return String::new();
    }
    // Simple identifier paths (no optional chaining / computed members).
    if text
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '$')
    {
        return text.to_string();
    }
    let obj = extract_ts_object_name(node, content);
    let field = extract_ts_member_name(node, content);
    if obj.is_empty() {
        field
    } else if field.is_empty() {
        obj
    } else {
        format!("{obj}.{field}")
    }
}

/// Extract the object name from a member_expression (e.g. app.get -> "app").
pub fn extract_ts_object_name(node: Node, content: &str) -> String {
    let mut cursor = node.walk();
    // The first child is typically the object
    for child in node.children(&mut cursor) {
        if child.kind() == "identifier" || child.kind() == "member_expression" {
            // For simple identifiers, return the name.
            // For nested member expressions (e.g. this.app), take the last identifier.
            if child.kind() == "identifier" {
                return child
                    .utf8_text(content.as_bytes())
                    .unwrap_or("")
                    .to_string();
            }
        }
    }
    String::new()
}
