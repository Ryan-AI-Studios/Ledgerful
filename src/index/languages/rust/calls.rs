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
                    let name = callee
                        .utf8_text(content.as_bytes())
                        .unwrap_or("")
                        .to_string();
                    if !name.is_empty() {
                        let short_name = name.rsplit("::").next().unwrap_or(&name).to_string();
                        let evidence = format!("call_expr:{}", name);
                        edges.push(CallEdge {
                            caller_name,
                            caller_file: path.to_path_buf(),
                            callee_name: short_name,
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
