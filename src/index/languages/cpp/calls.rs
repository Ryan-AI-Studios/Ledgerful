use super::common::{find_enclosing_function, node_text, truncate_evidence, unwrap_callee_name};
use crate::index::call_graph::{CallEdge, CallKind, ResolutionStatus};
use crate::index::symbols::{Symbol, SymbolKind};
use miette::{IntoDiagnostic, Result};
use std::collections::HashMap;
use std::path::Path;
use tree_sitter::Parser;

pub fn extract_calls(path: &Path, content: &str, symbols: &[Symbol]) -> Result<Vec<CallEdge>> {
    let mut parser = Parser::new();
    let language = tree_sitter_cpp::LANGUAGE;
    parser.set_language(&language.into()).into_diagnostic()?;

    let tree = parser
        .parse(content, None)
        .ok_or_else(|| miette::miette!("Failed to parse C/C++ content"))?;

    // Same-file callable name multiplicity for D5 resolution (Function / Method only).
    // Resolved only when exactly one local definition shares the name; overloads
    // (count > 1) and missing names (count == 0) stay Unresolved — no silent collapse.
    let mut local_callable_counts: HashMap<String, usize> = HashMap::new();
    for symbol in symbols
        .iter()
        .filter(|s| matches!(s.kind, SymbolKind::Function | SymbolKind::Method))
    {
        *local_callable_counts
            .entry(symbol.name.clone())
            .or_insert(0) += 1;
    }

    let mut edges = Vec::new();
    collect_cpp_call_edges(
        path,
        tree.root_node(),
        content,
        &local_callable_counts,
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

fn collect_cpp_call_edges(
    path: &Path,
    node: tree_sitter::Node,
    content: &str,
    local_callable_counts: &HashMap<String, usize>,
    edges: &mut Vec<CallEdge>,
) {
    if node.kind() == "call_expression" {
        let caller_name = find_enclosing_function(node, content);
        if let Some(function_node) = node.child_by_field_name("function") {
            let call_kind = match function_node.kind() {
                "field_expression" => CallKind::MethodCall,
                "identifier" => CallKind::Direct,
                _ => CallKind::Direct,
            };

            if let Some(callee_name) = unwrap_callee_name(function_node, content) {
                // D5: unique same-file name after unwrap → Resolved; overload /
                // zero matches → Unresolved (do not collapse overloads).
                let resolution_status = match local_callable_counts.get(&callee_name).copied() {
                    Some(1) => ResolutionStatus::Resolved,
                    _ => ResolutionStatus::Unresolved,
                };
                let full = truncate_evidence(&node_text(function_node, content), 120);
                let confidence = if resolution_status == ResolutionStatus::Resolved {
                    call_kind.default_confidence()
                } else {
                    CallKind::External.default_confidence()
                };
                edges.push(CallEdge {
                    caller_name,
                    caller_file: path.to_path_buf(),
                    callee_name,
                    callee_file: None,
                    call_kind,
                    resolution_status,
                    confidence,
                    evidence: format!("call_expr:{full}"),
                });
            }
        }
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_cpp_call_edges(path, child, content, local_callable_counts, edges);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::call_graph::{CallKind, ResolutionStatus};
    use crate::index::languages::cpp::extract_symbols;
    use std::path::Path;

    #[test]
    fn same_file_direct_call_resolved() {
        let content = r#"
int helper() { return 1; }
int caller() {
    return helper();
}
"#;
        let symbols = extract_symbols(content).unwrap().unwrap();
        let edges = extract_calls(Path::new("demo.cpp"), content, &symbols).unwrap();
        let local: Vec<_> = edges
            .iter()
            .filter(|e| e.callee_name == "helper" && e.call_kind == CallKind::Direct)
            .collect();
        assert!(!local.is_empty(), "edges={edges:?}");
        assert_eq!(local[0].caller_name, "caller");
        assert_eq!(local[0].resolution_status, ResolutionStatus::Resolved);
    }

    #[test]
    fn external_call_unresolved() {
        let content = r#"
#include <cstdio>
void f() {
    printf("hi");
}
"#;
        let symbols = extract_symbols(content).unwrap().unwrap();
        let edges = extract_calls(Path::new("demo.cpp"), content, &symbols).unwrap();
        let ext: Vec<_> = edges.iter().filter(|e| e.callee_name == "printf").collect();
        assert!(!ext.is_empty(), "edges={edges:?}");
        assert_eq!(ext[0].resolution_status, ResolutionStatus::Unresolved);
    }

    #[test]
    fn member_call_unwrap_field_expression() {
        // Single in-class definition only — decl+def of the same name would be
        // multiplicity > 1 and correctly Unresolved under D5 overload honesty.
        let content = r#"
struct S {
    void work() {}
};
void caller(S* s) {
    s->work();
}
"#;
        let symbols = extract_symbols(content).unwrap().unwrap();
        let edges = extract_calls(Path::new("demo.cpp"), content, &symbols).unwrap();
        let method: Vec<_> = edges
            .iter()
            .filter(|e| e.callee_name == "work" && e.call_kind == CallKind::MethodCall)
            .collect();
        assert!(
            !method.is_empty(),
            "expected field_expression unwrap to work; edges={edges:?}"
        );
        assert_eq!(method[0].resolution_status, ResolutionStatus::Resolved);
    }

    #[test]
    fn template_function_unwrap() {
        let content = r#"
template<typename T>
T id(T x) { return x; }

int caller() {
    return id<int>(42);
}
"#;
        let symbols = extract_symbols(content).unwrap().unwrap();
        let edges = extract_calls(Path::new("demo.cpp"), content, &symbols).unwrap();
        // template call may appear as template_function; unwrap leaf "id"
        let hit: Vec<_> = edges.iter().filter(|e| e.callee_name == "id").collect();
        assert!(
            !hit.is_empty(),
            "expected template_function unwrap to id; edges={edges:?}"
        );
    }

    #[test]
    fn overloaded_same_name_call_is_unresolved() {
        // D5 honesty: two same-file definitions of `foo` must not collapse to
        // a single HashSet entry and falsely resolve the call.
        let content = r#"
int foo(int x) { return x; }
int foo(double x) { return static_cast<int>(x); }
int caller() {
    return foo(1);
}
"#;
        let symbols = extract_symbols(content).unwrap().unwrap();
        let foo_defs: Vec<_> = symbols
            .iter()
            .filter(|s| {
                s.name == "foo"
                    && matches!(
                        s.kind,
                        crate::index::symbols::SymbolKind::Function
                            | crate::index::symbols::SymbolKind::Method
                    )
            })
            .collect();
        assert!(
            foo_defs.len() >= 2,
            "fixture needs ≥2 foo callables; symbols={symbols:?}"
        );

        let edges = extract_calls(Path::new("demo.cpp"), content, &symbols).unwrap();
        let foo_calls: Vec<_> = edges.iter().filter(|e| e.callee_name == "foo").collect();
        assert!(
            !foo_calls.is_empty(),
            "expected call edge for foo; edges={edges:?}"
        );
        for edge in &foo_calls {
            assert_eq!(
                edge.resolution_status,
                ResolutionStatus::Unresolved,
                "overload must stay Unresolved; edge={edge:?}"
            );
        }
    }
}
