use super::common::{is_exported, node_text};
use crate::index::data_models::{ExtractedModel, ModelKind};
use crate::index::symbols::Symbol;
use miette::{IntoDiagnostic, Result};
use tree_sitter::{Node, Parser};

pub fn extract_data_models(
    content: &str,
    _file_path: &str,
    _symbols: &[Symbol],
) -> Result<Vec<ExtractedModel>> {
    let mut parser = Parser::new();
    let language = tree_sitter_go::LANGUAGE;
    parser.set_language(&language.into()).into_diagnostic()?;

    let tree = parser
        .parse(content, None)
        .ok_or_else(|| miette::miette!("Failed to parse Go content"))?;

    let mut models = Vec::new();
    collect_go_data_models(tree.root_node(), content, &mut models);

    models.sort_by(|a, b| a.model_name.cmp(&b.model_name));
    Ok(models)
}

fn collect_go_data_models(node: Node, content: &str, models: &mut Vec<ExtractedModel>) {
    // type_spec with struct_type that has at least one field with a json tag
    if node.kind() == "type_spec" {
        let name = node
            .child_by_field_name("name")
            .map(|n| node_text(n, content))
            .unwrap_or_default();
        if let Some(type_node) = node.child_by_field_name("type")
            && type_node.kind() == "struct_type"
            && !name.is_empty()
        {
            let json_tags = collect_json_tags(type_node, content);
            if !json_tags.is_empty() {
                let evidence = format!("struct with json tags: {}", json_tags.join(", "));
                models.push(ExtractedModel {
                    model_name: name.clone(),
                    language: "go".to_string(),
                    model_kind: ModelKind::Struct,
                    confidence: if is_exported(&name) { 1.0 } else { 0.9 },
                    evidence,
                });
            }
        }
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_go_data_models(child, content, models);
    }
}

fn collect_json_tags(struct_type: Node, content: &str) -> Vec<String> {
    let mut tags = Vec::new();
    let mut stack = vec![struct_type];
    while let Some(node) = stack.pop() {
        if node.kind() == "field_declaration" {
            // field_declaration may have a `tag` field (raw_string_literal)
            if let Some(tag_node) = node.child_by_field_name("tag") {
                let tag_text = node_text(tag_node, content);
                if let Some(json_name) = parse_json_tag(&tag_text) {
                    tags.push(json_name);
                }
            } else {
                // Fallback: scan children for raw_string_literal containing json:
                let mut cursor = node.walk();
                for child in node.children(&mut cursor) {
                    if child.kind() == "raw_string_literal" {
                        let tag_text = node_text(child, content);
                        if let Some(json_name) = parse_json_tag(&tag_text) {
                            tags.push(json_name);
                        }
                    }
                }
            }
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            stack.push(child);
        }
    }
    tags.sort();
    tags.dedup();
    tags
}

/// Parse `json:"name,omitempty"` (or similar) from a struct tag string.
fn parse_json_tag(tag_text: &str) -> Option<String> {
    let raw = tag_text.trim().trim_matches('`');
    // Look for json:"..."
    let key = "json:\"";
    let idx = raw.find(key)?;
    let after = &raw[idx + key.len()..];
    let end = after.find('"')?;
    let value = &after[..end];
    // First segment before comma is the name; "-" means skip
    let name = value.split(',').next()?.trim();
    if name.is_empty() || name == "-" {
        return None;
    }
    Some(name.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::data_models::ModelKind;

    #[test]
    fn extract_struct_with_json_tags() {
        let content = r#"
package demo

type User struct {
    ID    int    `json:"id"`
    Name  string `json:"name"`
    Email string `json:"email,omitempty"`
    skip  string `json:"-"`
}

type Plain struct {
    X int
}
"#;
        let models = extract_data_models(content, "models/user.go", &[]).unwrap();
        let user = models
            .iter()
            .find(|m| m.model_name == "User")
            .expect("User model with json tags");
        assert_eq!(user.language, "go");
        assert_eq!(user.model_kind, ModelKind::Struct);
        assert!((user.confidence - 1.0).abs() < f64::EPSILON);
        assert!(user.evidence.contains("id"));
        assert!(user.evidence.contains("name"));

        assert!(
            models.iter().all(|m| m.model_name != "Plain"),
            "struct without json tags should not be a model"
        );
    }

    #[test]
    fn parse_json_tag_variants() {
        assert_eq!(
            parse_json_tag(r#"`json:"id"`"#).as_deref(),
            Some("id")
        );
        assert_eq!(
            parse_json_tag(r#"`json:"name,omitempty"`"#).as_deref(),
            Some("name")
        );
        assert_eq!(parse_json_tag(r#"`json:"-"`"#), None);
        assert_eq!(
            parse_json_tag(r#"`db:"x" json:"y"`"#).as_deref(),
            Some("y")
        );
    }
}
