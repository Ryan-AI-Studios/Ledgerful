use crate::index::bindings::{FileBinding, rust_use_is_local, sort_bindings};
use miette::{IntoDiagnostic, Result};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::LazyLock;
use tree_sitter::{Node, Parser, Query, QueryCursor, StreamingIterator};

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

// ---------------------------------------------------------------------------
// File bindings (0092 Part 1) — bound-name keyed, list forms expanded
// ---------------------------------------------------------------------------

/// Extract bound-name-keyed bindings for a source file.
///
/// Returns `Ok(None)` for unsupported extensions. An empty `Vec` means the
/// file was supported but has no imports/mods (DoD-1 empty set, not an error).
pub fn extract_file_bindings(path: &Path, content: &str) -> Result<Option<Vec<FileBinding>>> {
    let extension = path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or_default();

    let mut bindings = match extension {
        "rs" => extract_rust_bindings(content)?,
        "ts" | "tsx" | "js" | "jsx" => extract_typescript_bindings(content)?,
        "py" => extract_python_bindings(content)?,
        _ => return Ok(None),
    };

    sort_bindings(&mut bindings);
    // Dedup by the unique key used in the DB.
    bindings.dedup_by(|a, b| {
        a.bound_name == b.bound_name
            && a.source_path == b.source_path
            && a.binding_kind == b.binding_kind
    });

    Ok(Some(bindings))
}

fn node_text(node: Node<'_>, content: &str) -> String {
    node.utf8_text(content.as_bytes()).unwrap_or("").to_string()
}

fn path_last_segment(path: &str) -> &str {
    path.rsplit("::")
        .next()
        .unwrap_or(path)
        .rsplit('.')
        .next()
        .unwrap_or(path)
}

fn extract_rust_bindings(content: &str) -> Result<Vec<FileBinding>> {
    let mut parser = Parser::new();
    let language = tree_sitter_rust::LANGUAGE;
    parser.set_language(&language.into()).into_diagnostic()?;
    let tree = parser
        .parse(content, None)
        .ok_or_else(|| miette::miette!("Failed to parse Rust content for bindings"))?;

    let mut bindings = Vec::new();
    walk_rust_bindings(tree.root_node(), content, &mut bindings);
    Ok(bindings)
}

fn walk_rust_bindings(node: Node<'_>, content: &str, out: &mut Vec<FileBinding>) {
    match node.kind() {
        "use_declaration" => {
            if let Some(arg) = node.child_by_field_name("argument") {
                expand_rust_use_arg(arg, content, "", out);
            }
        }
        "mod_item" => {
            let name = node
                .child_by_field_name("name")
                .map(|n| node_text(n, content))
                .unwrap_or_default();
            if !name.is_empty() {
                let has_body = node.child_by_field_name("body").is_some();
                if has_body {
                    // Inline module — same file, not a sibling path.
                    out.push(FileBinding {
                        bound_name: name.clone(),
                        source_path: name.clone(),
                        binding_kind: "mod_inline".to_string(),
                        is_enumerable: true,
                        is_local: true,
                    });
                } else {
                    // `mod fs;` — file-path binding, proven local.
                    out.push(FileBinding {
                        bound_name: name.clone(),
                        source_path: name,
                        binding_kind: "mod".to_string(),
                        is_enumerable: true,
                        is_local: true,
                    });
                }
            }
        }
        _ => {}
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        // Do not walk into inline mod bodies for nested mods of other files;
        // nested `mod` inside an inline module is still a binding in this file.
        walk_rust_bindings(child, content, out);
    }
}

