use crate::index::call_graph::{CallEdge, CallKind, ResolutionStatus};
use crate::index::symbols::Symbol;
use miette::{IntoDiagnostic, Result};
use std::path::Path;
use tree_sitter::Parser;

pub fn extract_calls(path: &Path, content: &str, _symbols: &[Symbol]) -> Result<Vec<CallEdge>> {
    let mut parser = Parser::new();
    let language = tree_sitter_python::LANGUAGE;
    parser.set_language(&language.into()).into_diagnostic()?;

    let tree = parser
        .parse(content, None)
        .ok_or_else(|| miette::miette!("Failed to parse Python content"))?;

    let mut edges = Vec::new();
    collect_py_call_edges(path, tree.root_node(), content, &mut edges);
    Ok(edges)
}

fn collect_py_call_edges(
    path: &Path,
    node: tree_sitter::Node,
    content: &str,
    edges: &mut Vec<CallEdge>,
) {
    let kind = node.kind();

    if kind == "call" {
        let caller_name = find_py_enclosing_function(node, content);
        // In Python tree-sitter, a call node's first child is the function being called.
        let callee_node = node.child(0);
        if let Some(callee) = callee_node {
            match callee.kind() {
                "identifier" => {
                    let name = callee
                        .utf8_text(content.as_bytes())
                        .unwrap_or("")
                        .to_string();
                    if !name.is_empty() {
                        // Check if this is a known dynamic-dispatch pattern like getattr()
                        let call_kind = if name == "getattr" {
                            CallKind::Dynamic
                        } else {
                            CallKind::Direct
                        };
                        let resolution_status = if call_kind == CallKind::Dynamic {
                            ResolutionStatus::Unresolved
                        } else {
                            ResolutionStatus::Resolved
                        };
                        let confidence = call_kind.default_confidence();
                        let evidence = format!("call_expr:{name}()");
                        edges.push(CallEdge {
                            caller_name,
                            caller_file: path.to_path_buf(),
                            callee_name: name,
                            callee_file: None,
                            call_kind,
                            resolution_status,
                            confidence,
                            evidence,
                        });
                    }
                }
                "attribute" => {
                    // e.g. obj.method() / json.loads() — store dotted receiver.field
                    // so external members do not false-resolve to bare local names.
                    let callee_name =
                        super::common::extract_py_attribute_qualified(callee, content);
                    if !callee_name.is_empty() {
                        let full_text =
                            node.utf8_text(content.as_bytes()).unwrap_or("").to_string();
                        let evidence = format!("method_call:{full_text}");
                        edges.push(CallEdge {
                            caller_name,
                            caller_file: path.to_path_buf(),
                            callee_name,
                            callee_file: None,
                            call_kind: CallKind::MethodCall,
                            resolution_status: ResolutionStatus::Resolved,
                            confidence: CallKind::MethodCall.default_confidence(),
                            evidence,
                        });
                    }
                }
                _ => {
                    // Unrecognized pattern (e.g. subscript, lambda invocation)
                    let text = callee
                        .utf8_text(content.as_bytes())
                        .unwrap_or("")
                        .to_string();
                    if !text.is_empty() {
                        let evidence = format!("dynamic:{text}");
                        edges.push(CallEdge {
                            caller_name,
                            caller_file: path.to_path_buf(),
                            callee_name: text,
                            callee_file: None,
                            call_kind: CallKind::Dynamic,
                            resolution_status: ResolutionStatus::Unresolved,
                            confidence: CallKind::Dynamic.default_confidence(),
                            evidence,
                        });
                    }
                }
            }
        }
    }

    // Recurse into children
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_py_call_edges(path, child, content, edges);
    }
}

