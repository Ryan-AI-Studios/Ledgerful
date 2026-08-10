use super::common::{find_enclosing_function, node_text, truncate_evidence, unwrap_callee_name};
use crate::index::call_graph::{CallEdge, CallKind, ResolutionStatus};
use crate::index::symbols::{Symbol, SymbolKind};
use miette::{IntoDiagnostic, Result};
use std::collections::HashSet;
use std::path::Path;
use tree_sitter::Parser;

pub fn extract_calls(path: &Path, content: &str, symbols: &[Symbol]) -> Result<Vec<CallEdge>> {
    let mut parser = Parser::new();
    let language = tree_sitter_cpp::LANGUAGE;
    parser.set_language(&language.into()).into_diagnostic()?;

    let tree = parser
        .parse(content, None)
        .ok_or_else(|| miette::miette!("Failed to parse C/C++ content"))?;

    // Same-file callable names for D5 resolution (Function / Method only).
    let local_callables: HashSet<String> = symbols
        .iter()
        .filter(|s| matches!(s.kind, SymbolKind::Function | SymbolKind::Method))
        .map(|s| s.name.clone())
        .collect();

    let mut edges = Vec::new();
    collect_cpp_call_edges(
        path,
        tree.root_node(),
        content,
        &local_callables,
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
    local_callables: &HashSet<String>,
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
                let resolution_status = if local_callables.contains(&callee_name) {
                    ResolutionStatus::Resolved
                } else {
                    ResolutionStatus::Unresolved
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
        collect_cpp_call_edges(path, child, content, local_callables, edges);
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
        let content = r#"
struct S {
    void work();
};
void S::work() {}
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
}