/// Expand a `use` argument node into one binding per bound name.
fn expand_rust_use_arg(node: Node<'_>, content: &str, prefix: &str, out: &mut Vec<FileBinding>) {
    match node.kind() {
        "identifier" | "self" | "super" | "crate" | "metavariable" => {
            let seg = node_text(node, content);
            let source = if prefix.is_empty() {
                seg.clone()
            } else {
                format!("{prefix}::{seg}")
            };
            let bound = if seg == "self" && !prefix.is_empty() {
                path_last_segment(prefix).to_string()
            } else {
                seg
            };
            if !bound.is_empty() && bound != "*" {
                out.push(FileBinding {
                    bound_name: bound,
                    source_path: source.clone(),
                    binding_kind: "use".to_string(),
                    is_enumerable: true,
                    is_local: rust_use_is_local(&source),
                });
            }
        }
        "scoped_identifier" => {
            let path = node
                .child_by_field_name("path")
                .map(|n| node_text(n, content))
                .unwrap_or_default();
            let name = node
                .child_by_field_name("name")
                .map(|n| node_text(n, content))
                .unwrap_or_default();
            let full = if prefix.is_empty() {
                if path.is_empty() {
                    name.clone()
                } else {
                    format!("{path}::{name}")
                }
            } else if path.is_empty() {
                format!("{prefix}::{name}")
            } else {
                format!("{prefix}::{path}::{name}")
            };
            if !name.is_empty() {
                out.push(FileBinding {
                    bound_name: name,
                    source_path: full.clone(),
                    binding_kind: "use".to_string(),
                    is_enumerable: true,
                    is_local: rust_use_is_local(&full),
                });
            }
        }
        "use_as_clause" => {
            let path_node = node.child_by_field_name("path");
            let alias_node = node.child_by_field_name("alias");
            let path_text = path_node.map(|n| node_text(n, content)).unwrap_or_default();
            let alias = alias_node
                .map(|n| node_text(n, content))
                .unwrap_or_default();
            let source = if prefix.is_empty() {
                path_text
            } else if path_text.is_empty() {
                prefix.to_string()
            } else {
                format!("{prefix}::{path_text}")
            };
            let bound = if alias.is_empty() {
                path_last_segment(&source).to_string()
            } else {
                alias
            };
            if !bound.is_empty() {
                out.push(FileBinding {
                    bound_name: bound,
                    source_path: source.clone(),
                    binding_kind: "use".to_string(),
                    is_enumerable: true,
                    is_local: rust_use_is_local(&source),
                });
            }
        }
        "use_wildcard" => {
            // `use foo::*` — non-enumerable, never proves locality for any segment.
            let path_from_node = node
                .child_by_field_name("path")
                .map(|n| node_text(n, content))
                .filter(|s| !s.is_empty())
                .or_else(|| {
                    let mut cursor = node.walk();
                    node.children(&mut cursor)
                        .find(|c| c.kind() != "*")
                        .map(|c| node_text(c, content))
                        .filter(|s| !s.is_empty())
                });
            let source = if !prefix.is_empty() {
                format!("{prefix}::*")
            } else if let Some(inner) = path_from_node {
                format!("{inner}::*")
            } else {
                "*".to_string()
            };
            out.push(FileBinding {
                bound_name: "*".to_string(),
                source_path: source,
                binding_kind: "use_wildcard".to_string(),
                is_enumerable: false,
                is_local: false,
            });
        }
        "scoped_use_list" => {
            let path_node = node.child_by_field_name("path");
            let list_node = node.child_by_field_name("list");
            let path_text = path_node.map(|n| node_text(n, content)).unwrap_or_default();
            let new_prefix = if prefix.is_empty() {
                path_text
            } else if path_text.is_empty() {
                prefix.to_string()
            } else {
                format!("{prefix}::{path_text}")
            };
            if let Some(list) = list_node {
                expand_rust_use_list(list, content, &new_prefix, out);
            }
        }
        "use_list" => {
            expand_rust_use_list(node, content, prefix, out);
        }
        _ => {
            // Fall back: walk children that may be nested use forms.
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if matches!(
                    child.kind(),
                    "identifier"
                        | "self"
                        | "super"
                        | "crate"
                        | "scoped_identifier"
                        | "use_as_clause"
                        | "use_wildcard"
                        | "scoped_use_list"
                        | "use_list"
                ) {
                    expand_rust_use_arg(child, content, prefix, out);
                }
            }
        }
    }
}

