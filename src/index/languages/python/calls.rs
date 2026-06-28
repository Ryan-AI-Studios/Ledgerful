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
                    // e.g. obj.method() -- attribute node in Python tree-sitter
                    let callee_name = super::common::extract_py_attribute_name(callee, content);
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
            .filter(|e| e.call_kind == CallKind::MethodCall && e.callee_name == "process")
            .collect();
        assert!(!method.is_empty(), "should find a METHOD_CALL to process");
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
