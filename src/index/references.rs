use miette::{IntoDiagnostic, Result};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::LazyLock;
use tree_sitter::{Parser, Query, QueryCursor, StreamingIterator};

static TS_EXPORT_SPECIFIERS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"export\s*\{([^}]*)\}"#).expect("valid regex"));

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "camelCase")]
pub struct ImportExport {
    pub imported_from: Vec<String>,
    pub exported_symbols: Vec<String>,
}

pub fn extract_import_export(path: &Path, content: &str) -> Result<Option<ImportExport>> {
    let extension = path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or_default();

    let mut result = match extension {
        "rs" => extract_rust_import_export(content)?,
        "ts" | "tsx" | "js" | "jsx" => extract_typescript_import_export(content)?,
        "py" => extract_python_import_export(content)?,
        "go" => extract_go_import_export(content)?,
        _ => return Ok(None),
    };

    result.imported_from.sort_unstable();
    result.imported_from.dedup();
    result.exported_symbols.sort_unstable();
    result.exported_symbols.dedup();

    Ok(Some(result))
}

fn extract_rust_import_export(content: &str) -> Result<ImportExport> {
    let mut parser = Parser::new();
    let language = tree_sitter_rust::LANGUAGE;
    parser.set_language(&language.into()).into_diagnostic()?;
    let tree = parser
        .parse(content, None)
        .ok_or_else(|| miette::miette!("Failed to parse Rust content"))?;

    let import_query = Query::new(
        &language.into(),
        r#"(use_declaration argument: (_) @import)"#,
    )
    .into_diagnostic()?;
    let export_query = Query::new(
        &language.into(),
        r#"
        (function_item (visibility_modifier) name: (identifier) @export)
        (struct_item (visibility_modifier) name: (type_identifier) @export)
        (enum_item (visibility_modifier) name: (type_identifier) @export)
        (trait_item (visibility_modifier) name: (type_identifier) @export)
        (mod_item (visibility_modifier) name: (identifier) @export)
        (type_item (visibility_modifier) name: (type_identifier) @export)
    "#,
    )
    .into_diagnostic()?;

    Ok(ImportExport {
        imported_from: capture_texts(&import_query, &tree, content, "import")?,
        exported_symbols: capture_texts(&export_query, &tree, content, "export")?,
    })
}

fn extract_typescript_import_export(content: &str) -> Result<ImportExport> {
    let mut parser = Parser::new();
    let language = tree_sitter_typescript::LANGUAGE_TYPESCRIPT;
    parser.set_language(&language.into()).into_diagnostic()?;
    let tree = parser
        .parse(content, None)
        .ok_or_else(|| miette::miette!("Failed to parse TypeScript content"))?;

    let import_query = Query::new(
        &language.into(),
        r#"
        (import_statement source: (string (string_fragment) @import))
        (import_require_clause source: (string (string_fragment) @import))
    "#,
    )
    .into_diagnostic()?;
    let export_query = Query::new(
        &language.into(),
        r#"
        (export_statement declaration: (function_declaration name: (identifier) @export))
        (export_statement declaration: (class_declaration name: (type_identifier) @export))
        (export_statement declaration: (interface_declaration name: (type_identifier) @export))
        (export_statement declaration: (type_alias_declaration name: (type_identifier) @export))
        (export_statement declaration: (enum_declaration name: (identifier) @export))
    "#,
    )
    .into_diagnostic()?;
    let mut exported_symbols = capture_texts(&export_query, &tree, content, "export")?;
    for captures in TS_EXPORT_SPECIFIERS.captures_iter(content) {
        if let Some(specifiers) = captures.get(1) {
            for symbol in specifiers.as_str().split(',') {
                let symbol = symbol.trim();
                if symbol.is_empty() {
                    continue;
                }
                let symbol = symbol.split_whitespace().next().unwrap_or(symbol);
                exported_symbols.push(symbol.to_string());
            }
        }
    }

    Ok(ImportExport {
        imported_from: capture_texts(&import_query, &tree, content, "import")?,
        exported_symbols,
    })
}

fn extract_python_import_export(content: &str) -> Result<ImportExport> {
    let mut parser = Parser::new();
    let language = tree_sitter_python::LANGUAGE;
    parser.set_language(&language.into()).into_diagnostic()?;
    let tree = parser
        .parse(content, None)
        .ok_or_else(|| miette::miette!("Failed to parse Python content"))?;

    let import_query = Query::new(
        &language.into(),
        r#"
        (import_statement name: (dotted_name) @import)
        (import_from_statement module_name: (dotted_name) @import)
    "#,
    )
    .into_diagnostic()?;
    let export_query = Query::new(
        &language.into(),
        r#"
        (module (function_definition name: (identifier) @export))
        (module (class_definition name: (identifier) @export))
    "#,
    )
    .into_diagnostic()?;

    let exported_symbols = capture_texts(&export_query, &tree, content, "export")?
        .into_iter()
        .filter(|symbol| !symbol.starts_with('_'))
        .collect();

    Ok(ImportExport {
        imported_from: capture_texts(&import_query, &tree, content, "import")?,
        exported_symbols,
    })
}

