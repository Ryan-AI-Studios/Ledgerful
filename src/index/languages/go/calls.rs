use super::common::{
    collect_imports, extract_selector_field, extract_selector_operand, find_enclosing_function,
    node_text,
};
use crate::index::call_graph::{CallEdge, CallKind, ResolutionStatus};
use crate::index::symbols::Symbol;
use miette::{IntoDiagnostic, Result};
use std::collections::HashSet;
use std::path::Path;
use tree_sitter::Parser;

pub fn extract_calls(path: &Path, content: &str, _symbols: &[Symbol]) -> Result<Vec<CallEdge>> {
    let mut parser = Parser::new();
    let language = tree_sitter_go::LANGUAGE;
    parser.set_language(&language.into()).into_diagnostic()?;

    let tree = parser
        .parse(content, None)
        .ok_or_else(|| miette::miette!("Failed to parse Go content"))?;

    let imports = collect_imports(tree.root_node(), content);
    let import_aliases: HashSet<String> = imports.iter().map(|(alias, _)| alias.clone()).collect();

    let mut edges = Vec::new();
    collect_go_call_edges(path, tree.root_node(), content, &import_aliases, &mut edges);

    // Deterministic order for stable index output
    edges.sort_by(|a, b| {
        a.caller_name
            .cmp(&b.caller_name)
            .then(a.callee_name.cmp(&b.callee_name))
            .then(a.evidence.cmp(&b.evidence))
    });

    Ok(edges)
}

fn collect_go_call_edges(
    path: &Path,
    node: tree_sitter::Node,
    content: &str,
    import_aliases: &HashSet<String>,
    edges: &mut Vec<CallEdge>,
) {
    if node.kind() == "call_expression" {
        let caller_name = find_enclosing_function(node, content);
        let function_node = node.child_by_field_name("function");

        if let Some(callee) = function_node {
            match callee.kind() {
                "identifier" => {
                    let name = node_text(callee, content);
                    if !name.is_empty() {
                        // Bare identifiers default to same-package Resolved (Python parity).
                        edges.push(CallEdge {
                            caller_name,
                            caller_file: path.to_path_buf(),
                            callee_name: name.clone(),
                            callee_file: None,
                            call_kind: CallKind::Direct,
                            resolution_status: ResolutionStatus::Resolved,
                            confidence: CallKind::Direct.default_confidence(),
                            evidence: format!("call_expr:{name}()"),
                        });
                    }
                }
                "selector_expression" => {
                    let field = extract_selector_field(callee, content);
                    let operand = extract_selector_operand(callee, content);
                    if !field.is_empty() {
                        // Cross-package: selector whose operand is an imported package alias
                        let is_import_pkg = import_aliases.contains(&operand);
                        let resolution_status = if is_import_pkg {
                            ResolutionStatus::Unresolved
                        } else {
                            // Method call on a local value — same-package heuristic
                            ResolutionStatus::Resolved
                        };
                        let full = node_text(callee, content);
                        edges.push(CallEdge {
                            caller_name,
                            caller_file: path.to_path_buf(),
                            callee_name: if is_import_pkg {
                                format!("{operand}.{field}")
                            } else {
                                field
                            },
                            callee_file: None,
                            call_kind: CallKind::MethodCall,
                            resolution_status,
                            confidence: if is_import_pkg {
                                CallKind::External.default_confidence()
                            } else {
                                CallKind::MethodCall.default_confidence()
                            },
                            evidence: format!("method_call:{full}"),
                        });
                    }
                }
                _ => {
                    let text = node_text(callee, content);
                    if !text.is_empty() {
                        edges.push(CallEdge {
                            caller_name,
                            caller_file: path.to_path_buf(),
                            callee_name: text.clone(),
                            callee_file: None,
                            call_kind: CallKind::Dynamic,
                            resolution_status: ResolutionStatus::Unresolved,
                            confidence: CallKind::Dynamic.default_confidence(),
                            evidence: format!("dynamic:{text}"),
                        });
                    }
                }
            }
        }
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_go_call_edges(path, child, content, import_aliases, edges);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::call_graph::{CallKind, ResolutionStatus};
    use crate::index::languages::go::extract_symbols;
    use std::path::Path;

    #[test]
    fn resolved_local_call_and_unresolved_cross_package() {
        let content = r#"
package demo

import "fmt"

func helper() int {
    return 42
}

func caller() {
    _ = helper()
    fmt.Println("hi")
}
"#;
        let symbols = extract_symbols(content).unwrap().unwrap();
        let edges = extract_calls(Path::new("demo.go"), content, &symbols).unwrap();

        let local: Vec<&CallEdge> = edges
            .iter()
            .filter(|e| e.callee_name == "helper" && e.call_kind == CallKind::Direct)
            .collect();
        assert!(!local.is_empty(), "should find DIRECT call to helper");
        assert_eq!(local[0].caller_name, "caller");
        assert_eq!(local[0].resolution_status, ResolutionStatus::Resolved);

        let cross: Vec<&CallEdge> = edges
            .iter()
            .filter(|e| {
                e.callee_name == "fmt.Println"
                    || (e.callee_name == "Println" && e.evidence.contains("fmt"))
            })
            .collect();
        assert!(
            !cross.is_empty(),
            "should find cross-package call to fmt.Println; edges={edges:?}"
        );
        assert_eq!(cross[0].resolution_status, ResolutionStatus::Unresolved);
    }

    #[test]
    fn method_call_on_local_receiver_resolved() {
        let content = r#"
package demo

type S struct{}

func (s *S) Work() {}

func caller() {
    var s S
    s.Work()
}
"#;
        let symbols = extract_symbols(content).unwrap().unwrap();
        let edges = extract_calls(Path::new("demo.go"), content, &symbols).unwrap();
        let method: Vec<&CallEdge> = edges
            .iter()
            .filter(|e| e.call_kind == CallKind::MethodCall && e.callee_name == "Work")
            .collect();
        assert!(!method.is_empty(), "should find METHOD_CALL to Work");
        assert_eq!(method[0].resolution_status, ResolutionStatus::Resolved);
    }
}
