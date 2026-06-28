use tree_sitter::{Node, Parser};

#[test]
fn test_dump_rust_tree() {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_rust::LANGUAGE.into())
        .unwrap();

    let content = r#"
        use actix_web::get;
        #[get("/users")]
        pub async fn list_users() {}
    "#;

    let tree = parser.parse(content, None).unwrap();
    let root = tree.root_node();

    print_node(root, content, 0);
}

fn print_node(node: Node, content: &str, depth: usize) {
    let kind = node.kind();
    let start = node.start_byte();
    let end = node.end_byte();
    let text = &content[start..end];
    let display_text = if text.contains('\n') {
        text.split('\n').next().unwrap_or("").to_string() + "..."
    } else {
        text.to_string()
    };

    eprintln!(
        "{:indent$}{} [{} - {}]: {}",
        "",
        kind,
        start,
        end,
        display_text,
        indent = depth * 2
    );

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        print_node(child, content, depth + 1);
    }
}