fn expand_rust_use_list(list: Node<'_>, content: &str, prefix: &str, out: &mut Vec<FileBinding>) {
    let mut cursor = list.walk();
    for child in list.children(&mut cursor) {
        if matches!(
            child.kind(),
            "identifier"
                | "self"
                | "super"
                | "crate"
                | "scoped_identifier"
                | "use_as_clause"
                | "use_wildcard"
                | "scoped_use_list"
                | "use_list"
        ) {
            expand_rust_use_arg(child, content, prefix, out);
        }
    }
}

fn extract_typescript_bindings(content: &str) -> Result<Vec<FileBinding>> {
    let mut parser = Parser::new();
    let language = tree_sitter_typescript::LANGUAGE_TYPESCRIPT;
    parser.set_language(&language.into()).into_diagnostic()?;
    let tree = parser
        .parse(content, None)
        .ok_or_else(|| miette::miette!("Failed to parse TypeScript content for bindings"))?;

    let mut bindings = Vec::new();
    walk_ts_bindings(tree.root_node(), content, &mut bindings);
    Ok(bindings)
}

fn walk_ts_bindings(node: Node<'_>, content: &str, out: &mut Vec<FileBinding>) {
    if node.kind() == "import_statement" {
        let source = node
            .child_by_field_name("source")
            .map(|n| {
                node_text(n, content)
                    .trim_matches(|c| c == '"' || c == '\'' || c == '`')
                    .to_string()
            })
            .unwrap_or_default();

        // Relative/local path heuristic for TS: ./ or ../
        let is_local = source.starts_with("./") || source.starts_with("../");

        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.kind() == "import_clause" {
                walk_ts_import_clause(child, content, &source, is_local, out);
            }
        }
        // Side-effect import: `import "./x"` — no bound name; skip.
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_ts_bindings(child, content, out);
    }
}

