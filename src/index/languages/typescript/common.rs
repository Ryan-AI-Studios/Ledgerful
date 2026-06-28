use tree_sitter::Node;

/// Extract the method name from a member_expression (e.g. obj.method -> "method").
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
