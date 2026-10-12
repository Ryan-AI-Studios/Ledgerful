use super::common::{
    field_text, is_public_declaration, join_qualified, package_name,
    skip_anonymous_or_enum_constant_body, sort_symbols,
};
use crate::index::symbols::{Symbol, SymbolKind};
use miette::{IntoDiagnostic, Result};
use tree_sitter::{Node, Parser};

pub fn extract_symbols(content: &str) -> Result<Option<Vec<Symbol>>> {
    let mut parser = Parser::new();
    let language = tree_sitter_java::LANGUAGE;
    parser.set_language(&language.into()).into_diagnostic()?;

    let tree = parser
        .parse(content, None)
        .ok_or_else(|| miette::miette!("Failed to parse Java content"))?;

    let root = tree.root_node();
    let package = package_name(root, content);
    let mut symbols = Vec::new();
    let mut enclosing: Vec<String> = Vec::new();
    collect_symbols(root, content, &package, &mut enclosing, &mut symbols);
    sort_symbols(&mut symbols);
    Ok(Some(symbols))
}

fn collect_symbols(
    node: Node,
    content: &str,
    package: &str,
    enclosing: &mut Vec<String>,
    symbols: &mut Vec<Symbol>,
) {
    let kind = node.kind();
    match kind {
        "class_declaration" | "record_declaration" => {
            push_named_type(
                node,
                content,
                package,
                enclosing,
                symbols,
                SymbolKind::Class,
            );
            return;
        }
        "interface_declaration" | "annotation_type_declaration" => {
            push_named_type(
                node,
                content,
                package,
                enclosing,
                symbols,
                SymbolKind::Interface,
            );
            return;
        }
        "enum_declaration" => {
            push_named_type(node, content, package, enclosing, symbols, SymbolKind::Enum);
            return;
        }
        "enum_constant" => {
            let name = field_text(node, "name", content);
            if !name.is_empty() {
                push_symbol(
                    node,
                    package,
                    enclosing,
                    &name,
                    SymbolKind::Constant,
                    true,
                    symbols,
                );
            }
            walk_children_skipping_bodies(node, content, package, enclosing, symbols);
            return;
        }
        "method_declaration" => {
            let name = field_text(node, "name", content);
            if !name.is_empty() {
                push_symbol(
                    node,
                    package,
                    enclosing,
                    &name,
                    SymbolKind::Method,
                    is_public_declaration(node, kind),
                    symbols,
                );
            }
        }
        "constructor_declaration" | "compact_constructor_declaration" => {
            push_symbol(
                node,
                package,
                enclosing,
                "<init>",
                SymbolKind::Method,
                is_public_declaration(node, kind),
                symbols,
            );
        }
        "field_declaration" => {
            push_declarators(
                node,
                content,
                package,
                enclosing,
                SymbolKind::Variable,
                is_public_declaration(node, kind),
                symbols,
            );
        }
        "constant_declaration" => {
            push_declarators(
                node,
                content,
                package,
                enclosing,
                SymbolKind::Constant,
                is_public_declaration(node, kind),
                symbols,
            );
        }
        _ => {}
    }

    walk_children_skipping_bodies(node, content, package, enclosing, symbols);
}

fn push_named_type(
    node: Node,
    content: &str,
    package: &str,
    enclosing: &mut Vec<String>,
    symbols: &mut Vec<Symbol>,
    kind: SymbolKind,
) {
    let name = field_text(node, "name", content);
    if name.is_empty() {
        walk_children_skipping_bodies(node, content, package, enclosing, symbols);
        return;
    }
    push_symbol(
        node,
        package,
        enclosing,
        &name,
        kind,
        is_public_declaration(node, node.kind()),
        symbols,
    );
    enclosing.push(name);
    walk_children_skipping_bodies(node, content, package, enclosing, symbols);
    enclosing.pop();
}

fn walk_children_skipping_bodies(
    node: Node,
    content: &str,
    package: &str,
    enclosing: &mut Vec<String>,
    symbols: &mut Vec<Symbol>,
) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if skip_anonymous_or_enum_constant_body(child) {
            continue;
        }
        collect_symbols(child, content, package, enclosing, symbols);
    }
}

fn push_declarators(
    node: Node,
    content: &str,
    package: &str,
    enclosing: &[String],
    kind: SymbolKind,
    is_public: bool,
    symbols: &mut Vec<Symbol>,
) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() != "variable_declarator" {
            continue;
        }
        let name = field_text(child, "name", content);
        if name.is_empty() {
            continue;
        }
        push_symbol(
            child,
            package,
            enclosing,
            &name,
            kind.clone(),
            is_public,
            symbols,
        );
    }
}

