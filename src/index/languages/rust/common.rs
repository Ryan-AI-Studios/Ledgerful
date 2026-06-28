use tree_sitter::Node;

pub fn node_text<'a>(node: Node<'a>, content: &'a str) -> &'a str {
    node.utf8_text(content.as_bytes()).unwrap_or("")
}

pub fn find_child_by_kind<'a>(node: Node<'a>, kind: &str) -> Option<Node<'a>> {
    let mut cursor = node.walk();
    node.children(&mut cursor)
        .find(|&child| child.kind() == kind)
}
