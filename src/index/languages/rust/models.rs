use crate::index::data_models::{ExtractedModel, ModelKind};
use crate::index::symbols::Symbol;
use miette::{IntoDiagnostic, Result};
use tree_sitter::{Node, Parser};

const PERSISTENCE_IDENTS: &[&str] = &["FromRow", "Queryable", "Insertable"];

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
        let attr_idents = preceding_attribute_idents(node, content);
        if should_extract_persistence(&attr_idents)
            && let Some(name_node) = node.child_by_field_name("name")
        {
            let name = name_node
                .utf8_text(content.as_bytes())
                .unwrap_or("")
                .to_string();
            if !name.is_empty() {
                models.push(ExtractedModel {
                    model_name: name,
                    language: "Rust".to_string(),
                    model_kind: ModelKind::Schema,
                    confidence: 0.9,
                    evidence: "-> <persistence>".to_string(),
                });
            }
        }
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_rust_models(child, content, models);
    }
}

/// Outer `attribute_item` siblings only (same prev_sibling walk as 0315).
/// Collects `identifier` / `scoped_identifier` segments — never the type body.
fn preceding_attribute_idents(node: Node, content: &str) -> Vec<String> {
    let mut idents = Vec::new();
    let mut prev = node.prev_sibling();
    while let Some(p) = prev {
        if p.kind() == "attribute_item" {
            collect_ident_segments(p, content, &mut idents);
            prev = p.prev_sibling();
        } else if p.kind() == "line_comment" || p.kind() == "block_comment" {
            prev = p.prev_sibling();
        } else {
            break;
        }
    }
    idents
}

fn collect_ident_segments(node: Node, content: &str, out: &mut Vec<String>) {
    let kind = node.kind();
    if kind == "identifier" || kind == "scoped_identifier" {
        if let Ok(text) = node.utf8_text(content.as_bytes())
            && !text.is_empty()
        {
            out.push(text.to_string());
        }
        if kind == "identifier" {
            return;
        }
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_ident_segments(child, content, out);
    }
}

fn last_segment(ident: &str) -> &str {
    ident.rsplit("::").next().unwrap_or(ident)
}

fn idents_include(idents: &[String], tokens: &[&str]) -> bool {
    idents
        .iter()
        .any(|ident| tokens.contains(&last_segment(ident)))
}

/// Persistence derive wins over clap / thiserror on the same type:
/// `Parser` / `Args` / `Subcommand` / `ValueEnum` / `Error` are not extract
/// tokens. A type with both a persistence ident and those idents still extracts.
fn should_extract_persistence(idents: &[String]) -> bool {
    idents_include(idents, PERSISTENCE_IDENTS)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(content: &str) -> Vec<String> {
        extract_data_models(content, "src/models.rs", &[])
            .expect("extract")
            .into_iter()
            .map(|m| m.model_name)
            .collect()
    }

    fn one(content: &str, expected: &str) -> crate::index::data_models::ExtractedModel {
        let models = extract_data_models(content, "src/models.rs", &[]).expect("extract");
        assert_eq!(
            models.len(),
            1,
            "expected one model {expected}, got {models:?}"
        );
        assert_eq!(models[0].model_name, expected);
        models.into_iter().next().expect("one")
    }

    #[test]
    fn rust_fromrow_is_schema() {
        let model = one(
            r#"
#[derive(sqlx::FromRow)]
struct UserRow {
    id: i64,
}
"#,
            "UserRow",
        );
        assert_eq!(model.model_kind, ModelKind::Schema);
        assert!((model.confidence - 0.9).abs() < f64::EPSILON);
        assert!(
            model.evidence.contains("-> <persistence>"),
            "evidence must be extract-internal persistence: {}",
            model.evidence
        );
    }

    #[test]
    fn rust_fromrow_bare_ident_is_schema() {
        let model = one(
            r#"
#[derive(FromRow)]
struct LedgerRow {
    id: i64,
}
"#,
            "LedgerRow",
        );
        assert_eq!(model.model_kind, ModelKind::Schema);
        assert!(model.evidence.contains("-> <persistence>"));
    }

    #[test]
    fn rust_table_in_name_is_not_enough() {
        let extracted = names(
            r#"
struct SurfaceTableProbe {
    n: u8,
}
"#,
        );
        assert!(
            extracted.is_empty(),
            "name-only Table must not extract: {extracted:?}"
        );
    }

    #[test]
    fn rust_entity_in_name_is_not_enough() {
        let extracted = names(
            r#"
struct ResolvedEntity {
    id: u8,
}
"#,
        );
        assert!(
            extracted.is_empty(),
            "name-only Entity must not extract: {extracted:?}"
        );
    }

    #[test]
    fn rust_clap_subcommand_not_extracted() {
        let extracted = names(
            r#"
/// data-models list plus TestsForEntityArgs
#[derive(Subcommand)]
enum Commands {
    DataModels,
}
"#,
        );
        assert!(
            extracted.is_empty(),
            "clap Subcommand must not extract even when body mentions Entity: {extracted:?}"
        );
    }

    #[test]
    fn rust_thiserror_only_not_extracted() {
        let extracted = names(
            r#"
#[derive(Error, Debug)]
enum LedgerError {
    Io,
}
"#,
        );
        assert!(
            extracted.is_empty(),
            "thiserror-only must not extract: {extracted:?}"
        );
    }

    #[test]
    fn rust_fromrow_plus_error_still_extracted() {
        let model = one(
            r#"
#[derive(FromRow, Error)]
struct MixedRow {
    id: i64,
}
"#,
            "MixedRow",
        );
        assert_eq!(model.model_kind, ModelKind::Schema);
        assert!(model.evidence.contains("-> <persistence>"));
    }

    #[test]
    fn rust_serde_only_not_extracted() {
        let extracted = names(
            r#"
#[derive(Serialize, Deserialize)]
struct Packet {
    n: u8,
}
"#,
        );
        assert!(
            extracted.is_empty(),
            "serde-only must not extract: {extracted:?}"
        );
    }

    #[test]
    fn rust_queryable_and_insertable_extract() {
        let extracted = names(
            r#"
#[derive(Queryable)]
struct AccountRow {
    id: i64,
}

#[derive(Insertable)]
struct NewAccount {
    name: String,
}
"#,
        );
        assert_eq!(
            extracted,
            vec!["AccountRow".to_string(), "NewAccount".to_string()]
        );
    }
}
