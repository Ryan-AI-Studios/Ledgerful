use super::common::{is_public_name, node_text};
use crate::index::data_models::{ExtractedModel, ModelKind};
use crate::index::symbols::Symbol;
use miette::{IntoDiagnostic, Result};
use tree_sitter::{Node, Parser};

/// Type-ish models floor: named class/struct/enum/union (D4).
pub fn extract_data_models(
    content: &str,
    _file_path: &str,
    _symbols: &[Symbol],
) -> Result<Vec<ExtractedModel>> {
    let mut parser = Parser::new();
    let language = tree_sitter_cpp::LANGUAGE;
    parser.set_language(&language.into()).into_diagnostic()?;

    let tree = parser
        .parse(content, None)
        .ok_or_else(|| miette::miette!("Failed to parse C/C++ content"))?;

    let mut models = Vec::new();
    collect_cpp_models(tree.root_node(), content, &mut models);
    models.sort_by(|a, b| a.model_name.cmp(&b.model_name));
    Ok(models)
}

fn collect_cpp_models(node: Node, content: &str, models: &mut Vec<ExtractedModel>) {
    let (kind, evidence_label) = match node.kind() {
        "class_specifier" => (Some(ModelKind::Class), "class"),
        "struct_specifier" => (Some(ModelKind::Struct), "struct"),
        "enum_specifier" => (Some(ModelKind::Schema), "enum"),
        "union_specifier" => (Some(ModelKind::Struct), "union"),
        _ => (None, ""),
    };

    if let Some(model_kind) = kind
        && let Some(name_node) = node.child_by_field_name("name")
    {
        let name = node_text(name_node, content);
        if !name.is_empty() {
            models.push(ExtractedModel {
                model_name: name.clone(),
                language: "cpp".to_string(),
                model_kind,
                confidence: if is_public_name(&name) { 0.95 } else { 0.8 },
                evidence: format!("{evidence_label} {name}"),
            });
        }
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_cpp_models(child, content, models);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::data_models::ModelKind;

    #[test]
    fn extracts_class_and_struct_models() {
        let content = r#"
class Widget {};
struct Point { int x; int y; };
enum Color { Red };
"#;
        let models = extract_data_models(content, "demo.cpp", &[]).unwrap();
        assert!(
            models
                .iter()
                .any(|m| m.model_name == "Widget" && m.model_kind == ModelKind::Class)
        );
        assert!(
            models
                .iter()
                .any(|m| m.model_name == "Point" && m.model_kind == ModelKind::Struct)
        );
        assert_eq!(models[0].language, "cpp");
    }
}
