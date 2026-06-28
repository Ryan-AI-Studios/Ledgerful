use crate::index::symbols::{Symbol, SymbolKind};
use miette::{IntoDiagnostic, Result};
use std::collections::BTreeMap;
use tree_sitter::{Parser, Query, QueryCursor, StreamingIterator};

pub fn extract_symbols(content: &str) -> Result<Option<Vec<Symbol>>> {
    let mut parser = Parser::new();
    let language = tree_sitter_rust::LANGUAGE;
    parser.set_language(&language.into()).into_diagnostic()?;

    let tree = parser
        .parse(content, None)
        .ok_or_else(|| miette::miette!("Failed to parse Rust content"))?;

    let query_str = r#"
        (function_item name: (identifier) @name) @symbol
        (struct_item name: (type_identifier) @name) @symbol
        (enum_item name: (type_identifier) @name) @symbol
        (trait_item name: (type_identifier) @name) @symbol
        (mod_item name: (identifier) @name) @symbol
        (type_item name: (type_identifier) @name) @symbol
        (use_declaration) @symbol
        (impl_item) @symbol
    "#;

    let query = Query::new(&language.into(), query_str).into_diagnostic()?;
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&query, tree.root_node(), content.as_bytes());

    let mut symbols = Vec::new();

    while let Some(m) = matches.next() {
        let mut name = String::new();
        let mut is_public = false;
        let mut kind = SymbolKind::Function;
        let mut metadata = BTreeMap::new();
        let mut symbol_node: Option<tree_sitter::Node> = None;
        let mut skip = false;
        for capture in m.captures {
            let capture_name = query.capture_names()[capture.index as usize];
            match capture_name {
                "name" => {
                    name = capture
                        .node
                        .utf8_text(content.as_bytes())
                        .into_diagnostic()?
                        .to_string();
                }
                "symbol" => {
                    let node = capture.node;
                    symbol_node = Some(node);
                    match node.kind() {
                        "function_item" => kind = SymbolKind::Function,
                        "struct_item" => kind = SymbolKind::Struct,
                        "enum_item" => kind = SymbolKind::Enum,
                        "trait_item" => kind = SymbolKind::Trait,
                        "mod_item" => kind = SymbolKind::Module,
                        "type_item" => kind = SymbolKind::Type,
                        "impl_item" => {
                            kind = SymbolKind::Type;
                            // Try to find the type name in the impl block
                            let mut walk = node.walk();
                            for child in node.children(&mut walk) {
                                if child.kind() == "type_identifier" {
                                    name = child
                                        .utf8_text(content.as_bytes())
                                        .into_diagnostic()?
                                        .to_string();
                                    break;
                                }
                            }
                            if name.is_empty() {
                                name = "impl".to_string();
                            }
                        }
                        "use_declaration" => {
                            // Only handle public re-exports
                            let mut cursor = node.walk();
                            let mut is_pub = false;
                            for child in node.children(&mut cursor) {
                                if child.kind() == "visibility_modifier" {
                                    is_pub = true;
                                    break;
                                }
                            }
                            if is_pub {
                                kind = SymbolKind::Type; // Fallback kind
                                is_public = true;
                                // Extract re-exported name(s)
                                name = extract_use_name(node, content);
                                metadata.insert("reexport".to_string(), "true".to_string());
                            } else {
                                skip = true;
                            }
                        }
                        _ => {}
                    }

                    // Check for visibility and metadata by looking at children and preceding siblings
                    let mut cursor = node.walk();
                    for child in node.children(&mut cursor) {
                        if child.kind() == "visibility_modifier" {
                            is_public = true;
                        }
                        if child.kind() == "abi"
                            && let Ok(abi_text) = child.utf8_text(content.as_bytes())
                        {
                            metadata.insert("abi".to_string(), abi_text.to_string());
                        }
                    }

                    // Check preceding siblings for attributes
                    if let Some(parent) = node.parent() {
                        let mut pcursor = parent.walk();
                        let siblings: Vec<tree_sitter::Node> =
                            parent.children(&mut pcursor).collect();
                        if let Some(idx) = siblings.iter().position(|s| *s == node) {
                            // Accumulate derived trait names across all preceding
                            // `#[derive(...)]` attributes (there may be several).
                            // We union them and store sorted/deduped into
                            // `metadata["derived_traits"]` only when non-empty.
                            //
                            // DX4 design note: the plan literally says "Add a
                            // `derived_traits: Vec<String>` field to the parsed
                            // symbol metadata." We deliberately do NOT add a new
                            // field to `Symbol` — that would require a
                            // `project_symbols` DB schema change + migration +
                            // touching every language extractor's constructors.
                            // Instead we reuse the existing `metadata`
                            // BTreeMap (already used by `cfg`/`macro`/`abi`/
                            // `reexport` and persisted as a JSON string column),
                            // storing a deterministic comma-joined string. This
                            // is the faithful, lower-risk realization of the
                            // plan's intent and keeps read-back via
                            // `symbol.metadata.get("derived_traits")` trivial.
                            let mut derived: Vec<String> = Vec::new();
                            for i in (0..idx).rev() {
                                let sibling = siblings[i];
                                if sibling.kind() == "attribute_item" {
                                    if let Ok(attr_text) = sibling.utf8_text(content.as_bytes()) {
                                        if attr_text.contains("#[cfg(") {
                                            metadata
                                                .insert("cfg".to_string(), attr_text.to_string());
                                        }
                                        if attr_text.contains("proc_macro") {
                                            metadata.insert(
                                                "macro".to_string(),
                                                "proc_macro".to_string(),
                                            );
                                        }
                                        if attr_text.contains("#[derive(") {
                                            derived.extend(parse_derive_traits(attr_text));
                                        } else if attr_text.contains("cfg_attr")
                                            && attr_text.contains("derive(")
                                        {
                                            // DX4 (codex Finding 2): capture
                                            // `derive(...)` nested inside a
                                            // `#[cfg_attr(..., derive(...))]`
                                            // attribute. `parse_derive_traits`
                                            // locates the first `derive(`
                                            // substring and matches parens by
                                            // depth, so it extracts the inner
                                            // derive list correctly. The
                                            // `else if` ensures a plain
                                            // `#[derive(...)]` (no `cfg_attr`)
                                            // only runs the original path and
                                            // is not double-counted.
                                            derived.extend(parse_derive_traits(attr_text));
                                        }
                                    }
                                } else if sibling.kind() != "line_comment"
                                    && sibling.kind() != "block_comment"
                                {
                                    break;
                                }
                            }
                            if !derived.is_empty() {
                                // Sort + dedupe for deterministic storage so the
                                // scorer's penalty is stable across runs and
                                // index rebuilds.
                                derived.sort();
                                derived.dedup();
                                metadata.insert("derived_traits".to_string(), derived.join(","));
                            }
                        }
                    }
                }
                _ => {}
            }
        }

        if !skip && let Some(node) = symbol_node {
            let byte_start = Some(node.start_byte() as i32);
            let byte_end = Some(node.end_byte() as i32);
            let line_start = Some((node.start_position().row + 1) as i32);
            let line_end = Some((node.end_position().row + 1) as i32);

            if !name.is_empty() {
                symbols.push(Symbol {
                    name,
                    kind,
                    is_public,
                    cognitive_complexity: None,
                    cyclomatic_complexity: None,
                    line_start,
                    line_end,
                    qualified_name: None,
                    byte_start,
                    byte_end,
                    entrypoint_kind: None,
                    metadata,
                });
            }
        }
    }

    Ok(Some(symbols))
}

