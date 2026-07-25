use tree_sitter::Node;

/// Extract a UTF-8 string from a node, returning empty on failure.
pub fn node_text(node: Node, content: &str) -> String {
    node.utf8_text(content.as_bytes()).unwrap_or("").to_string()
}

/// Whether a Go identifier is exported (starts with an uppercase letter).
pub fn is_exported(name: &str) -> bool {
    name.chars().next().is_some_and(|c| c.is_uppercase())
}

/// Extract the receiver type name from a method_declaration's receiver field.
/// Strips a leading `*` for pointer receivers (e.g. `*User` → `User`).
pub fn extract_receiver_type(receiver_node: Node, content: &str) -> Option<String> {
    // receiver is a parameter_list containing parameter_declaration nodes
    let mut cursor = receiver_node.walk();
    for param in receiver_node.children(&mut cursor) {
        if param.kind() != "parameter_declaration" {
            continue;
        }
        if let Some(type_name) = type_identifier_from_type_node(param, content) {
            return Some(type_name);
        }
    }
    None
}

/// Walk a type node (or parameter_declaration containing a type) for a type_identifier,
/// stripping pointer_type wrappers.
fn type_identifier_from_type_node(node: Node, content: &str) -> Option<String> {
    // Direct type field on parameter_declaration
    if let Some(type_node) = node.child_by_field_name("type") {
        return type_identifier_from_type_node(type_node, content);
    }

    match node.kind() {
        "type_identifier" => {
            let name = node_text(node, content);
            if name.is_empty() { None } else { Some(name) }
        }
        "pointer_type" => {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if let Some(name) = type_identifier_from_type_node(child, content) {
                    return Some(name);
                }
            }
            None
        }
        "qualified_type" => {
            // pkg.Type — take the trailing type_identifier
            let mut cursor = node.walk();
            let mut last = None;
            for child in node.children(&mut cursor) {
                if child.kind() == "type_identifier" {
                    last = Some(node_text(child, content));
                }
            }
            last.filter(|s| !s.is_empty())
        }
        _ => {
            // Search descendants for type_identifier / pointer_type
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if let Some(name) = type_identifier_from_type_node(child, content) {
                    return Some(name);
                }
            }
            None
        }
    }
}

/// Extract the field/method name from a selector_expression (e.g. `fmt.Println` → `"Println"`).
pub fn extract_selector_field(node: Node, content: &str) -> String {
    if let Some(field) = node.child_by_field_name("field") {
        return node_text(field, content);
    }
    // Fallback: last identifier/field_identifier child
    let mut cursor = node.walk();
    let mut last = String::new();
    for child in node.children(&mut cursor) {
        if matches!(
            child.kind(),
            "identifier" | "field_identifier" | "type_identifier"
        ) {
            last = node_text(child, content);
        }
    }
    last
}

/// Extract the operand (left side) of a selector_expression (e.g. `fmt.Println` → `"fmt"`).
pub fn extract_selector_operand(node: Node, content: &str) -> String {
    if let Some(operand) = node.child_by_field_name("operand") {
        return node_text(operand, content);
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if matches!(child.kind(), "identifier" | "selector_expression") {
            return node_text(child, content);
        }
    }
    String::new()
}

/// Truncate evidence strings to a reasonable length.
pub fn truncate_evidence(text: &str, max: usize) -> String {
    if text.len() > max {
        format!("{}...", &text[..max.saturating_sub(3)])
    } else {
        text.to_string()
    }
}

/// Walk up the tree looking for an enclosing function_declaration / method_declaration /
/// func_literal and return a display name.
pub fn find_enclosing_function(node: Node, content: &str) -> String {
    let mut current = node.parent();
    while let Some(parent) = current {
        match parent.kind() {
            "function_declaration" | "method_declaration" => {
                if let Some(name_node) = parent.child_by_field_name("name") {
                    return node_text(name_node, content);
                }
            }
            "func_literal" => return "<func_literal>".to_string(),
            _ => {}
        }
        current = parent.parent();
    }
    "<package>".to_string()
}

/// True if the node is inside a function named like a Go test (`Test*`, `Benchmark*`, `Example*`).
pub fn is_in_go_test(node: Node, content: &str) -> bool {
    let mut current = node.parent();
    while let Some(parent) = current {
        if parent.kind() == "function_declaration"
            && let Some(name_node) = parent.child_by_field_name("name")
        {
            let name = node_text(name_node, content);
            if name.starts_with("Test")
                || name.starts_with("Benchmark")
                || name.starts_with("Example")
            {
                return true;
            }
        }
        current = parent.parent();
    }
    false
}

/// Collect import path strings (without quotes) and optional local aliases from the file.
/// Returns (alias_or_package_name, full_import_path) pairs.
pub fn collect_imports(root: Node, content: &str) -> Vec<(String, String)> {
    let mut imports = Vec::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.kind() == "import_spec" {
            let path = node
                .child_by_field_name("path")
                .map(|n| node_text(n, content))
                .unwrap_or_default()
                .trim_matches('"')
                .to_string();
            if path.is_empty() {
                // continue walking
            } else {
                // Optional name: identifier, blank_identifier, or package clause alias
                let alias = node
                    .child_by_field_name("name")
                    .map(|n| node_text(n, content))
                    .filter(|s| !s.is_empty() && s != "_")
                    .unwrap_or_else(|| {
                        // Default package name is the last path segment
                        path.rsplit('/').next().unwrap_or(&path).to_string()
                    });
                imports.push((alias, path));
            }
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            stack.push(child);
        }
    }
    imports.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
    imports
}