fn push_symbol(
    span: Node,
    package: &str,
    enclosing: &[String],
    name: &str,
    kind: SymbolKind,
    is_public: bool,
    symbols: &mut Vec<Symbol>,
) {
    let qualified_name = join_qualified(package, enclosing, name);
    symbols.push(Symbol {
        name: name.to_string(),
        kind,
        is_public,
        cognitive_complexity: None,
        cyclomatic_complexity: None,
        line_start: Some(span.start_position().row as i32 + 1),
        line_end: Some(span.end_position().row as i32 + 1),
        qualified_name: Some(qualified_name),
        byte_start: Some(span.start_byte() as i32),
        byte_end: Some(span.end_byte() as i32),
        entrypoint_kind: None,
        metadata: std::collections::BTreeMap::new(),
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::languages::java::common::is_type_kind;
    use crate::index::symbols::SymbolKind;

    const USER: &str = r#"
package com.example;
public class User {
  public void create(boolean a, boolean b) {
    if (a && b) {
      save();
    }
  }
  private void save() {}
}
"#;

    #[test]
    fn extracts_class_and_methods_with_package_qn() {
        let symbols = extract_symbols(USER).unwrap().unwrap();
        let class = symbols
            .iter()
            .find(|s| s.name == "User" && s.kind == SymbolKind::Class)
            .expect("User class");
        assert_eq!(class.qualified_name.as_deref(), Some("com.example.User"));
        assert!(class.is_public);

        let create = symbols
            .iter()
            .find(|s| s.name == "create" && s.kind == SymbolKind::Method)
            .expect("create");
        assert_eq!(
            create.qualified_name.as_deref(),
            Some("com.example.User.create")
        );
        assert!(create.is_public);

        let save = symbols
            .iter()
            .find(|s| s.name == "save" && s.kind == SymbolKind::Method)
            .expect("save");
        assert_eq!(
            save.qualified_name.as_deref(),
            Some("com.example.User.save")
        );
        assert!(!save.is_public);
    }

    #[test]
    fn constructor_name_is_init_not_class_identifier() {
        let content = r#"
package com.example;
public class User {
  public User(int id) {}
  public User() {}
}
"#;
        let symbols = extract_symbols(content).unwrap().unwrap();
        let ctors: Vec<_> = symbols
            .iter()
            .filter(|s| s.name == "<init>" && s.kind == SymbolKind::Method)
            .collect();
        assert_eq!(ctors.len(), 2, "both constructors emit before upsert");
        assert!(
            ctors
                .iter()
                .all(|s| s.qualified_name.as_deref() == Some("com.example.User.<init>"))
        );
        assert!(
            symbols
                .iter()
                .any(|s| s.name == "User" && s.kind == SymbolKind::Class)
        );
        assert!(
            symbols
                .iter()
                .filter(|s| s.name == "User" && s.kind == SymbolKind::Method)
                .count()
                == 0,
            "constructor must not use the class identifier as Symbol.name"
        );
    }

    #[test]
    fn overload_inventory_sort_is_stable_by_line() {
        let content = r#"
package com.example;
public class Store {
  public void save() {}
  public void save(int code) {}
}
"#;
        let symbols = extract_symbols(content).unwrap().unwrap();
        let saves: Vec<_> = symbols
            .iter()
            .filter(|s| s.name == "save" && s.kind == SymbolKind::Method)
            .collect();
        assert_eq!(saves.len(), 2);
        assert!(saves[0].line_start <= saves[1].line_start);
        assert_eq!(saves[0].qualified_name, saves[1].qualified_name);
    }

    #[test]
    fn interface_method_is_implicitly_public() {
        let content = r#"
package com.example;
public interface Worker {
  void execute();
  int TIMEOUT = 10;
}
"#;
        let symbols = extract_symbols(content).unwrap().unwrap();
        let execute = symbols
            .iter()
            .find(|s| s.name == "execute" && s.kind == SymbolKind::Method)
            .expect("execute");
        assert!(execute.is_public);
        let timeout = symbols
            .iter()
            .find(|s| s.name == "TIMEOUT" && s.kind == SymbolKind::Constant)
            .expect("TIMEOUT");
        assert!(timeout.is_public);
    }

    #[test]
    fn interface_private_method_is_not_public() {
        let content = r#"
package com.example;
public interface Service {
  private void helper() {}
  void execute();
}
"#;
        let symbols = extract_symbols(content).unwrap().unwrap();
        let helper = symbols
            .iter()
            .find(|s| s.name == "helper" && s.kind == SymbolKind::Method)
            .expect("helper");
        assert!(
            !helper.is_public,
            "Java 9+ private interface methods are not public; helper={helper:?}"
        );
        let execute = symbols
            .iter()
            .find(|s| s.name == "execute" && s.kind == SymbolKind::Method)
            .expect("execute");
        assert!(execute.is_public);
    }

    #[test]
    fn anonymous_class_body_members_are_omitted() {
        let content = r#"
package com.example;
public class Host {
  void start() {
    Runnable r = new Runnable() {
      public void run() {}
    };
  }
}
"#;
        let symbols = extract_symbols(content).unwrap().unwrap();
        assert!(
            symbols
                .iter()
                .all(|s| s.name != "run" && s.name != "Runnable"),
            "anonymous body members stay out of the inventory; symbols={symbols:?}"
        );
        assert!(
            symbols
                .iter()
                .any(|s| s.name == "Host" && s.kind == SymbolKind::Class)
        );
        assert!(
            symbols
                .iter()
                .any(|s| s.name == "start" && s.kind == SymbolKind::Method)
        );
    }

    #[test]
    fn annotation_type_elements_are_omitted() {
        let content = r#"
package com.example;
public @interface Marker {
  String value();
  int TIMEOUT = 10;
}
"#;
        let symbols = extract_symbols(content).unwrap().unwrap();
        assert!(
            symbols
                .iter()
                .any(|s| s.name == "Marker" && s.kind == SymbolKind::Interface)
        );
        assert!(
            symbols.iter().all(|s| s.name != "value"),
            "annotation type elements are omitted in v1; symbols={symbols:?}"
        );
        assert!(
            symbols
                .iter()
                .any(|s| s.name == "TIMEOUT" && s.kind == SymbolKind::Constant)
        );
    }

    #[test]
    fn one_symbol_per_declarator_and_enum_constant_body_skipped() {
        let content = r#"
package com.example;
public enum Color {
  RED {
    void extra() {}
  },
  BLUE;
  public int x, y;
}
"#;
        let symbols = extract_symbols(content).unwrap().unwrap();
        assert!(
            symbols
                .iter()
                .any(|s| s.name == "RED" && s.kind == SymbolKind::Constant && s.is_public)
        );
        assert!(symbols.iter().all(|s| s.name != "extra"));
        assert!(
            symbols
                .iter()
                .filter(|s| s.kind == SymbolKind::Variable && (s.name == "x" || s.name == "y"))
                .count()
                == 2
        );
    }

    #[test]
    fn class_member_public_vs_package_private() {
        let content = r#"
package com.example;
public class User {
  public int id;
  int count;
  public void open() {}
  void close() {}
}
"#;
        let symbols = extract_symbols(content).unwrap().unwrap();
        let id = symbols
            .iter()
            .find(|s| s.name == "id" && s.kind == SymbolKind::Variable)
            .expect("id");
        assert!(id.is_public);
        let count = symbols
            .iter()
            .find(|s| s.name == "count" && s.kind == SymbolKind::Variable)
            .expect("count");
        assert!(!count.is_public);
        let open = symbols
            .iter()
            .find(|s| s.name == "open" && s.kind == SymbolKind::Method)
            .expect("open");
        assert!(open.is_public);
        let close = symbols
            .iter()
            .find(|s| s.name == "close" && s.kind == SymbolKind::Method)
            .expect("close");
        assert!(!close.is_public);
    }

    #[test]
    fn compact_constructor_name_is_init() {
        let content = r#"
package com.example;
public record Point(int x, int y) {
  public Point {}
}
"#;
        let symbols = extract_symbols(content).unwrap().unwrap();
        assert!(
            symbols
                .iter()
                .any(|s| s.name == "<init>" && s.kind == SymbolKind::Method)
        );
        assert!(
            symbols
                .iter()
                .filter(|s| s.name == "Point" && s.kind == SymbolKind::Method)
                .count()
                == 0
        );
    }

    #[test]
    fn record_components_are_not_variables() {
        let content = r#"
package com.example;
public record Point(int x, int y) {}
"#;
        let symbols = extract_symbols(content).unwrap().unwrap();
        assert!(
            symbols
                .iter()
                .any(|s| s.name == "Point" && s.kind == SymbolKind::Class)
        );
        assert!(symbols.iter().all(|s| s.name != "x" && s.name != "y"));
    }

    #[test]
    fn empty_parse_still_some() {
        let symbols = extract_symbols("package com.example;\n").unwrap();
        assert!(symbols.is_some());
        assert!(
            symbols
                .unwrap()
                .iter()
                .all(|s| !is_type_kind(s.kind.as_str()) || s.name != "package")
        );
    }
}