/// Walk up the tree to find the nearest enclosing function_definition and return its name.
fn find_py_enclosing_function(node: tree_sitter::Node, content: &str) -> String {
    let mut current = node.parent();
    while let Some(parent) = current {
        if parent.kind() == "function_definition" {
            let mut cursor = parent.walk();
            for child in parent.children(&mut cursor) {
                if child.kind() == "identifier" {
                    return child
                        .utf8_text(content.as_bytes())
                        .unwrap_or("")
                        .to_string();
                }
            }
        }
        current = parent.parent();
    }
    "<module>".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::call_graph::{CallKind, ResolutionStatus};
    use std::path::Path;

    #[test]
    fn test_extract_calls_function() {
        let content = r#"
def helper():
    return 42

def caller():
    return helper()
"#;

        let edges = extract_calls(Path::new("test.py"), content, &[]).unwrap();
        let direct: Vec<&CallEdge> = edges
            .iter()
            .filter(|e| e.call_kind == CallKind::Direct && e.callee_name == "helper")
            .collect();
        assert!(!direct.is_empty(), "should find a DIRECT call to helper");
        assert_eq!(direct[0].caller_name, "caller");
        assert_eq!(direct[0].resolution_status, ResolutionStatus::Resolved);
    }

    #[test]
    fn test_extract_calls_method() {
        let content = r#"
class Service:
    def process(self):
        pass

def caller():
    s = Service()
    s.process()
"#;

        let edges = extract_calls(Path::new("test.py"), content, &[]).unwrap();
        let method: Vec<&CallEdge> = edges
            .iter()
            .filter(|e| e.call_kind == CallKind::MethodCall && e.callee_name == "s.process")
            .collect();
        assert!(
            !method.is_empty(),
            "should find a METHOD_CALL to s.process (dotted form)"
        );
    }

    #[test]
    fn test_extract_calls_external_member_dotted() {
        // DoD-9: json.loads must be stored as dotted form, not bare "loads".
        let content = r#"
def caller():
    return json.loads(x)
"#;
        let edges = extract_calls(Path::new("test.py"), content, &[]).unwrap();
        let loads: Vec<&CallEdge> = edges
            .iter()
            .filter(|e| e.callee_name == "json.loads")
            .collect();
        assert!(
            !loads.is_empty(),
            "should store callee as json.loads, got: {:?}",
            edges.iter().map(|e| &e.callee_name).collect::<Vec<_>>()
        );
        assert!(
            edges.iter().all(|e| e.callee_name != "loads"),
            "must not store bare loads"
        );
    }

    /// R1-07: extract `json.loads` + resolve with same-file Function `loads` → Unresolved.
    #[test]
    fn e2e_extract_json_loads_resolve_same_file_function_unresolved() {
        use crate::index::call_graph::ResolutionStatus;
        use crate::index::resolve::{
            ResolveCandidate, ResolveInput, build_resolve_maps, resolve_callee,
        };

        let content = r#"
def loads(x):
    return x

def caller():
    return json.loads(x)
"#;
        let edges = extract_calls(Path::new("test.py"), content, &[]).unwrap();
        let callee = edges
            .iter()
            .find(|e| e.callee_name.contains("loads") && e.caller_name == "caller")
            .expect("caller→loads edge");
        assert_eq!(callee.callee_name, "json.loads");

        let (by_bare, by_qn) = build_resolve_maps(vec![ResolveCandidate {
            symbol_id: 1,
            file_id: 10,
            symbol_name: "loads".to_string(),
            qualified_name: "loads".to_string(),
            symbol_kind: "Function".to_string(),
        }]);
        let bindings = std::collections::HashMap::new();
        let modules = std::collections::HashMap::new();
        let r = resolve_callee(ResolveInput {
            callee_name: &callee.callee_name,
            caller_file_id: 10,
            candidates_by_bare_name: &by_bare,
            candidates_by_qualified: &by_qn,
            caller_module_path: None,
            caller_bindings: &bindings,
            module_path_by_file: &modules,
        });
        assert_eq!(r.status, ResolutionStatus::Unresolved);
        assert_eq!(r.callee_symbol_id, None);
    }

    #[test]
    fn test_extract_calls_dynamic_dispatch() {
        let content = r#"
def caller():
    fn = getattr(obj, "method_name")
    fn()
"#;

        let edges = extract_calls(Path::new("test.py"), content, &[]).unwrap();
        let getattr_edge: Vec<&CallEdge> = edges
            .iter()
            .filter(|e| e.callee_name == "getattr" && e.call_kind == CallKind::Dynamic)
            .collect();
        assert!(
            !getattr_edge.is_empty(),
            "should find a DYNAMIC call to getattr"
        );
    }
}
