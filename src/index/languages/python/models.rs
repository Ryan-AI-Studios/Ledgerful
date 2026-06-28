use crate::index::data_models::{ExtractedModel, ModelKind};
use crate::index::symbols::Symbol;
use miette::{IntoDiagnostic, Result};
use tree_sitter::Parser;

/// Directories/filenames that conventionally indicate Python data models.
const PY_MODEL_DIRS: &[&str] = &["models/", "entities/", "domain/"];
const PY_MODEL_FILES: &[&str] = &["models.py"];

pub fn extract_data_models(
    content: &str,
    file_path: &str,
    _symbols: &[Symbol],
) -> Result<Vec<ExtractedModel>> {
    let mut parser = Parser::new();
    let language = tree_sitter_python::LANGUAGE;
    parser.set_language(&language.into()).into_diagnostic()?;

    let tree = parser
        .parse(content, None)
        .ok_or_else(|| miette::miette!("Failed to parse Python content"))?;

    let mut models = Vec::new();
    collect_py_data_models(tree.root_node(), content, file_path, &mut models);
    Ok(models)
}

fn collect_py_data_models(
    node: tree_sitter::Node,
    content: &str,
    file_path: &str,
    models: &mut Vec<ExtractedModel>,
) {
    let kind = node.kind();

    if kind == "class_definition" {
        let class_name = node
            .child_by_field_name("name")
            .and_then(|n| n.utf8_text(content.as_bytes()).ok())
            .unwrap_or("")
            .to_string();

        if !class_name.is_empty() {
            // Check base classes in argument_list
            let mut base_classes: Vec<String> = Vec::new();
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.kind() == "argument_list" {
                    let mut acursor = child.walk();
                    for arg in child.children(&mut acursor) {
                        let arg_text = arg.utf8_text(content.as_bytes()).unwrap_or("").to_string();
                        base_classes.push(arg_text);
                    }
                }
            }

            // Check for @dataclass decorator
            let mut has_dataclass = false;
            if let Some(parent) = node.parent()
                && parent.kind() == "decorated_definition"
            {
                let mut pcursor = parent.walk();
                for sibling in parent.children(&mut pcursor) {
                    if sibling.kind() == "decorator" {
                        let dec_text = sibling
                            .utf8_text(content.as_bytes())
                            .unwrap_or("")
                            .to_string();
                        if dec_text.contains("@dataclass") {
                            has_dataclass = true;
                        }
                    }
                }
            }

            // Check base classes against known model bases
            let mut found_model = false;
            for base in &base_classes {
                // Pydantic: BaseModel
                if base == "BaseModel" {
                    models.push(ExtractedModel {
                        model_name: class_name.clone(),
                        language: "Python".to_string(),
                        model_kind: ModelKind::Class,
                        confidence: 1.0,
                        evidence: "base: BaseModel".to_string(),
                    });
                    found_model = true;
                    break;
                }
                // SQLAlchemy: Base
                if base == "Base" {
                    models.push(ExtractedModel {
                        model_name: class_name.clone(),
                        language: "Python".to_string(),
                        model_kind: ModelKind::Class,
                        confidence: 1.0,
                        evidence: "base: Base".to_string(),
                    });
                    found_model = true;
                    break;
                }
                // Flask-SQLAlchemy: db.Model
                if base == "db.Model" {
                    models.push(ExtractedModel {
                        model_name: class_name.clone(),
                        language: "Python".to_string(),
                        model_kind: ModelKind::Class,
                        confidence: 1.0,
                        evidence: "base: db.Model".to_string(),
                    });
                    found_model = true;
                    break;
                }
                // Django: models.Model
                if base == "models.Model" {
                    models.push(ExtractedModel {
                        model_name: class_name.clone(),
                        language: "Python".to_string(),
                        model_kind: ModelKind::Class,
                        confidence: 1.0,
                        evidence: "base: models.Model".to_string(),
                    });
                    found_model = true;
                    break;
                }
            }

            // dataclass in models directory/file
            if !found_model && has_dataclass {
                let in_model_dir = PY_MODEL_DIRS.iter().any(|dir| file_path.contains(dir));
                let in_model_file = PY_MODEL_FILES.iter().any(|f| file_path.ends_with(f));
                if in_model_dir || in_model_file {
                    let dir_match = PY_MODEL_DIRS
                        .iter()
                        .find(|dir| file_path.contains(*dir))
                        .unwrap_or(&"models/");
                    models.push(ExtractedModel {
                        model_name: class_name.clone(),
                        language: "Python".to_string(),
                        model_kind: ModelKind::Class,
                        confidence: 0.7,
                        evidence: format!("dir: {dir_match}"),
                    });
                    found_model = true;
                }
            }

            // Directory convention: classes in models.py or models/ package
            if !found_model {
                let in_model_dir = PY_MODEL_DIRS.iter().any(|dir| file_path.contains(dir));
                let in_model_file = PY_MODEL_FILES.iter().any(|f| file_path.ends_with(f));
                if in_model_dir || in_model_file {
                    let dir_match = PY_MODEL_DIRS
                        .iter()
                        .find(|dir| file_path.contains(*dir))
                        .copied()
                        .unwrap_or("models.py");
                    models.push(ExtractedModel {
                        model_name: class_name.clone(),
                        language: "Python".to_string(),
                        model_kind: ModelKind::Class,
                        confidence: 0.7,
                        evidence: format!("dir: {dir_match}"),
                    });
                }
            }
        }
    }

    // Recurse into children
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_py_data_models(child, content, file_path, models);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::data_models::ModelKind;

    #[test]
    fn test_extract_data_models_pydantic() {
        let content = r#"
from pydantic import BaseModel

class User(BaseModel):
    id: int
    name: str
    email: str
"#;

        let models = extract_data_models(content, "src/models/user.py", &[]).unwrap();
        let model = models
            .iter()
            .find(|m| m.model_name == "User")
            .expect("should find User data model via Pydantic BaseModel");
        assert_eq!(model.model_kind, ModelKind::Class);
        assert!((model.confidence - 1.0).abs() < f64::EPSILON);
        assert!(model.evidence.contains("base: BaseModel"));
    }

    #[test]
    fn test_extract_data_models_sqlalchemy() {
        let content = r#"
from sqlalchemy.orm import Base

class User(Base):
    __tablename__ = "users"
    id = Column(Integer, primary_key=True)
"#;

        let models = extract_data_models(content, "src/db/user.py", &[]).unwrap();
        let model = models
            .iter()
            .find(|m| m.model_name == "User")
            .expect("should find User data model via SQLAlchemy Base");
        assert_eq!(model.model_kind, ModelKind::Class);
        assert!((model.confidence - 1.0).abs() < f64::EPSILON);
        assert!(model.evidence.contains("base: Base"));
    }

    #[test]
    fn test_extract_data_models_django() {
        let content = r#"
from django.db import models

class User(models.Model):
    name = models.CharField(max_length=100)
    email = models.EmailField()
"#;

        let models = extract_data_models(content, "src/models.py", &[]).unwrap();
        let model = models
            .iter()
            .find(|m| m.model_name == "User")
            .expect("should find User data model via Django models.Model");
        assert_eq!(model.model_kind, ModelKind::Class);
        assert!((model.confidence - 1.0).abs() < f64::EPSILON);
        assert!(model.evidence.contains("base: models.Model"));
    }

    #[test]
    fn test_extract_data_models_flask_sqlalchemy() {
        let content = r#"
from flask_sqlalchemy import SQLAlchemy

db = SQLAlchemy()

class User(db.Model):
    id = db.Column(db.Integer, primary_key=True)
    name = db.Column(db.String(100))
"#;

        let models = extract_data_models(content, "src/app/models.py", &[]).unwrap();
        let model = models
            .iter()
            .find(|m| m.model_name == "User")
            .expect("should find User data model via Flask-SQLAlchemy db.Model");
        assert_eq!(model.model_kind, ModelKind::Class);
        assert!((model.confidence - 1.0).abs() < f64::EPSILON);
        assert!(model.evidence.contains("base: db.Model"));
    }

    #[test]
    fn test_extract_data_models_dataclass_in_models() {
        let content = r#"
from dataclasses import dataclass

@dataclass
class UserDTO:
    id: int
    name: str
"#;

        let models = extract_data_models(content, "src/models/dto.py", &[]).unwrap();
        let model = models
            .iter()
            .find(|m| m.model_name == "UserDTO")
            .expect("should find UserDTO data model via dataclass in models dir");
        assert_eq!(model.model_kind, ModelKind::Class);
        assert!((model.confidence - 0.7).abs() < f64::EPSILON);
        assert!(model.evidence.contains("dir: models/"));
    }

    #[test]
    fn test_extract_data_models_plain_class_not_model() {
        let content = r#"
class Helper:
    def __init__(self, x: int):
        self.x = x

    def process(self):
        pass
"#;

        let models = extract_data_models(content, "src/utils/helper.py", &[]).unwrap();
        assert!(
            models.iter().all(|m| m.model_name != "Helper"),
            "plain class in non-model dir should NOT be a data model"
        );
    }
}
