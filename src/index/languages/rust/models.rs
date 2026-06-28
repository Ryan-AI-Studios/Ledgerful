use crate::index::data_models::{ExtractedModel, ModelKind};
use crate::index::symbols::Symbol;
use miette::{IntoDiagnostic, Result};
use tree_sitter::{Node, Parser};

pub fn extract_data_models(
    content: &str,
    _path: &str,
    _symbols: &[Symbol],
) -> Result<Vec<ExtractedModel>> {
    let mut parser = Parser::new();
    let language = tree_sitter_rust::LANGUAGE;
    parser.set_language(&language.into()).into_diagnostic()?;

    let tree = parser
        .parse(content, None)
        .ok_or_else(|| miette::miette!("Failed to parse Rust content"))?;

    let mut models = Vec::new();
    collect_rust_models(tree.root_node(), content, &mut models);
    Ok(models)
}

fn collect_rust_models(node: Node, content: &str, models: &mut Vec<ExtractedModel>) {
    let kind = node.kind();

    if kind == "struct_item" || kind == "enum_item" {
        let text = node.utf8_text(content.as_bytes()).unwrap_or("");
        let mut text_to_check = text.to_string();
        let mut prev = node.prev_sibling();
        while let Some(p) = prev {
            if p.kind() == "attribute_item" {
                if let Ok(attr_text) = p.utf8_text(content.as_bytes()) {
                    text_to_check.push_str(attr_text);
                }
                prev = p.prev_sibling();
            } else if p.kind() == "line_comment" || p.kind() == "block_comment" {
                prev = p.prev_sibling();
            } else {
                break;
            }
        }

        let mut is_model = false;
        let model_kind = ModelKind::Struct;

        // Check for common DB traits in derive or attributes
        if text_to_check.contains("FromRow")
            || text_to_check.contains("Entity")
            || text_to_check.contains("Table")
            || text_to_check.contains("Queryable")
            || text_to_check.contains("Insertable")
        {
            is_model = true;
        }

        if is_model && let Some(name_node) = node.child_by_field_name("name") {
            let name = name_node
                .utf8_text(content.as_bytes())
                .unwrap_or("")
                .to_string();
            models.push(ExtractedModel {
                model_name: name,
                language: "Rust".to_string(),
                model_kind: if kind == "enum_item" {
                    ModelKind::Schema
                } else {
                    model_kind
                },
                confidence: 0.9,
                evidence: "Detected based on derive traits and keywords".to_string(),
            });
        }
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_rust_models(child, content, models);
    }
}
