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
    type_qn: String,
    init_qn: String,
    ctor_count: usize,
}

enum CreationTypeKey {
    Simple(String),
    Scoped(Vec<String>),
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
    collect_edges(
        path,
        root,
        content,
        &package,
        &method_counts,
        &types,
        &mut edges,
    );

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
                type_qn,
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
    package: &str,
    method_counts: &HashMap<String, usize>,
    types: &[TypeInfo],
    edges: &mut Vec<CallEdge>,
) {
    match node.kind() {
        "method_invocation" => {
            if let Some(edge) = method_invocation_edge(path, node, content, package, method_counts)
            {
                edges.push(edge);
            }
        }
        "object_creation_expression" => {
            if let Some(edge) = object_creation_edge(path, node, content, package, types) {
                edges.push(edge);
            }
        }
        "explicit_constructor_invocation" => {
            if let Some(edge) = explicit_ctor_edge(path, node, content, package) {
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
        collect_edges(path, child, content, package, method_counts, types, edges);
    }
}

fn method_invocation_edge(
    path: &Path,
    node: Node,
    content: &str,
    package: &str,
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
        caller_name: enclosing_caller(node, content, package),
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
    match method_counts.get(name).copied().unwrap_or(0) {
        1 => (name.to_string(), ResolutionStatus::Resolved),
        0 => (format!("{name}.external"), ResolutionStatus::Unresolved),
        _ => (format!("{name}.overload"), ResolutionStatus::Unresolved),
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

fn creation_type_key(node: Node, content: &str) -> Option<CreationTypeKey> {
    if node.kind() == "generic_type" {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.kind() == "type_arguments" {
                continue;
            }
            if child.kind() == "scoped_type_identifier" {
                return creation_type_key(child, content);
            }
            break;
        }
    }
    match node.kind() {
        "type_identifier" | "identifier" | "generic_type" => {
            let name = simple_type_name(node, content);
            if name.is_empty() {
                None
            } else {
                Some(CreationTypeKey::Simple(name))
            }
        }
        "scoped_type_identifier" => {
            let segments = scoped_type_segments(node, content);
            if segments.is_empty() {
                None
            } else {
                Some(CreationTypeKey::Scoped(segments))
            }
        }
        _ => None,
    }
}

fn scoped_type_segments(node: Node, content: &str) -> Vec<String> {
    let mut segments = Vec::new();
    collect_type_segments(node, content, &mut segments);
    segments
}

fn collect_type_segments(node: Node, content: &str, out: &mut Vec<String>) {
    let kind = node.kind();
    if kind == "type_arguments" || kind.contains("annotation") {
        return;
    }
    if matches!(kind, "type_identifier" | "identifier") {
        let name = node_text(node, content);
        if !name.is_empty() {
            out.push(name);
        }
        return;
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_type_segments(child, content, out);
    }
}

fn type_qn_has_segment_suffix(type_qn: &str, key: &[String]) -> bool {
    if key.is_empty() {
        return false;
    }
    let qn_segs: Vec<&str> = type_qn.split('.').filter(|seg| !seg.is_empty()).collect();
    if key.len() > qn_segs.len() {
        return false;
    }
    let start = qn_segs.len() - key.len();
    qn_segs[start..]
        .iter()
        .zip(key.iter())
        .all(|(qn_seg, key_seg)| *qn_seg == key_seg.as_str())
}

fn creation_last_segment(key: &CreationTypeKey) -> String {
    match key {
        CreationTypeKey::Simple(name) => name.clone(),
        CreationTypeKey::Scoped(segments) => segments.last().cloned().unwrap_or_default(),
    }
}

fn object_creation_edge(
    path: &Path,
    node: Node,
    content: &str,
    package: &str,
    types: &[TypeInfo],
) -> Option<CallEdge> {
    let type_node = creation_type_node(node)?;
    let key = creation_type_key(type_node, content)?;
    let last = creation_last_segment(&key);
    if last.is_empty() {
        return None;
    }
    let has_body = creation_has_class_body(node);
    let matches: Vec<&TypeInfo> = match &key {
        CreationTypeKey::Simple(name) => types.iter().filter(|t| t.simple_name == *name).collect(),
        CreationTypeKey::Scoped(segments) => types
            .iter()
            .filter(|t| type_qn_has_segment_suffix(&t.type_qn, segments))
            .collect(),
    };
    let (callee_name, status) = if !has_body && matches.len() == 1 && matches[0].ctor_count == 1 {
        (matches[0].init_qn.clone(), ResolutionStatus::Resolved)
    } else {
        (format!("new.{last}"), ResolutionStatus::Unresolved)
    };
    Some(CallEdge {
        caller_name: enclosing_caller(node, content, package),
        caller_file: path.to_path_buf(),
        callee_name,
        callee_file: None,
        call_kind: CallKind::Direct,
        resolution_status: status,
        confidence: CallKind::Direct.default_confidence(),
        evidence: format!("new:{last}"),
    })
}

fn explicit_ctor_edge(path: &Path, node: Node, content: &str, package: &str) -> Option<CallEdge> {
    let ctor = node.child_by_field_name("constructor")?;
    let which = match ctor.kind() {
        "this" => "this",
        "super" => "super",
        _ => return None,
    };
    Some(CallEdge {
        caller_name: enclosing_caller(node, content, package),
        caller_file: path.to_path_buf(),
        callee_name: format!("ctor.{which}"),
        callee_file: None,
        call_kind: CallKind::Direct,
        resolution_status: ResolutionStatus::Unresolved,
        confidence: CallKind::Direct.default_confidence(),
        evidence: format!("ctor:{which}"),
    })
}

fn enclosing_caller(node: Node, content: &str, package: &str) -> String {
    let mut name: Option<String> = None;
    let mut types_inner_first: Vec<String> = Vec::new();
    let mut current = node.parent();
    while let Some(parent) = current {
        match parent.kind() {
            "method_declaration" if name.is_none() => {
                let method = field_text(parent, "name", content);
                if !method.is_empty() {
                    name = Some(method);
                }
            }
            "constructor_declaration" | "compact_constructor_declaration" if name.is_none() => {
                name = Some("<init>".to_string());
            }
            kind if is_type_kind(kind) => {
                let type_name = field_text(parent, "name", content);
                if !type_name.is_empty() {
                    types_inner_first.push(type_name);
                }
            }
            _ => {}
        }
        current = parent.parent();
    }
    let Some(name) = name else {
        return "<file>".to_string();
    };
    if types_inner_first.is_empty() {
        return "<file>".to_string();
    }
    types_inner_first.reverse();
    join_qualified(package, &types_inner_first, &name)
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
            .find(|e| e.callee_name == "save" && e.caller_name == "com.example.User.create")
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
        assert_eq!(overload.caller_name, "com.example.Store.callOn");
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
            edges.iter().any(|e| {
                e.caller_name == "com.example.Host.start"
                    && e.callee_name == "new.Runnable"
                    && e.resolution_status == ResolutionStatus::Unresolved
            }),
            "anonymous new still records unresolved new.Runnable; edges={edges:?}"
        );
        assert!(
            edges
                .iter()
                .all(|e| e.caller_name != "run" && !e.caller_name.ends_with(".run")),
            "anonymous / enum-constant run() bodies must not emit calls; edges={edges:?}"
        );
        assert!(
            edges.iter().all(|e| {
                e.callee_name != "save"
                    && e.callee_name != "save.overload"
                    && e.callee_name != "save.external"
            }),
            "skipped bodies must not emit save; edges={edges:?}"
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
        assert_eq!(edge.caller_name, "com.example.User.<init>");
    }

    #[test]
    fn distinct_constructor_callers_keep_qualified_init() {
        let content = r#"
package com.example;
class A { A() { aOnly(); } void aOnly() {} }
class B { B() { bOnly(); } void bOnly() {} }
"#;
        let edges = extract_calls(Path::new("Pair.java"), content, &[]).unwrap();
        assert!(
            edges.iter().any(|e| {
                e.caller_name == "com.example.A.<init>"
                    && e.callee_name == "aOnly"
                    && e.resolution_status == ResolutionStatus::Resolved
            }),
            "A.<init> calls aOnly; edges={edges:?}"
        );
        assert!(
            edges.iter().any(|e| {
                e.caller_name == "com.example.B.<init>"
                    && e.callee_name == "bOnly"
                    && e.resolution_status == ResolutionStatus::Resolved
            }),
            "B.<init> calls bOnly; edges={edges:?}"
        );
    }

    #[test]
    fn initializer_without_method_reports_file() {
        let content = r#"
package com.example;
class Host {
  { save(); }
  void save() {}
}
"#;
        let edges = extract_calls(Path::new("Host.java"), content, &[]).unwrap();
        assert!(
            edges
                .iter()
                .any(|e| e.caller_name == "<file>" && e.callee_name == "save"),
            "instance initializer has no enclosing method; edges={edges:?}"
        );
    }

    #[test]
    fn explicit_this_and_super_use_qualified_init_caller() {
        let content = r#"
package com.example;
class Base { Base(int x) {} }
class Child extends Base {
  Child() { this(1); }
  Child(int x) { super(x); }
}
"#;
        let edges = extract_calls(Path::new("Child.java"), content, &[]).unwrap();
        assert!(
            edges.iter().any(|e| {
                e.caller_name == "com.example.Child.<init>" && e.callee_name == "ctor.this"
            }),
            "this() caller is the enclosing <init>; edges={edges:?}"
        );
        assert!(
            edges.iter().any(|e| {
                e.caller_name == "com.example.Child.<init>" && e.callee_name == "ctor.super"
            }),
            "super() caller is the enclosing <init>; edges={edges:?}"
        );
    }

    #[test]
    fn zero_same_file_defs_use_external_not_overload() {
        let content = r#"
package com.example;
class Uses {
  void m() { max(); this.max(); }
}
"#;
        let edges = extract_calls(Path::new("Uses.java"), content, &[]).unwrap();
        let external: Vec<_> = edges
            .iter()
            .filter(|e| {
                e.callee_name == "max.external"
                    && e.resolution_status == ResolutionStatus::Unresolved
                    && e.evidence == "call_expr:max()"
            })
            .collect();
        assert_eq!(
            external.len(),
            2,
            "bare max() and this.max() are both external; edges={edges:?}"
        );
        assert!(
            edges.iter().all(|e| e.callee_name != "max.overload"),
            "count 0 must not be overload; edges={edges:?}"
        );
    }

    #[test]
    fn scoped_new_matches_type_qn_segment_suffix() {
        let path = Path::new("T.java");
        let other_box = r#"
package com.example;
class Box { Box() {} }
class Uses { void m() { new com.other.Box(); } }
"#;
        let edges = extract_calls(path, other_box, &[]).unwrap();
        assert!(
            edges.iter().any(|e| {
                e.callee_name == "new.Box" && e.resolution_status == ResolutionStatus::Unresolved
            }),
            "com.other.Box must not bind local Box; edges={edges:?}"
        );
        assert!(
            edges
                .iter()
                .all(|e| e.callee_name != "com.example.Box.<init>"),
            "local Box.<init> must not win; edges={edges:?}"
        );

        let both = r#"
package com.example;
class Host { class Inner { Inner() {} } }
class Other { class Inner { Inner() {} } }
class Uses { void m() { new Other.Inner(); } }
"#;
        let edges = extract_calls(path, both, &[]).unwrap();
        assert!(
            edges.iter().any(|e| {
                e.callee_name == "com.example.Other.Inner.<init>"
                    && e.resolution_status == ResolutionStatus::Resolved
            }),
            "Other.Inner binds the suffix type; edges={edges:?}"
        );

        let host_only = r#"
package com.example;
class Host { class Inner { Inner() {} } }
class Uses { void m() { new Other.Inner(); } }
"#;
        let edges = extract_calls(path, host_only, &[]).unwrap();
        assert!(
            edges.iter().any(|e| {
                e.callee_name == "new.Inner" && e.resolution_status == ResolutionStatus::Unresolved
            }),
            "Other.Inner with only Host.Inner stays unresolved; edges={edges:?}"
        );

        let not_other = r#"
package com.example;
class NotOther { class Inner { Inner() {} } }
class Uses { void m() { new Other.Inner(); } }
"#;
        let edges = extract_calls(path, not_other, &[]).unwrap();
        assert!(
            edges.iter().any(|e| {
                e.callee_name == "new.Inner" && e.resolution_status == ResolutionStatus::Unresolved
            }),
            "NotOther.Inner must not match Other.Inner; edges={edges:?}"
        );
        assert!(
            edges
                .iter()
                .all(|e| e.callee_name != "com.example.NotOther.Inner.<init>"),
            "segment suffix must not use ends_with; edges={edges:?}"
        );

        let anon = r#"
package com.example;
class Box { Box() {} }
class Uses { void m() { new com.example.Box() { }; } }
"#;
        let edges = extract_calls(path, anon, &[]).unwrap();
        assert!(
            edges.iter().any(|e| {
                e.callee_name == "new.Box" && e.resolution_status == ResolutionStatus::Unresolved
            }),
            "direct class_body keeps new unresolved; edges={edges:?}"
        );

        let generic_inner = r#"
package com.example;
class Outer { class Inner { Inner() {} } }
class Uses { void m() { new Outer<String>.Inner(); } }
"#;
        let edges = extract_calls(path, generic_inner, &[]).unwrap();
        assert!(
            edges.iter().any(|e| {
                e.callee_name == "com.example.Outer.Inner.<init>"
                    && e.resolution_status == ResolutionStatus::Resolved
            }),
            "Outer<String>.Inner skips the type argument; edges={edges:?}"
        );
        assert!(
            edges.iter().all(|e| e.callee_name != "new.String"),
            "String is not a segment; edges={edges:?}"
        );
    }
}
