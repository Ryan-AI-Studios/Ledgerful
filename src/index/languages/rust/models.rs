use crate::index::data_models::{ExtractedModel, ModelKind};
use crate::index::symbols::Symbol;
use miette::{IntoDiagnostic, Result};
use std::collections::HashSet;
use tree_sitter::{Node, Parser};

const PERSISTENCE_IDENTS: &[&str] = &["FromRow", "Queryable", "Insertable"];
const CLAP_OR_ERROR_IDENTS: &[&str] = &["Subcommand", "Parser", "Args", "ValueEnum", "Error"];
const ROW_GET_METHODS: &[&str] = &["get", "get_unwrap"];
const SKIP_OK_TYPES: &[&str] = &[
    "i8", "i16", "i32", "i64", "i128", "u8", "u16", "u32", "u64", "u128", "usize", "isize", "f32",
    "f64", "bool", "str", "String", "char", "Row", "Value", "ValueRef", "Option", "Vec", "HashMap",
    "BTreeMap", "HashSet", "BTreeSet",
];

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

    let clap_only = clap_or_error_only_names(tree.root_node(), content);
    let mut models = Vec::new();
    collect_rust_models(tree.root_node(), content, &clap_only, &mut models);
    Ok(unique_models(models))
}

fn unique_models(models: Vec<ExtractedModel>) -> Vec<ExtractedModel> {
    let mut seen: HashSet<(String, String)> = HashSet::new();
    let mut out = Vec::new();
    for model in models {
        let key = (
            model.model_name.clone(),
            model.model_kind.as_str().to_string(),
        );
        if seen.insert(key) {
            out.push(model);
        }
    }
    out
}

fn collect_rust_models(
    node: Node,
    content: &str,
    clap_only: &HashSet<String>,
    models: &mut Vec<ExtractedModel>,
) {
    if node.kind() == "mod_item" && is_cfg_test_module(node, content) {
        return;
    }

    match node.kind() {
        "struct_item" | "enum_item" => collect_derive_model(node, content, models),
        "function_item" => collect_row_mapper_model(node, content, clap_only, models),
        "closure_expression" => collect_query_map_struct_models(node, content, clap_only, models),
        _ => {}
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_rust_models(child, content, clap_only, models);
    }
}

fn collect_derive_model(node: Node, content: &str, models: &mut Vec<ExtractedModel>) {
    let attr_idents = preceding_attribute_idents(node, content);
    if should_extract_persistence(&attr_idents)
        && let Some(name) = type_item_name(node, content)
    {
        push_persistence_model(models, name);
    }
}

fn collect_row_mapper_model(
    node: Node,
    content: &str,
    clap_only: &HashSet<String>,
    models: &mut Vec<ExtractedModel>,
) {
    let Some(params) = node.child_by_field_name("parameters") else {
        return;
    };
    let Some(first_type) = first_value_parameter_type(params) else {
        return;
    };
    if !type_base_is_row(first_type, content) {
        return;
    }
    let Some(ret) = node.child_by_field_name("return_type") else {
        return;
    };
    let Some(ok_name) = single_named_ok_type(ret, content) else {
        return;
    };
    if clap_only.contains(&ok_name) {
        return;
    }
    push_persistence_model(models, ok_name);
}

fn collect_query_map_struct_models(
    node: Node,
    content: &str,
    clap_only: &HashSet<String>,
    models: &mut Vec<ExtractedModel>,
) {
    visit_named_descendants(node, &mut |n| {
        if n.kind() != "struct_expression" {
            return;
        }
        if !struct_expression_reads_row(n, content) {
            return;
        }
        let Some(name) = struct_expression_name(n, content) else {
            return;
        };
        if clap_only.contains(&name) || SKIP_OK_TYPES.contains(&name.as_str()) {
            return;
        }
        push_persistence_model(models, name);
    });
}

fn push_persistence_model(models: &mut Vec<ExtractedModel>, name: String) {
    if name.is_empty() {
        return;
    }
    models.push(ExtractedModel {
        model_name: name,
        language: "Rust".to_string(),
        model_kind: ModelKind::Schema,
        confidence: 0.9,
        evidence: "-> <persistence>".to_string(),
    });
}

fn is_cfg_test_module(node: Node, content: &str) -> bool {
    let mut prev = node.prev_sibling();
    while let Some(p) = prev {
        if p.kind() == "attribute_item" {
            if let Ok(text) = p.utf8_text(content.as_bytes())
                && text.contains("cfg(test)")
            {
                return true;
            }
            prev = p.prev_sibling();
        } else if p.kind() == "line_comment" || p.kind() == "block_comment" {
            prev = p.prev_sibling();
        } else {
            break;
        }
    }
    false
}