fn walk_ts_import_clause(
    node: Node<'_>,
    content: &str,
    source: &str,
    is_local: bool,
    out: &mut Vec<FileBinding>,
) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "identifier" => {
                // default import
                let name = node_text(child, content);
                if !name.is_empty() {
                    out.push(FileBinding {
                        bound_name: name,
                        source_path: source.to_string(),
                        binding_kind: "import".to_string(),
                        is_enumerable: true,
                        is_local,
                    });
                }
            }
            "namespace_import" => {
                // import * as ns from "…"
                let mut c2 = child.walk();
                for g in child.children(&mut c2) {
                    if g.kind() == "identifier" {
                        let name = node_text(g, content);
                        if !name.is_empty() {
                            out.push(FileBinding {
                                bound_name: name,
                                source_path: source.to_string(),
                                binding_kind: "import_namespace".to_string(),
                                is_enumerable: true,
                                is_local,
                            });
                        }
                    }
                }
            }
            "named_imports" => {
                let mut c2 = child.walk();
                for g in child.children(&mut c2) {
                    if g.kind() == "import_specifier" {
                        let name_node = g.child_by_field_name("name");
                        let alias_node = g.child_by_field_name("alias");
                        let name = name_node.map(|n| node_text(n, content)).unwrap_or_default();
                        let bound = alias_node
                            .map(|n| node_text(n, content))
                            .filter(|s| !s.is_empty())
                            .unwrap_or_else(|| name.clone());
                        if !bound.is_empty() {
                            out.push(FileBinding {
                                bound_name: bound,
                                source_path: format!("{source}#{name}"),
                                binding_kind: "import".to_string(),
                                is_enumerable: true,
                                is_local,
                            });
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

fn extract_python_bindings(content: &str) -> Result<Vec<FileBinding>> {
    let mut parser = Parser::new();
    let language = tree_sitter_python::LANGUAGE;
    parser.set_language(&language.into()).into_diagnostic()?;
    let tree = parser
        .parse(content, None)
        .ok_or_else(|| miette::miette!("Failed to parse Python content for bindings"))?;

    let mut bindings = Vec::new();
    walk_py_bindings(tree.root_node(), content, &mut bindings);
    Ok(bindings)
}

fn walk_py_bindings(node: Node<'_>, content: &str, out: &mut Vec<FileBinding>) {
    match node.kind() {
        "import_statement" => {
            // import os / import a.b as c
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.kind() == "dotted_name" {
                    let path = node_text(child, content);
                    let bound = path_last_segment(&path).to_string();
                    // Relative packages (leading dots) are local; absolute third-party not.
                    let is_local = path.starts_with('.');
                    out.push(FileBinding {
                        bound_name: bound,
                        source_path: path,
                        binding_kind: "import".to_string(),
                        is_enumerable: true,
                        is_local,
                    });
                } else if child.kind() == "aliased_import" {
                    let name = child
                        .child_by_field_name("name")
                        .map(|n| node_text(n, content))
                        .unwrap_or_default();
                    let alias = child
                        .child_by_field_name("alias")
                        .map(|n| node_text(n, content))
                        .unwrap_or_default();
                    let bound = if alias.is_empty() {
                        path_last_segment(&name).to_string()
                    } else {
                        alias
                    };
                    let is_local = name.starts_with('.');
                    if !bound.is_empty() {
                        out.push(FileBinding {
                            bound_name: bound,
                            source_path: name,
                            binding_kind: "import".to_string(),
                            is_enumerable: true,
                            is_local,
                        });
                    }
                }
            }
        }
        "import_from_statement" => {
            let module = node
                .child_by_field_name("module_name")
                .map(|n| node_text(n, content))
                .unwrap_or_default();
            // from .x import y → local; from pkg import y → not proven local
            let is_local = module.starts_with('.')
                || node_text(node, content).contains("from .")
                || node
                    .children(&mut node.walk())
                    .any(|c| c.kind() == "relative_import");

            let mut saw_wildcard = false;
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                match child.kind() {
                    "wildcard_import" => {
                        saw_wildcard = true;
                    }
                    "dotted_name"
                        if child.id()
                            != node
                                .child_by_field_name("module_name")
                                .map(|n| n.id())
                                .unwrap_or(0) =>
                    {
                        let name = node_text(child, content);
                        out.push(FileBinding {
                            bound_name: name.clone(),
                            source_path: if module.is_empty() {
                                name
                            } else {
                                format!("{module}.{name}")
                            },
                            binding_kind: "from_import".to_string(),
                            is_enumerable: true,
                            is_local,
                        });
                    }
                    "aliased_import" => {
                        let name = child
                            .child_by_field_name("name")
                            .map(|n| node_text(n, content))
                            .unwrap_or_default();
                        let alias = child
                            .child_by_field_name("alias")
                            .map(|n| node_text(n, content))
                            .unwrap_or_default();
                        let bound = if alias.is_empty() {
                            name.clone()
                        } else {
                            alias
                        };
                        if !bound.is_empty() {
                            out.push(FileBinding {
                                bound_name: bound,
                                source_path: if module.is_empty() {
                                    name
                                } else {
                                    format!("{module}.{name}")
                                },
                                binding_kind: "from_import".to_string(),
                                is_enumerable: true,
                                is_local,
                            });
                        }
                    }
                    _ => {}
                }
            }
            if saw_wildcard {
                out.push(FileBinding {
                    bound_name: "*".to_string(),
                    source_path: if module.is_empty() {
                        "*".to_string()
                    } else {
                        format!("{module}.*")
                    },
                    binding_kind: "from_import_wildcard".to_string(),
                    is_enumerable: false,
                    is_local: false,
                });
            }
        }
        _ => {}
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_py_bindings(child, content, out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn binding_names(bindings: &[FileBinding]) -> Vec<String> {
        bindings.iter().map(|b| b.bound_name.clone()).collect()
    }

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

    // --- 0092 DoD-1 binding extraction ---

    #[test]
    fn bindings_rust_alias() {
        let content = "use std::collections::HashMap as Map;\n";
        let b = extract_file_bindings(Path::new("src/main.rs"), content)
            .unwrap()
            .unwrap();
        assert_eq!(b.len(), 1);
        assert_eq!(b[0].bound_name, "Map");
        assert!(b[0].source_path.contains("HashMap"));
        assert!(b[0].is_enumerable);
        assert!(!b[0].is_local, "std path must not be local");
    }

    #[test]
    fn bindings_rust_list_form_two_rows() {
        let content = "use a::{b, c as d};\n";
        let b = extract_file_bindings(Path::new("src/main.rs"), content)
            .unwrap()
            .unwrap();
        let names = binding_names(&b);
        assert!(names.contains(&"b".to_string()), "got {names:?}");
        assert!(names.contains(&"d".to_string()), "got {names:?}");
        assert!(
            !names.contains(&"c".to_string()),
            "alias c as d binds d not c"
        );
        assert_eq!(b.len(), 2);
    }

    #[test]
    fn bindings_rust_mod_fs() {
        let content = "pub mod fs;\n";
        let b = extract_file_bindings(Path::new("src/util/mod.rs"), content)
            .unwrap()
            .unwrap();
        assert_eq!(b.len(), 1);
        assert_eq!(b[0].bound_name, "fs");
        assert_eq!(b[0].binding_kind, "mod");
        assert!(b[0].is_local);
        assert!(b[0].is_enumerable);
    }

    #[test]
    fn bindings_rust_inline_mod_tests() {
        let content = "mod tests { fn t() {} }\n";
        let b = extract_file_bindings(Path::new("src/lib.rs"), content)
            .unwrap()
            .unwrap();
        assert_eq!(b.len(), 1);
        assert_eq!(b[0].bound_name, "tests");
        assert_eq!(b[0].binding_kind, "mod_inline");
        assert!(b[0].is_local);
    }

    #[test]
    fn bindings_rust_wildcard_non_enumerable() {
        let content = "use foo::*;\n";
        let b = extract_file_bindings(Path::new("src/lib.rs"), content)
            .unwrap()
            .unwrap();
        assert_eq!(b.len(), 1);
        assert_eq!(b[0].bound_name, "*");
        assert!(!b[0].is_enumerable);
        assert!(!b[0].is_local);
        assert_eq!(b[0].binding_kind, "use_wildcard");
    }

    #[test]
    fn bindings_rust_local_crate_use() {
        let content = "use crate::util::fs;\n";
        let b = extract_file_bindings(Path::new("src/main.rs"), content)
            .unwrap()
            .unwrap();
        assert_eq!(b.len(), 1);
        assert_eq!(b[0].bound_name, "fs");
        assert!(b[0].is_local);
        assert!(b[0].is_enumerable);
    }

    #[test]
    fn bindings_rust_empty_imports() {
        let content = "fn main() {}\n";
        let b = extract_file_bindings(Path::new("src/main.rs"), content)
            .unwrap()
            .unwrap();
        assert!(b.is_empty(), "no imports → empty set, not error");
    }

    #[test]
    fn bindings_typescript_named_and_alias() {
        let content = r#"
import { Foo as Bar, Baz } from "./foo";
import axios from "axios";
"#;
        let b = extract_file_bindings(Path::new("src/app.ts"), content)
            .unwrap()
            .unwrap();
        let names = binding_names(&b);
        assert!(names.contains(&"Bar".to_string()), "got {names:?}");
        assert!(names.contains(&"Baz".to_string()), "got {names:?}");
        assert!(names.contains(&"axios".to_string()), "got {names:?}");
        let bar = b.iter().find(|x| x.bound_name == "Bar").unwrap();
        assert!(bar.is_local, "./foo is local");
        let axios_b = b.iter().find(|x| x.bound_name == "axios").unwrap();
        assert!(!axios_b.is_local, "package import not local");
    }

    #[test]
    fn bindings_python_from_import() {
        let content = r#"
import os
from pkg.module import thing as t
from .local import helper
"#;
        let b = extract_file_bindings(Path::new("app.py"), content)
            .unwrap()
            .unwrap();
        let names = binding_names(&b);
        assert!(names.contains(&"os".to_string()), "got {names:?}");
        assert!(names.contains(&"t".to_string()), "got {names:?}");
        assert!(names.contains(&"helper".to_string()), "got {names:?}");
        let t = b.iter().find(|x| x.bound_name == "t").unwrap();
        assert!(!t.is_local);
        let helper = b.iter().find(|x| x.bound_name == "helper").unwrap();
        assert!(helper.is_local);
    }

    #[test]
    fn bindings_unsupported_extension_none() {
        let r = extract_file_bindings(Path::new("notes.txt"), "hello").unwrap();
        assert!(r.is_none());
    }
}
