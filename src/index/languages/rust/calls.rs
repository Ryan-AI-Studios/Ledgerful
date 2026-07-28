use crate::index::call_graph::{CallEdge, CallKind, ResolutionStatus};
use crate::index::symbols::Symbol;
use miette::{IntoDiagnostic, Result};
use std::path::Path;
use tree_sitter::{Node, Parser};

pub fn extract_calls(path: &Path, content: &str, _symbols: &[Symbol]) -> Result<Vec<CallEdge>> {
    let mut parser = Parser::new();
    let language = tree_sitter_rust::LANGUAGE;
    parser.set_language(&language.into()).into_diagnostic()?;

    let tree = parser
        .parse(content, None)
        .ok_or_else(|| miette::miette!("Failed to parse Rust content"))?;

    let mut edges = Vec::new();
    collect_call_edges(path, tree.root_node(), content, &mut edges);
    Ok(edges)
}

fn collect_call_edges(path: &Path, node: Node, content: &str, edges: &mut Vec<CallEdge>) {
    let kind = node.kind();

    if kind == "call_expression" {
        let caller_name = find_enclosing_function(node, content);
        let callee_node = node.child(0);
        if let Some(callee) = callee_node {
            match callee.kind() {
                "identifier" => {
                    let name = callee
                        .utf8_text(content.as_bytes())
                        .unwrap_or("")
                        .to_string();
                    if !name.is_empty() {
                        let evidence = format!("call_expr:{}()", name);
                        edges.push(CallEdge {
                            caller_name,
                            caller_file: path.to_path_buf(),
                            callee_name: name,
                            callee_file: None,
                            call_kind: CallKind::Direct,
                            resolution_status: ResolutionStatus::Resolved,
                            confidence: CallKind::Direct.default_confidence(),
                            evidence,
                        });
                    }
                }
                "method_call_expression" | "field_expression" => {
                    let callee_name = extract_method_call_name(callee, content);
                    if !callee_name.is_empty() {
                        let full_text =
                            node.utf8_text(content.as_bytes()).unwrap_or("").to_string();
                        let evidence = format!("method_call:{}", full_text);
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
                "scoped_identifier" => {
                    // Store dotted form (Foo::new → Foo.new) so the shared
                    // resolver can hit qualified_name; evidence keeps original.
                    let name = callee
                        .utf8_text(content.as_bytes())
                        .unwrap_or("")
                        .to_string();
                    if !name.is_empty() {
                        let dotted = name.replace("::", ".");
                        let evidence = format!("call_expr:{}", name);
                        edges.push(CallEdge {
                            caller_name,
                            caller_file: path.to_path_buf(),
                            callee_name: dotted,
                            callee_file: None,
                            call_kind: CallKind::Direct,
                            resolution_status: ResolutionStatus::Resolved,
                            confidence: CallKind::Direct.default_confidence(),
                            evidence,
                        });
                    }
                }
                "generic_function" => {
                    let func_name = callee
                        .child(0)
                        .and_then(|c| c.utf8_text(content.as_bytes()).ok())
                        .unwrap_or("")
                        .to_string();
                    if !func_name.is_empty() {
                        let evidence = format!("trait_dispatch:{}", func_name);
                        edges.push(CallEdge {
                            caller_name,
                            caller_file: path.to_path_buf(),
                            callee_name: func_name,
                            callee_file: None,
                            call_kind: CallKind::TraitDispatch,
                            resolution_status: ResolutionStatus::Ambiguous,
                            confidence: CallKind::TraitDispatch.default_confidence(),
                            evidence,
                        });
                    }
                }
                _ => {
                    let text = callee
                        .utf8_text(content.as_bytes())
                        .unwrap_or("")
                        .to_string();
                    if !text.is_empty() {
                        let evidence = format!("dynamic:{}", text);
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

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_call_edges(path, child, content, edges);
    }
}

fn extract_method_call_name(node: Node, content: &str) -> String {
    let mut cursor = node.walk();
    let mut last_ident = String::new();
    for child in node.children(&mut cursor) {
        if child.kind() == "identifier" || child.kind() == "field_identifier" {
            last_ident = child
                .utf8_text(content.as_bytes())
                .unwrap_or("")
                .to_string();
        }
    }
    last_ident
}

fn find_enclosing_function(node: Node, content: &str) -> String {
    let mut current = node.parent();
    while let Some(parent) = current {
        if parent.kind() == "function_item" || parent.kind() == "impl_item" {
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
    use crate::index::call_graph::CallKind;
    use std::path::Path;

    #[test]
    fn scoped_identifier_stores_dotted_qn_form() {
        // Foo::new() → callee Foo.new (not bare "new") for QN resolution.
        let content = r#"
fn caller() {
    let _ = Foo::new();
}
"#;
        let edges = extract_calls(Path::new("test.rs"), content, &[]).unwrap();
        let scoped: Vec<&CallEdge> = edges
            .iter()
            .filter(|e| e.callee_name == "Foo.new")
            .collect();
        assert!(
            !scoped.is_empty(),
            "expected Foo.new callee, got: {:?}",
            edges.iter().map(|e| &e.callee_name).collect::<Vec<_>>()
        );
        assert!(
            edges.iter().all(|e| e.callee_name != "new"),
            "must not store bare new for scoped_identifier"
        );
        assert!(
            scoped[0].evidence.contains("Foo::new"),
            "evidence keeps original path text"
        );
        assert_eq!(scoped[0].call_kind, CallKind::Direct);
    }

    /// R1-07: extract Foo::new + resolve against Foo.new / Bar.new → Foo only.
    #[test]
    fn e2e_extract_foo_new_resolves_to_foo_only() {
        use crate::index::call_graph::ResolutionStatus;
        use crate::index::resolve::{
            ResolveCandidate, ResolveInput, build_resolve_maps, resolve_callee,
        };

        let content = r#"
fn caller() {
    let _ = Foo::new();
}
"#;
        let edges = extract_calls(Path::new("test.rs"), content, &[]).unwrap();
        let callee = edges
            .iter()
            .find(|e| e.callee_name == "Foo.new")
            .expect("Foo.new edge from extract");

        let (by_bare, by_qn) = build_resolve_maps(vec![
            ResolveCandidate {
                symbol_id: 1,
                file_id: 10,
                symbol_name: "new".to_string(),
                qualified_name: "Foo.new".to_string(),
                symbol_kind: "Method".to_string(),
            },
            ResolveCandidate {
                symbol_id: 2,
                file_id: 20,
                symbol_name: "new".to_string(),
                qualified_name: "Bar.new".to_string(),
                symbol_kind: "Method".to_string(),
            },
        ]);
        let bindings = std::collections::HashMap::new();
        let modules = std::collections::HashMap::new();
        let r = resolve_callee(ResolveInput {
            callee_name: &callee.callee_name,
            caller_file_id: 99,
            candidates_by_bare_name: &by_bare,
            candidates_by_qualified: &by_qn,
            caller_module_path: None,
            caller_bindings: &bindings,
            module_path_by_file: &modules,
        });
        assert_eq!(r.status, ResolutionStatus::Resolved);
        assert_eq!(r.callee_symbol_id, Some(1));
        assert_ne!(r.callee_symbol_id, Some(2));
    }
}