fn clap_or_error_only_names(root: Node, content: &str) -> HashSet<String> {
    let mut names = HashSet::new();
    visit_named_descendants(root, &mut |n| {
        if n.kind() != "struct_item" && n.kind() != "enum_item" {
            return;
        }
        let idents = preceding_attribute_idents(n, content);
        if !idents_include(&idents, CLAP_OR_ERROR_IDENTS) || should_extract_persistence(&idents) {
            return;
        }
        if let Some(name) = type_item_name(n, content) {
            names.insert(name);
        }
    });
    names
}

fn type_item_name(node: Node, content: &str) -> Option<String> {
    let name_node = node.child_by_field_name("name")?;
    let name = name_node.utf8_text(content.as_bytes()).ok()?.to_string();
    if name.is_empty() { None } else { Some(name) }
}

fn first_value_parameter_type<'a>(params: Node<'a>) -> Option<Node<'a>> {
    let mut cursor = params.walk();
    for child in params.named_children(&mut cursor) {
        match child.kind() {
            "self_parameter" => continue,
            "parameter" => return child.child_by_field_name("type"),
            _ => {}
        }
    }
    None
}

fn type_base_is_row(node: Node, content: &str) -> bool {
    let inner = unwrap_reference_type(node);
    let base = generic_type_base(inner);
    type_ident_last_segment(base, content) == Some("Row")
}