fn extract_go_import_export(content: &str) -> Result<ImportExport> {
    let mut parser = Parser::new();
    let language = tree_sitter_go::LANGUAGE;
    parser.set_language(&language.into()).into_diagnostic()?;
    let tree = parser
        .parse(content, None)
        .ok_or_else(|| miette::miette!("Failed to parse Go content"))?;

    let import_query = Query::new(
        &language.into(),
        r#"(import_spec path: (interpreted_string_literal) @import)"#,
    )
    .into_diagnostic()?;

    let export_query = Query::new(
        &language.into(),
        r#"
        (function_declaration name: (identifier) @export)
        (method_declaration name: (field_identifier) @export)
        (type_declaration (type_spec name: (type_identifier) @export))
        (const_declaration (const_spec name: (identifier) @export))
        (var_declaration (var_spec name: (identifier) @export))
    "#,
    )
    .into_diagnostic()?;

    let imported_from = capture_texts(&import_query, &tree, content, "import")?
        .into_iter()
        .map(|s| s.trim_matches('"').to_string())
        .collect();

    let exported_symbols = capture_texts(&export_query, &tree, content, "export")?
        .into_iter()
        .filter(|s| s.chars().next().is_some_and(|c| c.is_uppercase()))
        .collect();

    Ok(ImportExport {
        imported_from,
        exported_symbols,
    })
}

fn capture_texts(
    query: &Query,
    tree: &tree_sitter::Tree,
    content: &str,
    capture_name: &str,
) -> Result<Vec<String>> {
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(query, tree.root_node(), content.as_bytes());
    let capture_index = query
        .capture_names()
        .iter()
        .position(|name| *name == capture_name)
        .ok_or_else(|| miette::miette!("Missing capture {capture_name}"))?;
    let mut values = Vec::new();

    while let Some(m) = matches.next() {
        for capture in m.captures {
            if capture.index as usize == capture_index {
                values.push(
                    capture
                        .node
                        .utf8_text(content.as_bytes())
                        .into_diagnostic()?
                        .to_string(),
                );
            }
        }
    }

    Ok(values)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_rust_import_export() {
        let content = r#"
use std::collections::HashMap;
pub fn run() {}
struct Private;
"#;
        let result = extract_import_export(Path::new("src/main.rs"), content)
            .unwrap()
            .unwrap();
        assert!(
            result
                .imported_from
                .contains(&"std::collections::HashMap".to_string())
        );
        assert!(result.exported_symbols.contains(&"run".to_string()));
    }

    #[test]
    fn test_extract_typescript_import_export() {
        let content = r#"
import { Foo } from "./foo";
export function run() {}
export { Foo };
"#;
        let result = extract_import_export(Path::new("src/app.ts"), content)
            .unwrap()
            .unwrap();
        assert!(result.imported_from.contains(&"./foo".to_string()));
        assert!(result.exported_symbols.contains(&"run".to_string()));
        assert!(result.exported_symbols.contains(&"Foo".to_string()));
    }

    #[test]
    fn test_extract_python_import_export() {
        let content = r#"
import os
from pkg.module import thing

def public_fn():
    pass

def _private_fn():
    pass
"#;
        let result = extract_import_export(Path::new("app.py"), content)
            .unwrap()
            .unwrap();
        assert!(result.imported_from.contains(&"os".to_string()));
        assert!(result.imported_from.contains(&"pkg.module".to_string()));
        assert!(result.exported_symbols.contains(&"public_fn".to_string()));
        assert!(!result.exported_symbols.contains(&"_private_fn".to_string()));
    }

    #[test]
    fn test_extract_go_import_export() {
        let content = r#"
package main
import "fmt"
import (
    "os"
    "github.com/stripe/stripe-go"
)
func Run() {}
func (s *Server) Start() {}
type User struct {}
const Max = 10
var Debug = true
"#;
        let result = extract_import_export(Path::new("main.go"), content)
            .unwrap()
            .unwrap();
        assert!(result.imported_from.contains(&"fmt".to_string()));
        assert!(result.imported_from.contains(&"os".to_string()));
        assert!(
            result
                .imported_from
                .contains(&"github.com/stripe/stripe-go".to_string())
        );
        assert!(result.exported_symbols.contains(&"Run".to_string()));
        assert!(result.exported_symbols.contains(&"Start".to_string()));
        assert!(result.exported_symbols.contains(&"User".to_string()));
        assert!(result.exported_symbols.contains(&"Max".to_string()));
        assert!(result.exported_symbols.contains(&"Debug".to_string()));
    }
}
