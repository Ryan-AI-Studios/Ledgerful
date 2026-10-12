use super::common::{
    field_text, is_type_kind, join_qualified, node_text, package_name, simple_type_name,
    skip_anonymous_or_enum_constant_body,
};
use crate::index::call_graph::{CallEdge, CallKind, ResolutionStatus};
use crate::index::symbols::Symbol;
use miette::{IntoDiagnostic, Result};
use std::collections::HashMap;
use std::path::Path;
use tree_sitter::{Node, Parser};

struct TypeInfo {
    simple_name: String,
    init_qn: String,
    ctor_count: usize,
}

pub fn extract_calls(path: &Path, content: &str, _symbols: &[Symbol]) -> Result<Vec<CallEdge>> {
    let mut parser = Parser::new();
    let language = tree_sitter_java::LANGUAGE;
    parser.set_language(&language.into()).into_diagnostic()?;

    let tree = parser
        .parse(content, None)
        .ok_or_else(|| miette::miette!("Failed to parse Java content"))?;

    let root = tree.root_node();
    let package = package_name(root, content);
    let mut method_counts: HashMap<String, usize> = HashMap::new();
    let mut types: Vec<TypeInfo> = Vec::new();
    let mut enclosing: Vec<String> = Vec::new();
    collect_inventory(
        root,
        content,
        &package,
        &mut enclosing,
        &mut method_counts,
        &mut types,
    );

    let mut edges = Vec::new();
    collect_edges(path, root, content, &method_counts, &types, &mut edges);

    edges.sort_by(|a, b| {
        a.caller_name
            .cmp(&b.caller_name)
            .then(a.callee_name.cmp(&b.callee_name))
            .then(a.evidence.cmp(&b.evidence))
    });
    Ok(edges)
}

fn collect_inventory(
    node: Node,
    content: &str,
    package: &str,
    enclosing: &mut Vec<String>,
    method_counts: &mut HashMap<String, usize>,
    types: &mut Vec<TypeInfo>,
) {
    let kind = node.kind();
    if is_type_kind(kind) {
        let name = field_text(node, "name", content);
        if !name.is_empty() {
            let type_qn = join_qualified(package, enclosing, &name);
            let init_qn = if type_qn.is_empty() {
                "<init>".to_string()
            } else {
                format!("{type_qn}.<init>")
            };
            types.push(TypeInfo {
                simple_name: name.clone(),
                init_qn,
                ctor_count: count_constructors(node),
            });
            enclosing.push(name);
            walk_inventory_children(node, content, package, enclosing, method_counts, types);
            enclosing.pop();
            return;
        }
    }

    if kind == "method_declaration" {
        let name = field_text(node, "name", content);
        if !name.is_empty() {
            *method_counts.entry(name).or_insert(0) += 1;
        }
    }

    walk_inventory_children(node, content, package, enclosing, method_counts, types);
}

fn walk_inventory_children(
    node: Node,
    content: &str,
    package: &str,
    enclosing: &mut Vec<String>,
    method_counts: &mut HashMap<String, usize>,
    types: &mut Vec<TypeInfo>,
) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if skip_anonymous_or_enum_constant_body(child) {
            continue;
        }
        collect_inventory(child, content, package, enclosing, method_counts, types);
    }
}

fn count_constructors(type_node: Node) -> usize {
    let Some(body) = type_node.child_by_field_name("body") else {
        return 0;
    };
    let mut count = 0;
    let mut cursor = body.walk();
    for child in body.children(&mut cursor) {
        if matches!(
            child.kind(),
            "constructor_declaration" | "compact_constructor_declaration"
        ) {
            count += 1;
        }
    }
    count
}

fn collect_edges(
    path: &Path,
    node: Node,
    content: &str,
    method_counts: &HashMap<String, usize>,
    types: &[TypeInfo],
    edges: &mut Vec<CallEdge>,
) {
    match node.kind() {
        "method_invocation" => {
            if let Some(edge) = method_invocation_edge(path, node, content, method_counts) {
                edges.push(edge);
            }
        }
        "object_creation_expression" => {
            if let Some(edge) = object_creation_edge(path, node, content, types) {
                edges.push(edge);
            }
        }
        "explicit_constructor_invocation" => {
            if let Some(edge) = explicit_ctor_edge(path, node, content) {
                edges.push(edge);
            }
        }
        _ => {}
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if skip_anonymous_or_enum_constant_body(child) {
            continue;
        }
        collect_edges(path, child, content, method_counts, types, edges);
    }
}

fn method_invocation_edge(
    path: &Path,
    node: Node,
    content: &str,
    method_counts: &HashMap<String, usize>,
) -> Option<CallEdge> {
    let name = field_text(node, "name", content);
    if name.is_empty() {
        return None;
    }
    let object = node.child_by_field_name("object");
    let (callee_name, status) = match object {
        None => unique_or_overload(&name, method_counts),
        Some(obj) if obj.kind() == "this" => unique_or_overload(&name, method_counts),
        Some(obj) => {
            let object_text = node_text(obj, content);
            if object_text.is_empty() {
                (format!("recv.{name}"), ResolutionStatus::Unresolved)
            } else {
                (
                    format!("{object_text}.{name}"),
                    ResolutionStatus::Unresolved,
                )
            }
        }
    };
    Some(CallEdge {
        caller_name: enclosing_caller(node, content),
        caller_file: path.to_path_buf(),
        callee_name,
        callee_file: None,
        call_kind: CallKind::Direct,
        resolution_status: status,
        confidence: CallKind::Direct.default_confidence(),
        evidence: format!("call_expr:{name}()"),
    })
}

