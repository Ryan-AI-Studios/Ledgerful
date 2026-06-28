use tree_sitter::Node;

/// Extract the attribute name from a Python attribute node (e.g. obj.method -> "method").
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
