use crate::index::signature::{
    SignatureParam, SymbolSignatureParts, build_symbol_signature, write_signature_metadata,
};
use crate::index::symbols::{Symbol, SymbolKind};
use miette::{IntoDiagnostic, Result};
use std::collections::BTreeMap;
use tree_sitter::{Node, Parser, Query, QueryCursor, StreamingIterator};

pub fn extract_symbols(content: &str) -> Result<Option<Vec<Symbol>>> {
    let mut parser = Parser::new();
    let language = tree_sitter_rust::LANGUAGE;
    parser.set_language(&language.into()).into_diagnostic()?;

    let tree = parser
        .parse(content, None)
        .ok_or_else(|| miette::miette!("Failed to parse Rust content"))?;

    // function_signature_item = trait method declarations (no body) — §2.8b coverage.
    let query_str = r#"
        (function_item name: (identifier) @name) @symbol
        (function_signature_item name: (identifier) @name) @symbol
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
                        // Trait method declarations (no default body). Same fields as
                        // function_item; classified as Method so contract surfaces are
                        // distinguishable from free functions.
                        "function_signature_item" => kind = SymbolKind::Method,
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

            // Populate signature metadata for free functions and trait method decls.
            if matches!(node.kind(), "function_item" | "function_signature_item")
                && !name.is_empty()
                && let Some(sig) = extract_rust_signature(node, content, &name)
            {
                write_signature_metadata(&mut metadata, &sig);
            }

            if !name.is_empty() {
                // Qualify methods so same-name symbols in one file do not collide:
                // - trait method decls → TraitName.method
                // - impl methods → TypeName.method (matches Go Receiver.method)
                // Free functions keep qualified_name: None.
                let qualified_name = match node.kind() {
                    "function_signature_item" => qualify_trait_method(node, content, &name),
                    "function_item" => qualify_impl_method(node, content, &name),
                    _ => None,
                };

                symbols.push(Symbol {
                    name,
                    kind,
                    is_public,
                    cognitive_complexity: None,
                    cyclomatic_complexity: None,
                    line_start,
                    line_end,
                    qualified_name,
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

/// Extract a normalized signature from a Rust `function_item` or `function_signature_item`.
fn extract_rust_signature(
    node: Node<'_>,
    content: &str,
    name: &str,
) -> Option<crate::index::signature::SymbolSignature> {
    let modifiers = extract_function_modifiers(node, content);
    let params = extract_rust_params(node, content);
    let return_type = node
        .child_by_field_name("return_type")
        .and_then(|n| node_text_owned(n, content))
        .map(|s| s.trim().trim_start_matches("->").trim().to_string())
        .filter(|s| !s.is_empty());

    let parts = SymbolSignatureParts {
        name: name.to_string(),
        modifiers,
        params,
        return_type,
    };
    Some(build_symbol_signature(&parts))
}

fn extract_function_modifiers(node: Node<'_>, content: &str) -> Vec<String> {
    let mut mods = Vec::new();
    // `function_modifiers` is a named child on both function_item and function_signature_item.
    if let Some(mod_node) = node.child_by_field_name("function_modifiers").or_else(|| {
        let mut c = node.walk();
        node.children(&mut c)
            .find(|ch| ch.kind() == "function_modifiers")
    }) {
        let mut c = mod_node.walk();
        for child in mod_node.children(&mut c) {
            match child.kind() {
                "async" | "const" | "unsafe" => {
                    if let Some(t) = node_text_owned(child, content) {
                        mods.push(t);
                    }
                }
                "extern_modifier" | "abi" => {
                    if let Some(t) = node_text_owned(child, content) {
                        // Collapse whitespace for stable shape text.
                        mods.push(t.split_whitespace().collect::<Vec<_>>().join(" "));
                    }
                }
                _ => {}
            }
        }
    } else {
        // Fallback: scan direct children for modifier keywords (some grammar layouts).
        let mut c = node.walk();
        for child in node.children(&mut c) {
            match child.kind() {
                "async" | "const" | "unsafe" => {
                    if let Some(t) = node_text_owned(child, content) {
                        mods.push(t);
                    }
                }
                _ => {}
            }
        }
    }
    mods
}

fn extract_rust_params(node: Node<'_>, content: &str) -> Vec<SignatureParam> {
    let Some(params_node) = node.child_by_field_name("parameters") else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut c = params_node.walk();
    for child in params_node.children(&mut c) {
        match child.kind() {
            "parameter" => {
                let name = child
                    .child_by_field_name("pattern")
                    .and_then(|p| first_identifier(p, content));
                let type_text = child
                    .child_by_field_name("type")
                    .and_then(|t| node_text_owned(t, content));
                out.push(SignatureParam { name, type_text });
            }
            "self_parameter" => {
                let text = node_text_owned(child, content);
                out.push(SignatureParam {
                    name: Some("self".to_string()),
                    type_text: text,
                });
            }
            "variadic_parameter" => {
                out.push(SignatureParam {
                    name: None,
                    type_text: node_text_owned(child, content),
                });
            }
            _ => {}
        }
    }
    out
}

fn first_identifier(node: Node<'_>, content: &str) -> Option<String> {
    if matches!(node.kind(), "identifier" | "type_identifier") {
        return node_text_owned(node, content);
    }
    let mut c = node.walk();
    for child in node.children(&mut c) {
        if let Some(id) = first_identifier(child, content) {
            return Some(id);
        }
    }
    None
}

fn node_text_owned(node: Node<'_>, content: &str) -> Option<String> {
    node.utf8_text(content.as_bytes())
        .ok()
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty())
}

/// `TraitName.method` when the signature item sits inside a `trait_item`.
///
/// Nested local `fn`s inside a default trait method body are **not**
/// qualified (stop at an intervening `function_item`).
fn qualify_trait_method(node: Node<'_>, content: &str, method_name: &str) -> Option<String> {
    let mut current = node.parent();
    while let Some(n) = current {
        if n.kind() == "trait_item" {
            let trait_name = n
                .child_by_field_name("name")
                .and_then(|name_node| node_text_owned(name_node, content))?;
            return Some(format!("{trait_name}.{method_name}"));
        }
        // Nested local function inside a method body — not a trait method.
        if matches!(n.kind(), "function_item" | "function_signature_item") {
            return None;
        }
        if matches!(n.kind(), "source_file" | "mod_item") {
            return None;
        }
        current = n.parent();
    }
    None
}

/// `TypeName.method` when a `function_item` is a **direct method** of an
/// `impl_item` (not a nested local function inside a method body).
///
/// Uses the impl's `type` field (not the first `type_identifier`) so
/// `impl Display for Foo` qualifies as `Foo.method`, not `Display.method`.
/// Free functions and nested locals return `None`.
///
/// Codex 0088 P2: walking past an intervening `function_item` mislabeled
/// `impl Foo { fn bar() { fn helper() {} } }` as `Foo.helper`.
fn qualify_impl_method(node: Node<'_>, content: &str, method_name: &str) -> Option<String> {
    let mut current = node.parent();
    while let Some(n) = current {
        if n.kind() == "impl_item" {
            let type_node = n.child_by_field_name("type")?;
            let type_name = first_type_identifier(type_node, content)?;
            return Some(format!("{type_name}.{method_name}"));
        }
        // Nested local `fn` inside a method (or free function) body: stop.
        // Parent chain for `helper` is block → function_item(method) → … →
        // impl_item; without this guard we would emit `TypeName.helper`.
        if matches!(n.kind(), "function_item" | "function_signature_item") {
            return None;
        }
        // Stop at item containers so we do not climb into an outer impl for a
        // free function nested via macros or other odd structures.
        if matches!(n.kind(), "source_file" | "mod_item" | "trait_item") {
            return None;
        }
        current = n.parent();
    }
    None
}

/// First `type_identifier` under a type node (handles plain and generic types).
fn first_type_identifier(node: Node<'_>, content: &str) -> Option<String> {
    if node.kind() == "type_identifier" {
        return node_text_owned(node, content);
    }
    let mut c = node.walk();
    for child in node.children(&mut c) {
        if let Some(id) = first_type_identifier(child, content) {
            return Some(id);
        }
    }
    None
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

    #[test]
    fn extracts_function_signature_metadata() {
        let content = r#"
            pub fn greet(name: String, age: u32) -> bool {
                true
            }
        "#;
        let symbols = extract_symbols(content).unwrap().unwrap();
        let greet = symbols
            .iter()
            .find(|s| s.name == "greet" && s.kind == SymbolKind::Function)
            .expect("greet");
        let sig = greet.metadata.get("signature").expect("signature");
        let shape = greet
            .metadata
            .get("signatureShape")
            .expect("signatureShape");
        assert!(sig.contains("name: String"), "readable: {sig}");
        assert!(sig.contains("age: u32"), "readable: {sig}");
        assert!(sig.contains("-> bool"), "readable: {sig}");
        assert!(
            !shape.contains("name") && !shape.contains("age"),
            "shape excludes names: {shape}"
        );
        assert!(shape.contains("params=String,u32"), "shape: {shape}");
        assert!(shape.contains("ret=bool"), "shape: {shape}");
    }

    #[test]
    fn async_fn_changes_signature_shape() {
        let sync_src = "fn foo() {}";
        let async_src = "async fn foo() {}";
        let sync_sym = extract_symbols(sync_src).unwrap().unwrap();
        let async_sym = extract_symbols(async_src).unwrap().unwrap();
        let s_shape = sync_sym
            .iter()
            .find(|s| s.name == "foo")
            .and_then(|s| s.metadata.get("signatureShape"))
            .expect("sync shape");
        let a_shape = async_sym
            .iter()
            .find(|s| s.name == "foo")
            .and_then(|s| s.metadata.get("signatureShape"))
            .expect("async shape");
        assert_ne!(s_shape, a_shape, "async must move shape");
        assert!(a_shape.contains("async"), "async shape: {a_shape}");
    }

    #[test]
    fn trait_method_signature_item_is_indexed() {
        let content = r#"
            pub trait Reader {
                fn read(&self, buf: &mut [u8]) -> Result<usize, std::io::Error>;
                fn name(&self) -> &str;
            }
        "#;
        let symbols = extract_symbols(content).unwrap().unwrap();
        let read = symbols
            .iter()
            .find(|s| s.name == "read" && s.kind == SymbolKind::Method)
            .expect("trait method read must be indexed as Method");
        assert_eq!(read.qualified_name.as_deref(), Some("Reader.read"));
        assert!(
            read.metadata.contains_key("signature"),
            "trait method must carry signature metadata"
        );
        assert!(
            read.metadata.contains_key("signatureShape"),
            "trait method must carry signatureShape"
        );
        let name_m = symbols
            .iter()
            .find(|s| s.name == "name" && s.kind == SymbolKind::Method)
            .expect("trait method name");
        assert_eq!(name_m.qualified_name.as_deref(), Some("Reader.name"));
    }

    #[test]
    fn signature_hash_non_null_via_project_symbol() {
        use crate::index::types::symbol_to_project_symbol;
        let content = "pub fn add(a: i32, b: i32) -> i32 { a + b }";
        let symbols = extract_symbols(content).unwrap().unwrap();
        let add = symbols.iter().find(|s| s.name == "add").expect("add");
        let ps = symbol_to_project_symbol(add, 1, "now");
        assert!(
            ps.signature_hash.is_some(),
            "Rust function must yield non-null signature_hash"
        );
    }

    #[test]
    fn impl_methods_are_qualified_by_type_name() {
        let content = r#"
            struct Foo;
            struct Bar;
            impl Foo {
                fn new() -> Self { Foo }
            }
            impl Bar {
                fn new() -> Self { Bar }
            }
            impl std::fmt::Display for Foo {
                fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                    Ok(())
                }
            }
            fn free_new() {}
        "#;
        let symbols = extract_symbols(content).unwrap().unwrap();
        let foo_new = symbols
            .iter()
            .find(|s| s.name == "new" && s.qualified_name.as_deref() == Some("Foo.new"))
            .expect("Foo.new must be qualified");
        let bar_new = symbols
            .iter()
            .find(|s| s.name == "new" && s.qualified_name.as_deref() == Some("Bar.new"))
            .expect("Bar.new must be qualified");
        assert!(foo_new.metadata.contains_key("signatureShape"));
        assert!(bar_new.metadata.contains_key("signatureShape"));
        // Trait impl uses the `type` field (Foo), not the trait name (Display).
        let fmt = symbols
            .iter()
            .find(|s| s.name == "fmt")
            .expect("fmt method");
        assert_eq!(fmt.qualified_name.as_deref(), Some("Foo.fmt"));
        // Free functions stay unqualified.
        let free = symbols
            .iter()
            .find(|s| s.name == "free_new")
            .expect("free_new");
        assert_eq!(free.qualified_name, None);
    }

    /// Codex 0088 P2: nested local `fn` inside an impl method must NOT be
    /// qualified as `TypeName.helper` (only direct impl methods qualify).
    #[test]
    fn nested_local_fn_inside_impl_method_is_not_qualified() {
        let content = r#"
            struct Foo;
            impl Foo {
                fn bar(&self) {
                    fn helper(x: u32) -> u32 { x }
                    let _ = helper(1);
                }
            }
        "#;
        let symbols = extract_symbols(content).unwrap().unwrap();
        let bar = symbols
            .iter()
            .find(|s| s.name == "bar")
            .expect("impl method bar");
        assert_eq!(bar.qualified_name.as_deref(), Some("Foo.bar"));
        let helper = symbols
            .iter()
            .find(|s| s.name == "helper")
            .expect("nested local helper must still be indexed");
        assert_eq!(
            helper.qualified_name, None,
            "nested local fn must not be Foo.helper; got {:?}",
            helper.qualified_name
        );
        // Still gets signature metadata for its own shape.
        assert!(helper.metadata.contains_key("signatureShape"));
    }

    /// Phase 0 P1/P3: confirm field names against the pinned tree-sitter-rust grammar.
    #[test]
    fn phase0_rust_definition_node_fields() {
        let content = r#"
            async fn foo(a: u32) -> u64 { a as u64 }
            trait T { fn bar(&self) -> i32; }
        "#;
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_rust::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(content, None).unwrap();
        let root = tree.root_node();

        fn find_kind<'a>(node: tree_sitter::Node<'a>, kind: &str) -> Option<tree_sitter::Node<'a>> {
            if node.kind() == kind {
                return Some(node);
            }
            let mut c = node.walk();
            for child in node.children(&mut c) {
                if let Some(found) = find_kind(child, kind) {
                    return Some(found);
                }
            }
            None
        }

        let fi = find_kind(root, "function_item").expect("function_item");
        assert!(
            fi.child_by_field_name("parameters").is_some(),
            "function_item.parameters"
        );
        assert!(
            fi.child_by_field_name("return_type").is_some(),
            "function_item.return_type"
        );
        // function_modifiers may be a field or a child node depending on grammar layout
        let has_async = {
            let mut c = fi.walk();
            fi.children(&mut c).any(|ch| {
                ch.kind() == "function_modifiers"
                    || ch.kind() == "async"
                    || ch
                        .utf8_text(content.as_bytes())
                        .unwrap_or("")
                        .contains("async")
            })
        };
        assert!(has_async, "async modifier present on function_item");

        let fsi = find_kind(root, "function_signature_item").expect("function_signature_item");
        assert!(
            fsi.child_by_field_name("parameters").is_some(),
            "function_signature_item.parameters"
        );
        assert!(
            fsi.child_by_field_name("return_type").is_some(),
            "function_signature_item.return_type"
        );
    }
}