fn unique_or_overload(
    name: &str,
    method_counts: &HashMap<String, usize>,
) -> (String, ResolutionStatus) {
    if method_counts.get(name).copied().unwrap_or(0) == 1 {
        (name.to_string(), ResolutionStatus::Resolved)
    } else {
        (format!("{name}.overload"), ResolutionStatus::Unresolved)
    }
}

fn creation_type_node(node: Node) -> Option<Node> {
    if let Some(ty) = node.child_by_field_name("type") {
        return Some(ty);
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if let Some(ty) = child.child_by_field_name("type") {
            return Some(ty);
        }
    }
    None
}

fn creation_has_class_body(node: Node) -> bool {
    let mut cursor = node.walk();
    node.children(&mut cursor).any(|c| c.kind() == "class_body")
}

fn object_creation_edge(
    path: &Path,
    node: Node,
    content: &str,
    types: &[TypeInfo],
) -> Option<CallEdge> {
    let type_node = creation_type_node(node)?;
    let simple = simple_type_name(type_node, content);
    if simple.is_empty() {
        return None;
    }
    let has_body = creation_has_class_body(node);
    let matches: Vec<&TypeInfo> = types.iter().filter(|t| t.simple_name == simple).collect();
    let (callee_name, status) = if !has_body && matches.len() == 1 && matches[0].ctor_count == 1 {
        (matches[0].init_qn.clone(), ResolutionStatus::Resolved)
    } else {
        (format!("new.{simple}"), ResolutionStatus::Unresolved)
    };
    Some(CallEdge {
        caller_name: enclosing_caller(node, content),
        caller_file: path.to_path_buf(),
        callee_name,
        callee_file: None,
        call_kind: CallKind::Direct,
        resolution_status: status,
        confidence: CallKind::Direct.default_confidence(),
        evidence: format!("new:{simple}"),
    })
}

fn explicit_ctor_edge(path: &Path, node: Node, content: &str) -> Option<CallEdge> {
    let ctor = node.child_by_field_name("constructor")?;
    let which = match ctor.kind() {
        "this" => "this",
        "super" => "super",
        _ => return None,
    };
    Some(CallEdge {
        caller_name: enclosing_caller(node, content),
        caller_file: path.to_path_buf(),
        callee_name: format!("ctor.{which}"),
        callee_file: None,
        call_kind: CallKind::Direct,
        resolution_status: ResolutionStatus::Unresolved,
        confidence: CallKind::Direct.default_confidence(),
        evidence: format!("ctor:{which}"),
    })
}

