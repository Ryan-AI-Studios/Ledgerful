use crate::index::data_models::{ExtractedModel, ModelKind};
use crate::index::symbols::Symbol;
use miette::{IntoDiagnostic, Result};
use tree_sitter::Parser;

/// Directories that conventionally indicate data model definitions in TypeScript projects.
const TS_MODEL_DIRS: &[&str] = &["models/", "types/", "schemas/", "interfaces/"];

pub fn extract_data_models(
    content: &str,
    file_path: &str,
    _symbols: &[Symbol],
) -> Result<Vec<ExtractedModel>> {
    let mut parser = Parser::new();
    let language = tree_sitter_typescript::LANGUAGE_TYPESCRIPT;
    parser.set_language(&language.into()).into_diagnostic()?;

    let tree = parser
        .parse(content, None)
        .ok_or_else(|| miette::miette!("Failed to parse TypeScript content"))?;

    let mut models = Vec::new();
    collect_ts_data_models(tree.root_node(), content, file_path, &mut models);
    Ok(models)
}

fn collect_ts_data_models(
    node: tree_sitter::Node,
    content: &str,
    file_path: &str,
    models: &mut Vec<ExtractedModel>,
) {
    let kind = node.kind();

    // --- TypeORM: class with @Entity decorator ---
    if kind == "decorator" {
        let decorator_text = node.utf8_text(content.as_bytes()).unwrap_or("");

        if decorator_text.contains("@Entity") {
            // The decorated class is a sibling of the decorator under the parent
            if let Some(parent) = node.parent() {
                let mut cursor = parent.walk();
                for child in parent.children(&mut cursor) {
                    if child.kind() == "class_declaration" {
                        let class_name = child
                            .child_by_field_name("name")
                            .and_then(|n| n.utf8_text(content.as_bytes()).ok())
                            .unwrap_or("")
                            .to_string();
                        if !class_name.is_empty() {
                            models.push(ExtractedModel {
                                model_name: class_name,
                                language: "TypeScript".to_string(),
                                model_kind: ModelKind::Class,
                                confidence: 1.0,
                                evidence: "decorator: @Entity".to_string(),
                            });
                        }
                    }
                }
            }
        }
    }

    // --- Classes that extend Model (Sequelize, Objection) ---
    if kind == "class_declaration" {
        let class_name = node
            .child_by_field_name("name")
            .and_then(|n| n.utf8_text(content.as_bytes()).ok())
            .unwrap_or("")
            .to_string();

        if !class_name.is_empty() {
            // Check for extends clause
            let class_text = node.utf8_text(content.as_bytes()).unwrap_or("");
            if class_text.contains("extends Model") {
                models.push(ExtractedModel {
                    model_name: class_name,
                    language: "TypeScript".to_string(),
                    model_kind: ModelKind::Class,
                    confidence: 0.9,
                    evidence: "extends: Model".to_string(),
                });
            }
        }
    }

    // --- Directory convention: interfaces and type aliases in model directories ---
    if kind == "interface_declaration" || kind == "type_alias_declaration" {
        let name = node
            .child_by_field_name("name")
            .and_then(|n| n.utf8_text(content.as_bytes()).ok())
            .unwrap_or("")
            .to_string();

        if !name.is_empty() {
            let dir_match = TS_MODEL_DIRS.iter().find(|dir| file_path.contains(*dir));
            if let Some(dir) = dir_match {
                // Only add if not already detected by a higher-confidence rule
                if !models.iter().any(|m| m.model_name == name) {
                    let model_kind = if kind == "interface_declaration" {
                        ModelKind::Interface
                    } else {
                        ModelKind::Schema
                    };
                    models.push(ExtractedModel {
                        model_name: name,
                        language: "TypeScript".to_string(),
                        model_kind,
                        confidence: 0.7,
                        evidence: format!("dir: {dir}"),
                    });
                }
            }
        }
    }

    // Recurse into children
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_ts_data_models(child, content, file_path, models);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::data_models::ModelKind;

    #[test]
    fn test_extract_data_models_interface_in_models_dir() {
        let content = r#"
            export interface User {
                id: number;
                name: string;
                email: string;
            }
        "#;

        let models = extract_data_models(content, "src/models/user.ts", &[]).unwrap();
        let model = models
            .iter()
            .find(|m| m.model_name == "User")
            .expect("should find User data model via directory convention");
        assert_eq!(model.model_kind, ModelKind::Interface);
        assert!((model.confidence - 0.7).abs() < f64::EPSILON);
        assert!(model.evidence.contains("dir: models/"));
    }

    #[test]
    fn test_extract_data_models_entity_decorator() {
        let content = r#"
            @Entity("users")
            export class User {
                @PrimaryGeneratedColumn()
                id: number;

                @Column()
                name: string;
            }
        "#;

        let models = extract_data_models(content, "src/entities/user.entity.ts", &[]).unwrap();
        let model = models
            .iter()
            .find(|m| m.model_name == "User")
            .expect("should find User data model via @Entity decorator");
        assert_eq!(model.model_kind, ModelKind::Class);
        assert!((model.confidence - 1.0).abs() < f64::EPSILON);
        assert!(model.evidence.contains("decorator: @Entity"));
    }

    #[test]
    fn test_extract_data_models_extends_model() {
        let content = r#"
            export class User extends Model<User> {
                declare id: number;
                declare name: string;
            }
        "#;

        let models = extract_data_models(content, "src/db/user.model.ts", &[]).unwrap();
        let model = models
            .iter()
            .find(|m| m.model_name == "User")
            .expect("should find User data model via extends Model");
        assert_eq!(model.model_kind, ModelKind::Class);
        assert!((model.confidence - 0.9).abs() < f64::EPSILON);
        assert!(model.evidence.contains("extends: Model"));
    }

    #[test]
    fn test_extract_data_models_interface_not_in_model_dir() {
        let content = r#"
            export interface ConfigOptions {
                debug: boolean;
                port: number;
            }
        "#;

        let models = extract_data_models(content, "src/config/options.ts", &[]).unwrap();
        assert!(
            models.iter().all(|m| m.model_name != "ConfigOptions"),
            "interface in non-model dir should NOT be a data model"
        );
    }
}