fn unwrap_reference_type(mut node: Node<'_>) -> Node<'_> {
    while node.kind() == "reference_type" {
        if let Some(inner) = node.child_by_field_name("type") {
            node = inner;
            continue;
        }
        break;
    }
    node
}

fn generic_type_base(node: Node<'_>) -> Node<'_> {
    if node.kind() == "generic_type" {
        node.child_by_field_name("type").unwrap_or(node)
    } else {
        node
    }
}

fn type_ident_last_segment<'a>(node: Node<'a>, content: &'a str) -> Option<&'a str> {
    match node.kind() {
        "type_identifier" | "scoped_type_identifier" | "identifier" => {
            let text = node.utf8_text(content.as_bytes()).ok()?;
            Some(last_segment(text))
        }
        _ => None,
    }
}

fn single_named_ok_type(node: Node, content: &str) -> Option<String> {
    let inner = unwrap_reference_type(node);
    let unwrapped = unwrap_result_ok(inner, content)?;
    if unwrapped.kind() == "tuple_type" || unwrapped.kind() == "unit_type" {
        return None;
    }
    if unwrapped.kind() == "primitive_type" {
        return None;
    }
    let base = generic_type_base(unwrapped);
    let name = type_ident_last_segment(base, content)?;
    if SKIP_OK_TYPES.contains(&name) {
        return None;
    }
    Some(name.to_string())
}

fn unwrap_result_ok<'a>(node: Node<'a>, content: &'a str) -> Option<Node<'a>> {
    if node.kind() != "generic_type" {
        return Some(node);
    }
    let base = node.child_by_field_name("type")?;
    let base_name = type_ident_last_segment(base, content)?;
    if base_name != "Result" {
        return Some(node);
    }
    let args = node.child_by_field_name("type_arguments")?;
    first_type_argument(args)
}

fn first_type_argument(type_arguments: Node<'_>) -> Option<Node<'_>> {
    let mut cursor = type_arguments.walk();
    for child in type_arguments.named_children(&mut cursor) {
        if child.kind() == "lifetime" {
            continue;
        }
        return Some(child);
    }
    None
}

fn struct_expression_name(node: Node, content: &str) -> Option<String> {
    let name_node = node.child_by_field_name("name")?;
    let base = generic_type_base(name_node);
    let name = type_ident_last_segment(base, content)?;
    Some(name.to_string())
}

fn struct_expression_reads_row(node: Node, content: &str) -> bool {
    let mut found = false;
    visit_named_descendants(node, &mut |n| {
        if n.kind() == "call_expression" && call_is_row_get(n, content) {
            found = true;
        }
    });
    found
}

fn call_is_row_get(node: Node, content: &str) -> bool {
    let Some(mut function) = node.child_by_field_name("function") else {
        return false;
    };
    if function.kind() == "generic_function" {
        function = match function.child_by_field_name("function") {
            Some(inner) => inner,
            None => return false,
        };
    }
    if function.kind() != "field_expression" {
        return false;
    }
    let Some(field) = function.child_by_field_name("field") else {
        return false;
    };
    let Ok(method) = field.utf8_text(content.as_bytes()) else {
        return false;
    };
    if !ROW_GET_METHODS.contains(&method) {
        return false;
    }
    let Some(value) = function.child_by_field_name("value") else {
        return false;
    };
    type_ident_last_segment(value, content) == Some("row")
}

fn visit_named_descendants(node: Node, visit: &mut impl FnMut(Node)) {
    visit(node);
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        visit_named_descendants(child, visit);
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

    #[test]
    fn rust_row_mapper_fn_extracts_ledger_entry() {
        let model = one(
            r#"
struct LedgerEntry { id: i64 }
fn map_ledger_entry(row: &rusqlite::Row) -> rusqlite::Result<LedgerEntry> {
    Ok(LedgerEntry { id: row.get(0)? })
}
"#,
            "LedgerEntry",
        );
        assert_eq!(model.model_kind, ModelKind::Schema);
        assert!(model.evidence.contains("-> <persistence>"));
    }

    #[test]
    fn rust_row_lifetime_param_extracts_mapped_test() {
        let model = one(
            r#"
struct MappedTest { id: i64 }
fn mapped_test_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<MappedTest> {
    Ok(MappedTest { id: row.get(0)? })
}
"#,
            "MappedTest",
        );
        assert_eq!(model.model_kind, ModelKind::Schema);
    }

    #[test]
    fn rust_std_result_first_type_arg_is_symbol() {
        let model = one(
            r#"
struct Symbol { name: String }
fn map_project_symbol_row(row: &rusqlite::Row) -> std::result::Result<Symbol, rusqlite::Error> {
    Ok(Symbol { name: row.get(0)? })
}
"#,
            "Symbol",
        );
        assert_eq!(model.model_kind, ModelKind::Schema);
    }

    #[test]
    fn rust_tuple_ok_type_is_skipped() {
        let extracted = names(
            r#"
fn map_pair(row: &rusqlite::Row) -> rusqlite::Result<(Symbol, i64)> {
    Ok((row.get(0)?, row.get(1)?))
}
"#,
        );
        assert!(
            extracted.is_empty(),
            "tuple Ok must not extract: {extracted:?}"
        );
    }

    #[test]
    fn rust_query_map_struct_construction_extracts_timing_row() {
        let model = one(
            r#"
struct TimingRow { id: i64 }
fn load(conn: &rusqlite::Connection) {
    let _ = conn.query_map([], |row| {
        Ok(TimingRow { id: row.get(0)? })
    });
}
"#,
            "TimingRow",
        );
        assert_eq!(model.model_kind, ModelKind::Schema);
    }

    #[test]
    fn rust_query_map_turbofish_get_extracts() {
        let model = one(
            r#"
struct DataModelRow { id: i64 }
fn load(conn: &rusqlite::Connection) {
    let _ = conn.query_map([], |row| {
        Ok(DataModelRow { id: row.get::<_, i64>(0)? })
    });
}
"#,
            "DataModelRow",
        );
        assert_eq!(model.model_kind, ModelKind::Schema);
    }

    #[test]
    fn rust_query_map_get_unwrap_extracts() {
        let model = one(
            r#"
struct UnwrapRow { id: i64 }
fn load(conn: &rusqlite::Connection) {
    let _ = conn.query_map([], |row| {
        Ok(UnwrapRow { id: row.get_unwrap(0) })
    });
}
"#,
            "UnwrapRow",
        );
        assert_eq!(model.model_kind, ModelKind::Schema);
    }

    #[test]
    fn rust_two_constructions_in_one_file_dedupe() {
        let extracted = names(
            r#"
struct TimingRow { id: i64 }
fn a(conn: &rusqlite::Connection) {
    let _ = conn.query_map([], |row| Ok(TimingRow { id: row.get(0)? }));
}
fn b(conn: &rusqlite::Connection) {
    let _ = conn.query_row([], |row| Ok(TimingRow { id: row.get(0)? }));
}
"#,
        );
        assert_eq!(extracted, vec!["TimingRow".to_string()]);
    }

    #[test]
    fn rust_cfg_test_mod_struct_is_omitted() {
        let extracted = names(
            r#"
struct Product { id: i64 }
fn map_product(row: &rusqlite::Row) -> rusqlite::Result<Product> {
    Ok(Product { id: row.get(0)? })
}

#[cfg(test)]
mod tests {
    struct HelperRow { id: i64 }
    fn load(conn: &rusqlite::Connection) {
        let _ = conn.query_map([], |row| Ok(HelperRow { id: row.get(0)? }));
    }
}
"#,
        );
        assert_eq!(extracted, vec!["Product".to_string()]);
    }
}