fn enclosing_caller(node: Node, content: &str) -> String {
    let mut current = node.parent();
    while let Some(parent) = current {
        match parent.kind() {
            "method_declaration" => {
                let name = field_text(parent, "name", content);
                if !name.is_empty() {
                    return name;
                }
            }
            "constructor_declaration" | "compact_constructor_declaration" => {
                return "<init>".to_string();
            }
            _ => {}
        }
        current = parent.parent();
    }
    "<file>".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::languages::java::extract_symbols;
    use std::path::Path;

    #[test]
    fn unique_bare_save_resolved_receiver_unresolved() {
        let content = r#"
package com.example;
public class User {
  public void create() { save(); }
  private void save() {}
  public void other(User obj) { obj.save(); }
}
"#;
        let symbols = extract_symbols(content).unwrap().unwrap();
        let edges = extract_calls(Path::new("User.java"), content, &symbols).unwrap();
        let bare = edges
            .iter()
            .find(|e| e.callee_name == "save" && e.caller_name == "create")
            .expect("bare save");
        assert_eq!(bare.resolution_status, ResolutionStatus::Resolved);
        let recv = edges
            .iter()
            .find(|e| e.callee_name == "obj.save")
            .expect("obj.save");
        assert_eq!(recv.resolution_status, ResolutionStatus::Unresolved);
        assert_eq!(recv.evidence, "call_expr:save()");
    }

    #[test]
    fn overload_uses_dotted_callee() {
        let content = r#"
package com.example;
public class Store {
  public void save() {}
  public void save(int code) {}
  public void callOn() { save(); }
}
"#;
        let symbols = extract_symbols(content).unwrap().unwrap();
        let edges = extract_calls(Path::new("Store.java"), content, &symbols).unwrap();
        let overload = edges
            .iter()
            .find(|e| e.callee_name == "save.overload")
            .expect("overload encoding");
        assert_eq!(overload.resolution_status, ResolutionStatus::Unresolved);
        assert_eq!(overload.caller_name, "callOn");
    }

    #[test]
    fn new_zero_one_two_constructors() {
        let zero = r#"
package com.example;
public class A {}
class Uses { void m() { new A(); } }
"#;
        let one = r#"
package com.example;
public class B { B() {} }
class Uses { void m() { new B(); } }
"#;
        let two = r#"
package com.example;
public class C { C() {} C(int x) {} }
class Uses { void m() { new C(); } }
"#;
        let path = Path::new("T.java");
        let z = extract_calls(path, zero, &[]).unwrap();
        assert!(z.iter().any(
            |e| e.callee_name == "new.A" && e.resolution_status == ResolutionStatus::Unresolved
        ));
        let o = extract_calls(path, one, &[]).unwrap();
        assert!(
            o.iter().any(|e| {
                e.callee_name == "com.example.B.<init>"
                    && e.resolution_status == ResolutionStatus::Resolved
            }),
            "unique ctor; edges={o:?}"
        );
        let t = extract_calls(path, two, &[]).unwrap();
        assert!(t.iter().any(
            |e| e.callee_name == "new.C" && e.resolution_status == ResolutionStatus::Unresolved
        ));
    }

    #[test]
    fn generic_new_uses_constructed_type_not_type_argument() {
        let content = r#"
package com.example;
public class Box { Box() {} }
class Inner { Inner() {} }
class Uses { void m() { new Box<Inner>(); new com.example.Box<Inner>(); } }
"#;
        let edges = extract_calls(Path::new("T.java"), content, &[]).unwrap();
        let box_hits: Vec<_> = edges
            .iter()
            .filter(|e| e.callee_name == "com.example.Box.<init>")
            .collect();
        assert_eq!(
            box_hits.len(),
            2,
            "both generic news resolve to Box.<init>; edges={edges:?}"
        );
        assert!(
            box_hits
                .iter()
                .all(|e| e.resolution_status == ResolutionStatus::Resolved)
        );
        assert!(
            edges.iter().all(
                |e| e.callee_name != "com.example.Inner.<init>" && e.callee_name != "new.Inner"
            ),
            "type argument Inner must not become the constructed type; edges={edges:?}"
        );
    }

    #[test]
    fn anonymous_and_enum_constant_bodies_do_not_emit_calls() {
        let content = r#"
package com.example;
public class Host {
  void run() {}
  void save() {}
  void start() {
    Runnable r = new Runnable() {
      public void run() { save(); }
    };
  }
}
enum Color {
  RED {
    void run() { save(); }
  };
  void save() {}
  void run() {}
}
"#;
        let edges = extract_calls(Path::new("Host.java"), content, &[]).unwrap();
        assert!(
            edges.iter().all(|e| e.caller_name != "run"),
            "anonymous / enum-constant run() bodies must not emit calls; edges={edges:?}"
        );
        assert!(
            edges.iter().any(|e| {
                e.caller_name == "start"
                    && e.callee_name == "new.Runnable"
                    && e.resolution_status == ResolutionStatus::Unresolved
            }),
            "anonymous new still records unresolved new.Runnable; edges={edges:?}"
        );
    }

    #[test]
    fn nested_anonymous_argument_does_not_demote_outer_new() {
        let content = r#"
package com.example;
class Listener { Listener() {} }
class Button { Button() {} }
class Uses {
  void m() {
    new Button(new Listener() { });
  }
}
"#;
        let edges = extract_calls(Path::new("T.java"), content, &[]).unwrap();
        assert!(
            edges.iter().any(|e| {
                e.callee_name == "com.example.Button.<init>"
                    && e.resolution_status == ResolutionStatus::Resolved
            }),
            "unique Button must still resolve when an argument is anonymous; edges={edges:?}"
        );
        assert!(
            edges.iter().any(|e| {
                e.callee_name == "new.Listener"
                    && e.resolution_status == ResolutionStatus::Unresolved
            }),
            "anonymous Listener stays unresolved; edges={edges:?}"
        );
    }

    #[test]
    fn qualifying_anonymous_outer_does_not_demote_inner_new() {
        let content = r#"
package com.example;
class Outer {
  class Inner { Inner() {} }
}
class Uses {
  void m() {
    new Outer() { }.new Inner();
  }
}
"#;
        let edges = extract_calls(Path::new("T.java"), content, &[]).unwrap();
        assert!(
            edges.iter().any(|e| {
                e.callee_name == "com.example.Outer.Inner.<init>"
                    && e.resolution_status == ResolutionStatus::Resolved
            }),
            "unique Inner must resolve after qualifying anonymous Outer; edges={edges:?}"
        );
        assert!(
            edges.iter().any(|e| {
                e.callee_name == "new.Outer" && e.resolution_status == ResolutionStatus::Unresolved
            }),
            "anonymous Outer stays unresolved; edges={edges:?}"
        );
    }

    #[test]
    fn constructor_caller_is_init() {
        let content = r#"
package com.example;
public class User {
  User() { save(); }
  void save() {}
}
"#;
        let edges = extract_calls(Path::new("User.java"), content, &[]).unwrap();
        let edge = edges
            .iter()
            .find(|e| e.callee_name == "save")
            .expect("save");
        assert_eq!(edge.caller_name, "<init>");
    }
}