fn extract_use_name(node: tree_sitter::Node, content: &str) -> String {
    let mut last_ident = String::new();
    let mut stack = vec![node];
    while let Some(n) = stack.pop() {
        if n.kind() == "identifier" || n.kind() == "type_identifier" {
            last_ident = n.utf8_text(content.as_bytes()).unwrap_or("").to_string();
        }
        let mut c = n.walk();
        let children: Vec<_> = n.children(&mut c).collect();
        for child in children.into_iter().rev() {
            stack.push(child);
        }
    }
    last_ident
}

/// Extracts the trait identifiers from a `#[derive(...)]` attribute string.
///
/// Handles the common forms:
/// - `#[derive(Serialize, Deserialize)]`
/// - `#[derive(Debug)]`
///
/// For path-qualified derives such as `#[derive(serde::Serialize)]`, only the
/// last path segment (`Serialize`) is captured — this is the identifier that
/// matters for the implicit-usage heuristic (serde, Debug, etc. are recognized
/// by their final segment). Whitespace inside the parens is tolerated.
///
/// Trait names are Rust identifiers (ASCII alphanumerics + underscore, starting
/// with a non-digit). Anything else inside the parens (e.g. attributes nested
/// in derives via `#[derive(Debug)]` style) is skipped gracefully — we simply
/// don't emit a non-identifier token.
fn parse_derive_traits(attr_text: &str) -> Vec<String> {
    // Locate the first `#[derive(` ... matching close paren. The tree-sitter
    // `attribute_item` text is the full `#[derive(...)]`, possibly with nested
    // parens in rare cases. We scan to the first `derive(` and then take the
    // matching `)` using a depth counter so nested parens don't trip us.
    let key = "derive(";
    let Some(start) = attr_text.find(key) else {
        return Vec::new();
    };
    let body_start = start + key.len();
    let mut depth = 1usize;
    let mut body_end = attr_text.len();
    for (i, ch) in attr_text[body_start..].char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    body_end = body_start + i;
                    break;
                }
            }
            _ => {}
        }
    }
    let body = &attr_text[body_start..body_end];

    body.split(',')
        .map(|t| t.trim())
        .filter_map(|t| {
            // Strip any trailing nested attribute bracket group like
            // `Foo #[bar]` (rare in derives); take everything up to the
            // first whitespace/`#` and then take the last `::` segment.
            let token = t.split(|c: char| c.is_whitespace() || c == '#').next()?;
            let token = token.trim();
            if token.is_empty() {
                return None;
            }
            let last_segment = token
                .rsplit("::")
                .next()
                .filter(|s| !s.is_empty())
                .unwrap_or(token);
            if last_segment.is_empty() {
                return None;
            }
            // Validate it's a plausible Rust identifier (ASCII, starts with
            // non-digit). Non-ASCII / digits-only tokens are skipped rather
            // than recorded, keeping the heuristic conservative.
            let mut chars = last_segment.chars();
            let first = chars.next()?;
            if !(first.is_ascii_alphabetic() || first == '_') {
                return None;
            }
            if !last_segment
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_')
            {
                return None;
            }
            Some(last_segment.to_string())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_derive_traits_basic() {
        let got = parse_derive_traits("#[derive(Serialize, Deserialize, Debug)]");
        let mut want = vec!["Serialize", "Deserialize", "Debug"];
        want.sort();
        let mut got_sorted = got.clone();
        got_sorted.sort();
        assert_eq!(got_sorted, want);
    }

    #[test]
    fn test_parse_derive_traits_single() {
        let got = parse_derive_traits("#[derive(Debug)]");
        assert_eq!(got, vec!["Debug"]);
    }

    #[test]
    fn test_parse_derive_traits_path_qualified_takes_last_segment() {
        let got = parse_derive_traits("#[derive(serde::Serialize, serde::Deserialize)]");
        let mut want = vec!["Serialize", "Deserialize"];
        want.sort();
        let mut got_sorted = got.clone();
        got_sorted.sort();
        assert_eq!(got_sorted, want);
    }

    #[test]
    fn test_parse_derive_traits_no_derive() {
        assert!(parse_derive_traits("#[cfg(feature = \"x\")]").is_empty());
        assert!(parse_derive_traits("// comment").is_empty());
    }

    #[test]
    fn test_parse_derive_traits_tolerates_whitespace() {
        let got = parse_derive_traits("#[derive( Serialize ,  Debug  )]");
        let mut want = vec!["Serialize", "Debug"];
        want.sort();
        let mut got_sorted = got.clone();
        got_sorted.sort();
        assert_eq!(got_sorted, want);
    }

    #[test]
    fn test_extract_symbols_captures_derived_traits_on_struct() {
        let content = r#"
            #[derive(Serialize, Deserialize, Debug)]
            pub struct User {
                name: String,
            }
        "#;
        let symbols = extract_symbols(content).unwrap().unwrap();
        let user = symbols
            .iter()
            .find(|s| s.kind == SymbolKind::Struct && s.name == "User")
            .expect("User struct should be extracted");
        let derived = user
            .metadata
            .get("derived_traits")
            .expect("derived_traits metadata should be set");
        // Sort assertion: stored as sorted, deduped, comma-joined.
        assert_eq!(derived, "Debug,Deserialize,Serialize");
    }

    #[test]
    fn test_extract_symbols_captures_derived_traits_on_enum() {
        let content = r#"
            #[derive(Debug, Clone, Copy)]
            enum Shape {
                Circle,
                Square,
            }
        "#;
        let symbols = extract_symbols(content).unwrap().unwrap();
        let shape = symbols
            .iter()
            .find(|s| s.kind == SymbolKind::Enum && s.name == "Shape")
            .expect("Shape enum should be extracted");
        let derived = shape
            .metadata
            .get("derived_traits")
            .expect("derived_traits metadata should be set");
        assert_eq!(derived, "Clone,Copy,Debug");
    }

    #[test]
    fn test_extract_symbols_unions_multiple_derive_attrs() {
        let content = r#"
            #[derive(Debug)]
            #[derive(Serialize, Deserialize)]
            struct Packet {
                id: u64,
            }
        "#;
        let symbols = extract_symbols(content).unwrap().unwrap();
        let packet = symbols
            .iter()
            .find(|s| s.kind == SymbolKind::Struct && s.name == "Packet")
            .expect("Packet struct should be extracted");
        let derived = packet.metadata.get("derived_traits").unwrap();
        assert_eq!(derived, "Debug,Deserialize,Serialize");
    }

    #[test]
    fn test_extract_symbols_no_derived_traits_key_without_derive() {
        let content = r#"
            struct Plain {
                x: i32,
            }
        "#;
        let symbols = extract_symbols(content).unwrap().unwrap();
        let plain = symbols
            .iter()
            .find(|s| s.kind == SymbolKind::Struct && s.name == "Plain")
            .expect("Plain struct should be extracted");
        assert!(
            !plain.metadata.contains_key("derived_traits"),
            "derived_traits must NOT be set when there is no #[derive(...)]"
        );
    }

    #[test]
    fn test_parse_derive_traits_empty_yields_no_key() {
        // `#[derive()]` with no traits returns an empty vec; the extractor
        // only writes the `derived_traits` metadata key when non-empty, so
        // pinning this keeps the "no key set" invariant locked.
        let got = parse_derive_traits("#[derive()]");
        assert!(got.is_empty(), "empty derive must yield no traits");
    }

    #[test]
    fn test_parse_derive_traits_mixed_path_qualified_and_plain_sorted() {
        // Mixed path-qualified + plain traits: path-qualified takes the last
        // `::` segment, plain passes through. Result is sorted/deduped by the
        // caller, so we assert the sorted set here.
        let got = parse_derive_traits("#[derive(serde::Serialize, Clone)]");
        let mut got_sorted = got.clone();
        got_sorted.sort();
        assert_eq!(got_sorted, vec!["Clone", "Serialize"]);
    }

    #[test]
    fn test_extract_symbols_path_qualified_derive_captures_last_segment() {
        let content = r#"
            #[derive(serde::Serialize, serde::Deserialize)]
            struct Doc {
                title: String,
            }
        "#;
        let symbols = extract_symbols(content).unwrap().unwrap();
        let doc = symbols
            .iter()
            .find(|s| s.kind == SymbolKind::Struct && s.name == "Doc")
            .expect("Doc struct should be extracted");
        let derived = doc.metadata.get("derived_traits").unwrap();
        assert_eq!(derived, "Deserialize,Serialize");
    }

    #[test]
    fn test_extract_symbols_captures_cfg_attr_gated_derive() {
        // DX4 (codex Finding 2): `derive(...)` nested inside
        // `#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]`
        // must be captured. The trait list is sorted/deduped by the
        // extractor, so we assert the canonical form.
        let content = r#"
            #[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
            struct Config {
                name: String,
            }
        "#;
        let symbols = extract_symbols(content).unwrap().unwrap();
        let config = symbols
            .iter()
            .find(|s| s.kind == SymbolKind::Struct && s.name == "Config")
            .expect("Config struct should be extracted");
        let derived = config
            .metadata
            .get("derived_traits")
            .expect("derived_traits metadata should be set from cfg_attr-gated derive");
        assert_eq!(derived, "Deserialize,Serialize");
    }

    #[test]
    fn test_extract_symbols_cfg_attr_without_derive_sets_no_derived_traits() {
        // DX4 (codex Finding 2): a `cfg_attr` (or `cfg`) attribute with no
        // `derive(...)` must NOT set `derived_traits`. The trigger requires
        // both `cfg_attr` AND `derive(`, so this must stay inert.
        let content = r#"
            #[cfg_attr(feature = "serde", ignore)]
            struct MaybeConfig {
                name: String,
            }
        "#;
        let symbols = extract_symbols(content).unwrap().unwrap();
        let config = symbols
            .iter()
            .find(|s| s.kind == SymbolKind::Struct && s.name == "MaybeConfig")
            .expect("MaybeConfig struct should be extracted");
        assert!(
            !config.metadata.contains_key("derived_traits"),
            "cfg_attr without derive(...) must not set derived_traits"
        );
    }
}
