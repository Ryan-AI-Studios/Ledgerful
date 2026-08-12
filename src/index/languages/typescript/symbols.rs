use crate::index::signature::{
    SignatureParam, SymbolSignatureParts, build_symbol_signature, write_signature_metadata,
};
use crate::index::symbols::{Symbol, SymbolKind};
use miette::{IntoDiagnostic, Result};
use std::path::Path;
use tree_sitter::{Node, Parser, Query, QueryCursor, StreamingIterator};

/// Extract TypeScript/JavaScript symbols.
///
/// `path` is optional. When present:
/// - file stem drives default-export arrow QN (`{stem}.default`)
/// - `.tsx`/`.jsx` select `LANGUAGE_TSX` (JSX-aware grammar); other extensions
///   use `LANGUAGE_TYPESCRIPT`
pub fn extract_symbols(content: &str, path: Option<&Path>) -> Result<Option<Vec<Symbol>>> {
    let mut parser = Parser::new();
    let language: tree_sitter::Language = select_ts_language(path);
    parser.set_language(&language).into_diagnostic()?;

    let tree = parser
        .parse(content, None)
        .ok_or_else(|| miette::miette!("Failed to parse TypeScript content"))?;

    let query_str = r#"
        (function_declaration name: (identifier) @name) @symbol
        (class_declaration name: (type_identifier) @name) @symbol
        (abstract_class_declaration name: (type_identifier) @name) @symbol
        (interface_declaration name: (type_identifier) @name) @symbol
        (type_alias_declaration name: (type_identifier) @name) @symbol
        (enum_declaration name: (identifier) @name) @symbol
        (method_definition name: (property_identifier) @name) @symbol
        (method_signature name: (property_identifier) @name) @symbol
        (abstract_method_signature name: (property_identifier) @name) @symbol
        (function_signature name: (identifier) @name) @symbol
        (arrow_function) @arrow
    "#;

    let query = Query::new(&language, query_str).into_diagnostic()?;
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&query, tree.root_node(), content.as_bytes());

    let mut symbols = Vec::new();

    while let Some(m) = matches.next() {
        let mut name = String::new();
        let mut is_public = false;
        let mut kind = SymbolKind::Function;
        let mut symbol_node: Option<Node<'_>> = None;
        let mut is_arrow = false;

        for capture in m.captures {
            let capture_name = query.capture_names()[capture.index as usize];
            if capture_name == "name" {
                name = capture
                    .node
                    .utf8_text(content.as_bytes())
                    .into_diagnostic()?
                    .to_string();
            } else if capture_name == "arrow" {
                is_arrow = true;
                symbol_node = Some(capture.node);
                kind = SymbolKind::Function;
            } else if capture_name == "symbol" {
                let node = capture.node;
                symbol_node = Some(node);
                match node.kind() {
                    "function_declaration" | "function_signature" => kind = SymbolKind::Function,
                    "class_declaration" | "abstract_class_declaration" => kind = SymbolKind::Class,
                    "interface_declaration" => kind = SymbolKind::Interface,
                    "type_alias_declaration" => kind = SymbolKind::Type,
                    "enum_declaration" => kind = SymbolKind::Enum,
                    "method_definition" | "method_signature" | "abstract_method_signature" => {
                        kind = SymbolKind::Method
                    }
                    _ => {}
                }

                // Check if exported (functions/classes/etc.)
                if let Some(parent) = node.parent()
                    && parent.kind() == "export_statement"
                {
                    is_public = true;
                }
            }
        }

        // Arrow functions: resolve name from host or skip.
        let mut qualified_name = None;
        let mut arrow_host_for_mods: Option<Node<'_>> = None;
        if is_arrow {
            let Some(node) = symbol_node else {
                continue;
            };
            match resolve_arrow_host(node, content, path) {
                Some(ArrowHost {
                    name: host_name,
                    qualified_name: host_qn,
                    is_public: host_pub,
                    kind: host_kind,
                    host_node_kind,
                    host_byte_start,
                }) => {
                    name = host_name;
                    qualified_name = host_qn;
                    is_public = host_pub;
                    kind = host_kind;
                    // Recover host node for field-level modifiers (private/static/readonly).
                    if host_node_kind == Some("public_field_definition")
                        && let Some(start) = host_byte_start
                    {
                        arrow_host_for_mods =
                            find_ancestor_at_byte(node, "public_field_definition", start);
                    }
                }
                None => continue, // anonymous arrow — emit no symbol
            }
        }

        if name.is_empty() {
            continue;
        }

        // Methods (incl. interface/abstract): qualify as Owner.method.
        if kind == SymbolKind::Method && !is_arrow {
            let Some(node) = symbol_node else {
                continue;
            };
            if let Some(owner_name) = enclosing_class_name(node, content) {
                qualified_name = Some(format!("{owner_name}.{name}"));
                // Method visibility: exported class/interface ⇒ treat methods as public.
                if class_is_exported(node) {
                    is_public = true;
                }
            }
            // else: no enclosing owner — keep Method, qualified_name: None
        }

        // Free functions keep qualified_name: None (unless arrow set it).
        if kind == SymbolKind::Function && !is_arrow {
            qualified_name = None;
        }

        let (byte_start, byte_end, line_start, line_end) = if let Some(node) = symbol_node {
            (
                Some(node.start_byte() as i32),
                Some(node.end_byte() as i32),
                Some((node.start_position().row + 1) as i32),
                Some((node.end_position().row + 1) as i32),
            )
        } else {
            (None, None, None, None)
        };

        let mut metadata = std::collections::BTreeMap::new();
        if matches!(kind, SymbolKind::Function | SymbolKind::Method)
            && let Some(node) = symbol_node
            && let Some(sig) =
                extract_typescript_signature(node, content, &name, arrow_host_for_mods)
        {
            write_signature_metadata(&mut metadata, &sig);
        }

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

    // Deterministic order
    symbols.sort_by(|a, b| {
        a.name
            .cmp(&b.name)
            .then(a.kind.as_str().cmp(b.kind.as_str()))
            .then(
                a.qualified_name
                    .as_deref()
                    .unwrap_or("")
                    .cmp(b.qualified_name.as_deref().unwrap_or("")),
            )
    });

    Ok(Some(symbols))
}

fn select_ts_language(path: Option<&Path>) -> tree_sitter::Language {
    match path.and_then(|p| p.extension()).and_then(|e| e.to_str()) {
        Some("tsx") | Some("jsx") => tree_sitter_typescript::LANGUAGE_TSX.into(),
        _ => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
    }
}

struct ArrowHost {
    name: String,
    qualified_name: Option<String>,
    is_public: bool,
    kind: SymbolKind,
    /// Host AST node when modifiers live on the host (e.g. `public_field_definition`).
    host_node_kind: Option<&'static str>,
    host_byte_start: Option<usize>,
}

/// Walk UP from an `arrow_function` to a naming host.
///
/// Hosts (spec §2.6):
/// 1. `variable_declarator` with `name` = identifier (not destructuring)
/// 2. `public_field_definition` with `name` = property_identifier → Class.name
/// 3. `export_statement` with value = arrow → `{file_stem}.default`
///
/// The arrow must be the **direct** value of the host after unwrapping pure
/// expression wrappers (`parenthesized_expression`, `as_expression`, …).
/// Subtree containment is **not** enough: nested callbacks such as
/// `const load = async () => { items.map(x => x.id); }` must not inherit
/// the outer name `load` (DoD-4 / R1).
///
/// No host / destructuring → `None` (emit no symbol).
fn resolve_arrow_host(
    arrow_node: Node<'_>,
    content: &str,
    path: Option<&Path>,
) -> Option<ArrowHost> {
    let mut current = arrow_node.parent();
    while let Some(parent) = current {
        match parent.kind() {
            "variable_declarator" => {
                let Some(value) = parent.child_by_field_name("value") else {
                    current = parent.parent();
                    continue;
                };
                if !is_direct_expression_value(value, arrow_node) {
                    // Outer declarator whose value *contains* this arrow (e.g.
                    // nested callback) is not a naming host for this arrow.
                    current = parent.parent();
                    continue;
                }
                let name_node = parent.child_by_field_name("name")?;
                if name_node.kind() != "identifier" {
                    // Destructuring host — no single name.
                    return None;
                }
                let name = node_text_owned(name_node, content)?;
                let is_public = is_exported_ancestor(parent);
                return Some(ArrowHost {
                    name,
                    qualified_name: None,
                    is_public,
                    kind: SymbolKind::Function,
                    host_node_kind: Some("variable_declarator"),
                    host_byte_start: Some(parent.start_byte()),
                });
            }
            "public_field_definition" => {
                let Some(value) = parent.child_by_field_name("value") else {
                    current = parent.parent();
                    continue;
                };
                if !is_direct_expression_value(value, arrow_node) {
                    current = parent.parent();
                    continue;
                }
                let name_node = parent.child_by_field_name("name")?;
                if name_node.kind() != "property_identifier" {
                    return None;
                }
                let field_name = node_text_owned(name_node, content)?;
                let owner = enclosing_class_name(parent, content);
                let qualified_name = owner.map(|o| format!("{o}.{field_name}"));
                let is_public = class_is_exported(parent);
                return Some(ArrowHost {
                    name: field_name,
                    qualified_name,
                    is_public,
                    kind: SymbolKind::Method,
                    host_node_kind: Some("public_field_definition"),
                    host_byte_start: Some(parent.start_byte()),
                });
            }
            "export_statement" => {
                // export default (a) => {}  — value field holds the arrow.
                if let Some(value) = parent.child_by_field_name("value")
                    && is_direct_expression_value(value, arrow_node)
                {
                    // Module-derived QN from full path (dirs + stem), not bare stem:
                    // `src/foo/index.ts` and `src/bar/index.ts` must not both be
                    // `index.default` (codex R2 P2).
                    let qn = default_export_qualified_name(path)?;
                    return Some(ArrowHost {
                        name: "default".to_string(),
                        qualified_name: Some(qn),
                        is_public: true,
                        kind: SymbolKind::Function,
                        host_node_kind: Some("export_statement"),
                        host_byte_start: Some(parent.start_byte()),
                    });
                }
            }
            _ => {}
        }
        current = parent.parent();
    }
    None
}

/// True when `target` is `expr` after stripping pure TypeScript expression
/// wrappers (not statement/body containers). Nested arrows inside a body fail.
fn is_direct_expression_value(expr: Node<'_>, target: Node<'_>) -> bool {
    let mut cur = expr;
    loop {
        if cur.id() == target.id() {
            return true;
        }
        match cur.kind() {
            "parenthesized_expression"
            | "as_expression"
            | "type_assertion"
            | "non_null_expression"
            | "satisfies_expression"
            | "await_expression"
            | "ts_non_null_expression" => {
                // Prefer field `expression` when present; else first named child.
                if let Some(inner) = cur
                    .child_by_field_name("expression")
                    .or_else(|| cur.child_by_field_name("value"))
                {
                    cur = inner;
                    continue;
                }
                let mut walk = cur.walk();
                let next = cur
                    .named_children(&mut walk)
                    .find(|c| c.kind() != "type_annotation" && c.kind() != "type_arguments");
                if let Some(inner) = next {
                    cur = inner;
                    continue;
                }
                return false;
            }
            _ => return false,
        }
    }
}

fn is_exported_ancestor(node: Node<'_>) -> bool {
    let mut current = Some(node);
    while let Some(n) = current {
        if n.kind() == "export_statement" {
            return true;
        }
        // lexical_declaration / variable_declaration wrap the declarator.
        current = n.parent();
        if current.map(|p| p.kind()) == Some("program")
            || current.map(|p| p.kind()) == Some("source_file")
        {
            break;
        }
    }
    false
}

/// Owner name for methods: class, abstract class, or interface.
fn enclosing_class_name(method_node: Node<'_>, content: &str) -> Option<String> {
    let mut current = method_node.parent();
    while let Some(parent) = current {
        match parent.kind() {
            "class_declaration" | "abstract_class_declaration" | "interface_declaration" => {
                if let Some(name_node) = parent.child_by_field_name("name") {
                    let n = name_node.utf8_text(content.as_bytes()).ok()?.to_string();
                    if !n.is_empty() {
                        return Some(n);
                    }
                }
                let mut cursor = parent.walk();
                for child in parent.children(&mut cursor) {
                    if child.kind() == "type_identifier" || child.kind() == "identifier" {
                        let n = child.utf8_text(content.as_bytes()).ok()?.to_string();
                        if !n.is_empty() {
                            return Some(n);
                        }
                    }
                }
                return None;
            }
            _ => {}
        }
        current = parent.parent();
    }
    None
}

fn class_is_exported(method_node: Node<'_>) -> bool {
    let mut current = method_node.parent();
    while let Some(parent) = current {
        if matches!(
            parent.kind(),
            "class_declaration" | "abstract_class_declaration" | "interface_declaration"
        ) {
            if let Some(gp) = parent.parent() {
                return gp.kind() == "export_statement";
            }
            return false;
        }
        current = parent.parent();
    }
    false
}

/// Extract a normalized signature from a TS function/method/arrow node.
///
/// `host_for_modifiers`: when the symbol is a class-field arrow, field-level
/// modifiers (`private`/`public`/`protected`/`static`/`readonly`) live on the
/// `public_field_definition`, not on the `arrow_function` itself (codex R1 P2).
fn extract_typescript_signature(
    node: Node<'_>,
    content: &str,
    name: &str,
    host_for_modifiers: Option<Node<'_>>,
) -> Option<crate::index::signature::SymbolSignature> {
    let mut modifiers = extract_ts_modifiers(node, content);
    if let Some(host) = host_for_modifiers {
        let host_mods = extract_ts_modifiers(host, content);
        // Host modifiers first (declaration order on the field), then arrow-local.
        for m in host_mods.into_iter().rev() {
            if !modifiers.iter().any(|x| x == &m) {
                modifiers.insert(0, m);
            }
        }
    }
    let params = extract_ts_params(node, content);
    let return_type = extract_ts_return_type(node, content);

    let parts = SymbolSignatureParts {
        name: name.to_string(),
        modifiers,
        params,
        return_type,
    };
    Some(build_symbol_signature(&parts))
}

fn find_ancestor_at_byte<'a>(start: Node<'a>, kind: &str, byte_start: usize) -> Option<Node<'a>> {
    let mut current = Some(start);
    while let Some(n) = current {
        if n.kind() == kind && n.start_byte() == byte_start {
            return Some(n);
        }
        current = n.parent();
    }
    None
}

/// `src/foo/index.ts` → `src.foo.index.default` (path separators → dots, no ext).
fn default_export_qualified_name(path: Option<&Path>) -> Option<String> {
    let path = path?;
    let mut parts: Vec<String> = Vec::new();
    for comp in path.components() {
        if let std::path::Component::Normal(s) = comp {
            let s = s.to_str()?.to_string();
            if !s.is_empty() {
                parts.push(s);
            }
        }
    }
    if parts.is_empty() {
        return None;
    }
    // Strip extension from the last component.
    if let Some(last) = parts.last_mut()
        && let Some(stem) = Path::new(last.as_str())
            .file_stem()
            .and_then(|s| s.to_str())
    {
        *last = stem.to_string();
    }
    if parts.iter().any(|p| p.is_empty()) {
        return None;
    }
    Some(format!("{}.default", parts.join(".")))
}

fn extract_ts_modifiers(node: Node<'_>, content: &str) -> Vec<String> {
    // Grammar order for method_definition: accessibility, static, override,
    // readonly, async, get|set|*, name, optional `?`.
    // Only name/parameters/return_type/type_parameters/body are fields.
    let mut mods = Vec::new();
    let mut c = node.walk();
    for child in node.children(&mut c) {
        match child.kind() {
            "accessibility_modifier" | "override_modifier" => {
                if let Some(t) = node_text_owned(child, content) {
                    mods.push(t);
                }
            }
            "static" | "readonly" | "async" | "get" | "set" | "abstract" => {
                if let Some(t) = node_text_owned(child, content) {
                    mods.push(t);
                }
            }
            "*" => {
                mods.push("*".to_string());
            }
            "?" => {
                // Optional method marker after name.
                mods.push("optional".to_string());
            }
            _ => {
                // Anonymous tokens sometimes report kind as the text itself.
                if !child.is_named()
                    && let Some(t) = node_text_owned(child, content)
                {
                    match t.as_str() {
                        "static" | "readonly" | "async" | "get" | "set" | "abstract" | "*"
                            if !mods.iter().any(|m| m == &t) =>
                        {
                            mods.push(t);
                        }
                        "?" if !mods.iter().any(|m| m == "optional") => {
                            mods.push("optional".to_string());
                        }
                        _ => {}
                    }
                }
            }
        }
    }
    mods
}

fn extract_ts_params(node: Node<'_>, content: &str) -> Vec<SignatureParam> {
    // formal_parameters via field `parameters`, or singular bare `parameter` on arrows.
    if let Some(params_node) = node.child_by_field_name("parameters") {
        return extract_formal_parameters(params_node, content);
    }
    // Arrow single-arg form: `parameter` field is an identifier (arity 1).
    if let Some(param) = node.child_by_field_name("parameter") {
        let name = if param.kind() == "identifier" {
            node_text_owned(param, content)
        } else {
            None
        };
        return vec![SignatureParam {
            name,
            type_text: None,
        }];
    }
    Vec::new()
}

fn extract_formal_parameters(params_node: Node<'_>, content: &str) -> Vec<SignatureParam> {
    let mut out = Vec::new();
    let mut c = params_node.walk();
    for child in params_node.children(&mut c) {
        match child.kind() {
            "required_parameter" | "optional_parameter" => {
                let optional = child.kind() == "optional_parameter";
                let (name, is_rest) = ts_param_name(child, content);
                let mut type_text = child
                    .child_by_field_name("type")
                    .and_then(|t| strip_type_annotation(t, content));
                // Encode optionality so required→optional is Shape.
                if optional {
                    type_text = Some(match type_text {
                        Some(t) => format!("{t}?"),
                        None => "?".to_string(),
                    });
                }
                // Rest params: mark variadic in type text (`...string[]`).
                if is_rest {
                    type_text = Some(match type_text {
                        Some(t) if t.starts_with("...") => t,
                        Some(t) => format!("...{t}"),
                        None => "...".to_string(),
                    });
                }
                out.push(SignatureParam { name, type_text });
            }
            _ => {}
        }
    }
    out
}

fn ts_param_name(param: Node<'_>, content: &str) -> (Option<String>, bool /* is_rest */) {
    if let Some(name_node) = param.child_by_field_name("name") {
        if name_node.kind() == "rest_pattern" {
            let inner = first_identifier(name_node, content);
            return (inner, true);
        }
        if name_node.kind() == "identifier" {
            return (node_text_owned(name_node, content), false);
        }
    }
    if let Some(pattern) = param.child_by_field_name("pattern") {
        if pattern.kind() == "rest_pattern" {
            return (first_identifier(pattern, content), true);
        }
        return (first_identifier(pattern, content), false);
    }
    (None, false)
}

fn extract_ts_return_type(node: Node<'_>, content: &str) -> Option<String> {
    let ret = node.child_by_field_name("return_type")?;
    match ret.kind() {
        "type_annotation" | "asserts_annotation" | "type_predicate_annotation" => {
            strip_type_annotation(ret, content)
        }
        _ => {
            // Fallback: strip leading `:` if present.
            node_text_owned(ret, content).map(|s| normalize_ts_type_text(s.trim_start_matches(':')))
        }
    }
    .filter(|s| !s.is_empty())
}

/// Strip leading `:` from a type_annotation and normalize whitespace so
/// `Record<string, number>` and `Record<string,number>` share a shape
/// (codex R2 P1 — same contract as Python `normalize_type_text`).
fn strip_type_annotation(node: Node<'_>, content: &str) -> Option<String> {
    let raw = node_text_owned(node, content)?;
    let s = normalize_ts_type_text(raw.trim_start_matches(':'));
    if s.is_empty() { None } else { Some(s) }
}

/// Collapse formatting-only whitespace without destroying token boundaries.
/// - `Record<string, number>` → `Record<string,number>` (space after `,` dropped)
/// - `x is Foo` → `x is Foo` (space between words kept for type predicates)
fn normalize_ts_type_text(raw: &str) -> String {
    collapse_type_whitespace(raw)
}

fn collapse_type_whitespace(raw: &str) -> String {
    let chars: Vec<char> = raw.chars().collect();
    let mut out = String::with_capacity(chars.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i].is_whitespace() {
            let mut j = i;
            while j < chars.len() && chars[j].is_whitespace() {
                j += 1;
            }
            let left_word = out
                .chars()
                .last()
                .is_some_and(|c| c.is_alphanumeric() || c == '_');
            let right_word = chars
                .get(j)
                .is_some_and(|c| c.is_alphanumeric() || *c == '_');
            if left_word && right_word {
                out.push(' ');
            }
            i = j;
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

fn first_identifier(node: Node<'_>, content: &str) -> Option<String> {
    if matches!(
        node.kind(),
        "identifier" | "property_identifier" | "type_identifier"
    ) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::signature::{
        METADATA_SIGNATURE, METADATA_SIGNATURE_SHAPE, SignatureChangeClass, SymbolSignature,
        classify_signature_change,
    };
    use crate::index::symbols::SymbolKind;
    use std::path::Path;

    fn extract(content: &str) -> Vec<Symbol> {
        extract_symbols(content, None).unwrap().unwrap()
    }

    fn extract_at(content: &str, path: &str) -> Vec<Symbol> {
        extract_symbols(content, Some(Path::new(path)))
            .unwrap()
            .unwrap()
    }

    fn sig_of(content: &str, name: &str) -> SymbolSignature {
        let symbols = extract(content);
        let s = symbols
            .iter()
            .find(|s| s.name == name)
            .unwrap_or_else(|| panic!("symbol {name} not found"));
        SymbolSignature {
            text: s
                .metadata
                .get(METADATA_SIGNATURE)
                .cloned()
                .unwrap_or_default(),
            shape: s
                .metadata
                .get(METADATA_SIGNATURE_SHAPE)
                .cloned()
                .unwrap_or_default(),
        }
    }

    #[test]
    fn test_extract_typescript_symbols() {
        let content = r#"
            export function publicFn() {}
            function privateFn() {}
            export class PublicClass {}
            class PrivateClass {}
            export interface PublicInterface {}
            export type PublicType = string;
            export enum PublicEnum { A }
        "#;

        let symbols = extract(content);

        assert!(
            symbols
                .iter()
                .any(|s| s.name == "publicFn" && s.kind == SymbolKind::Function && s.is_public)
        );
        assert!(
            symbols
                .iter()
                .any(|s| s.name == "privateFn" && s.kind == SymbolKind::Function && !s.is_public)
        );
        assert!(
            symbols
                .iter()
                .any(|s| s.name == "PublicClass" && s.kind == SymbolKind::Class && s.is_public)
        );
        assert!(
            symbols
                .iter()
                .any(|s| s.name == "PrivateClass" && s.kind == SymbolKind::Class && !s.is_public)
        );
        assert!(symbols.iter().any(|s| s.name == "PublicInterface"
            && s.kind == SymbolKind::Interface
            && s.is_public));
        assert!(
            symbols
                .iter()
                .any(|s| s.name == "PublicType" && s.kind == SymbolKind::Type && s.is_public)
        );
        assert!(
            symbols
                .iter()
                .any(|s| s.name == "PublicEnum" && s.kind == SymbolKind::Enum && s.is_public)
        );
    }

    #[test]
    fn test_typescript_method_qualified_name() {
        let content = r#"
            export class Service {
                process(): void {}
                helper(): number { return 1; }
            }
            class Other {
                process(): void {}
            }
            function freeFn(): void {}
        "#;

        let symbols = extract(content);

        let process = symbols
            .iter()
            .find(|s| s.qualified_name.as_deref() == Some("Service.process"))
            .expect("Service.process");
        assert_eq!(process.kind, SymbolKind::Method);
        assert_eq!(process.name, "process");
        assert!(process.is_public, "method on exported class is public");

        let other = symbols
            .iter()
            .find(|s| s.qualified_name.as_deref() == Some("Other.process"))
            .expect("Other.process");
        assert_eq!(other.kind, SymbolKind::Method);

        let free = symbols.iter().find(|s| s.name == "freeFn").expect("freeFn");
        assert_eq!(free.kind, SymbolKind::Function);
        assert_eq!(free.qualified_name, None);

        let helper = symbols
            .iter()
            .find(|s| s.qualified_name.as_deref() == Some("Service.helper"))
            .expect("Service.helper");
        assert_eq!(helper.kind, SymbolKind::Method);
    }

    #[test]
    fn test_typescript_object_literal_method_emitted_as_method() {
        // R1-06: method_definition outside class must not be dropped.
        let content = r#"
            const api = {
                fetchData(): number { return 1; },
                get(): string { return "x"; }
            };
            export class Service {
                get(): void {}
            }
        "#;

        let symbols = extract(content);

        let object_fetch = symbols
            .iter()
            .find(|s| s.name == "fetchData" && s.kind == SymbolKind::Method)
            .expect("object-literal fetchData as Method");
        assert_eq!(
            object_fetch.qualified_name, None,
            "non-class method keeps QN None"
        );

        let object_get = symbols
            .iter()
            .filter(|s| s.name == "get" && s.kind == SymbolKind::Method)
            .collect::<Vec<_>>();
        assert!(
            object_get.len() >= 2,
            "both object-literal get and Service.get should be present, got: {:?}",
            symbols
                .iter()
                .map(|s| format!("{}:{:?}:{:?}", s.name, s.kind, s.qualified_name))
                .collect::<Vec<_>>()
        );
        assert!(
            object_get
                .iter()
                .any(|s| s.qualified_name.as_deref() == Some("Service.get")),
            "class method keeps Class.method QN"
        );
        assert!(
            object_get.iter().any(|s| s.qualified_name.is_none()),
            "object-literal get has no QN"
        );
    }

    #[test]
    fn signature_hash_non_null_via_project_symbol() {
        use crate::index::types::symbol_to_project_symbol;
        let content = "export function add(a: number, b: number): number { return a + b; }";
        let symbols = extract(content);
        let add = symbols.iter().find(|s| s.name == "add").expect("add");
        assert!(add.metadata.contains_key(METADATA_SIGNATURE));
        assert!(add.metadata.contains_key(METADATA_SIGNATURE_SHAPE));
        let ps = symbol_to_project_symbol(add, 1, "now");
        assert!(
            ps.signature_hash.is_some(),
            "TS function must yield non-null signature_hash"
        );
    }

    #[test]
    fn optional_param_is_shape_change() {
        let req = sig_of("function f(a: string) {}\n", "f");
        let opt = sig_of("function f(a?: string) {}\n", "f");
        assert!(!req.shape.contains("string?"), "required: {}", req.shape);
        assert!(
            opt.shape.contains("string?"),
            "optional encoded: {}",
            opt.shape
        );
        assert_eq!(
            classify_signature_change(&req, &opt),
            Some(SignatureChangeClass::Shape)
        );
    }

    #[test]
    fn no_leading_colon_in_shape() {
        let s = sig_of("function f(a: string): number { return 1; }\n", "f");
        assert!(
            !s.shape.contains(": string") && !s.shape.contains(":number"),
            "no leading colon: {}",
            s.shape
        );
        assert!(s.shape.contains("params=string"), "{}", s.shape);
        assert!(s.shape.contains("ret=number"), "{}", s.shape);
    }

    #[test]
    fn async_static_private_move_shape() {
        let plain = sig_of("class C { m(): void {} }\n", "m");
        let async_m = sig_of("class C { async m(): Promise<void> {} }\n", "m");
        let static_m = sig_of("class C { static m(): void {} }\n", "m");
        let private_m = sig_of("class C { private m(): void {} }\n", "m");
        assert_ne!(plain.shape, async_m.shape, "async");
        assert!(async_m.shape.contains("async"), "{}", async_m.shape);
        assert_ne!(plain.shape, static_m.shape, "static");
        assert!(static_m.shape.contains("static"), "{}", static_m.shape);
        assert_ne!(plain.shape, private_m.shape, "private");
        assert!(private_m.shape.contains("private"), "{}", private_m.shape);
    }

    #[test]
    fn type_predicate_return_captured() {
        let s = sig_of(
            "function isFoo(x: unknown): x is Foo { return true; }\n",
            "isFoo",
        );
        assert!(
            s.shape.contains("x is Foo") || s.shape.contains("is Foo"),
            "predicate ret: {}",
            s.shape
        );
        assert!(!s.shape.contains(": x is"), "no leading colon: {}", s.shape);
    }

    #[test]
    fn rest_param_is_shape_change() {
        let fixed = sig_of("function f(a: string, b: string) {}\n", "f");
        let rest = sig_of("function f(a: string, ...args: string[]) {}\n", "f");
        assert!(rest.shape.contains("..."), "rest marker: {}", rest.shape);
        assert_eq!(
            classify_signature_change(&fixed, &rest),
            Some(SignatureChangeClass::Shape)
        );
    }

    #[test]
    fn anonymous_arrow_yields_nothing() {
        let content = r#"
            const xs = [1, 2, 3];
            xs.map(x => x * 2);
            function keep() {}
        "#;
        let symbols = extract(content);
        assert!(
            symbols.iter().all(|s| s.name != "x" && s.name != "default"),
            "anonymous arrow must not produce a symbol: {:?}",
            symbols.iter().map(|s| &s.name).collect::<Vec<_>>()
        );
        assert!(symbols.iter().any(|s| s.name == "keep"));
    }

    #[test]
    fn nested_anonymous_arrow_does_not_steal_outer_host() {
        // DoD-4 / R1: callback inside a named arrow must not inherit the outer name.
        let content = r#"
            const load = async () => {
                items.map(x => x.id);
            };
            class C {
                handler = () => {
                    xs.map(y => y * 2);
                };
            }
        "#;
        let symbols = extract(content);
        let load_count = symbols.iter().filter(|s| s.name == "load").count();
        assert_eq!(
            load_count,
            1,
            "outer load once only; nested map callback must not also be named load: {:?}",
            symbols
                .iter()
                .map(|s| format!("{}:{:?}", s.name, s.kind))
                .collect::<Vec<_>>()
        );
        assert!(
            symbols.iter().all(|s| s.name != "x" && s.name != "y"),
            "nested callbacks must not be indexed: {:?}",
            symbols.iter().map(|s| &s.name).collect::<Vec<_>>()
        );
        let handler_count = symbols
            .iter()
            .filter(|s| s.name == "handler" || s.qualified_name.as_deref() == Some("C.handler"))
            .count();
        assert_eq!(handler_count, 1, "C.handler once only");
    }

    #[test]
    fn parenthesized_named_arrow_still_hosts() {
        let symbols = extract("const id = ((x: number) => x);\n");
        let id = symbols.iter().find(|s| s.name == "id").expect("id");
        assert!(id.metadata.contains_key(METADATA_SIGNATURE_SHAPE));
    }

    #[test]
    fn class_field_arrow_includes_field_modifiers() {
        // codex R1 P2: private/static/readonly live on public_field_definition.
        let private_field = extract("class C { private handler = (e: Event) => {}; }\n");
        let public_field = extract("class C { public handler = (e: Event) => {}; }\n");
        let private_shape = private_field
            .iter()
            .find(|s| s.qualified_name.as_deref() == Some("C.handler"))
            .and_then(|s| s.metadata.get(METADATA_SIGNATURE_SHAPE))
            .expect("private C.handler shape");
        let public_shape = public_field
            .iter()
            .find(|s| s.qualified_name.as_deref() == Some("C.handler"))
            .and_then(|s| s.metadata.get(METADATA_SIGNATURE_SHAPE))
            .expect("public C.handler shape");
        assert!(
            private_shape.contains("private"),
            "field accessibility must enter shape: {private_shape}"
        );
        assert!(
            public_shape.contains("public"),
            "field accessibility must enter shape: {public_shape}"
        );
        assert_ne!(
            private_shape, public_shape,
            "private → public on field arrow must be Shape"
        );
    }

    #[test]
    fn named_arrows_at_three_hosts() {
        // 1. variable_declarator
        let var = extract("const handler = (e: Event) => {};\n");
        let h = var.iter().find(|s| s.name == "handler").expect("handler");
        assert_eq!(h.kind, SymbolKind::Function);
        assert!(h.metadata.contains_key(METADATA_SIGNATURE_SHAPE));

        // 2. public_field_definition
        let field = extract("class C { handler = (e: Event) => {}; }\n");
        let fh = field
            .iter()
            .find(|s| s.qualified_name.as_deref() == Some("C.handler"))
            .expect("C.handler");
        assert_eq!(fh.kind, SymbolKind::Method);

        // 3. export default arrow with path-derived QN
        let def = extract_at("export default (a: string) => {};\n", "src/handlers.ts");
        let d = def
            .iter()
            .find(|s| s.qualified_name.as_deref() == Some("src.handlers.default"))
            .expect("src.handlers.default");
        assert_eq!(d.name, "default");
        assert!(d.is_public);
        assert!(d.metadata.contains_key(METADATA_SIGNATURE_SHAPE));
    }

    #[test]
    fn two_default_exports_do_not_collide() {
        let a = extract_at("export default (a: string) => {};\n", "src/handlers.ts");
        let b = extract_at("export default (a: number) => {};\n", "src/routes.ts");
        let qa = a
            .iter()
            .find(|s| s.name == "default")
            .and_then(|s| s.qualified_name.as_deref());
        let qb = b
            .iter()
            .find(|s| s.name == "default")
            .and_then(|s| s.qualified_name.as_deref());
        assert_eq!(qa, Some("src.handlers.default"));
        assert_eq!(qb, Some("src.routes.default"));
        assert_ne!(qa, qb);
    }

    #[test]
    fn same_stem_different_dirs_default_exports_do_not_collide() {
        // codex R2 P2: bare stem `index` collides; path-qualified must not.
        let a = extract_at("export default (a: string) => {};\n", "src/foo/index.ts");
        let b = extract_at("export default (a: number) => {};\n", "src/bar/index.ts");
        let qa = a
            .iter()
            .find(|s| s.name == "default")
            .and_then(|s| s.qualified_name.clone());
        let qb = b
            .iter()
            .find(|s| s.name == "default")
            .and_then(|s| s.qualified_name.clone());
        assert_eq!(qa.as_deref(), Some("src.foo.index.default"));
        assert_eq!(qb.as_deref(), Some("src.bar.index.default"));
        assert_ne!(qa, qb);
    }

    #[test]
    fn ts_type_whitespace_normalization() {
        let spaced = extract("function f(d: Record<string, number>): void {}\n");
        let tight = extract("function f(d: Record<string,number>): void {}\n");
        let s = spaced
            .iter()
            .find(|x| x.name == "f")
            .and_then(|x| x.metadata.get(METADATA_SIGNATURE_SHAPE))
            .expect("spaced");
        let t = tight
            .iter()
            .find(|x| x.name == "f")
            .and_then(|x| x.metadata.get(METADATA_SIGNATURE_SHAPE))
            .expect("tight");
        assert_eq!(s, t, "TS type spacing must not move shape: {s} vs {t}");
    }

    #[test]
    fn interface_method_signature_qualified() {
        let content = r#"
            export interface Iface {
                method(a: string): void;
            }
        "#;
        let symbols = extract(content);
        let m = symbols
            .iter()
            .find(|s| s.qualified_name.as_deref() == Some("Iface.method"))
            .expect("Iface.method");
        assert_eq!(m.kind, SymbolKind::Method);
        assert!(m.metadata.contains_key(METADATA_SIGNATURE_SHAPE));
        let shape = m.metadata.get(METADATA_SIGNATURE_SHAPE).unwrap();
        assert!(shape.contains("params=string"), "{shape}");
    }

    #[test]
    fn bare_parameter_arrow_arity_one() {
        let symbols = extract("const id = x => x;\n");
        let id = symbols.iter().find(|s| s.name == "id").expect("id");
        let shape = id.metadata.get(METADATA_SIGNATURE_SHAPE).expect("shape");
        assert!(shape.contains("arity=1"), "bare parameter arity: {shape}");
    }

    #[test]
    fn abstract_method_signature_captured() {
        let content = r#"
            abstract class Base {
                abstract run(n: number): void;
            }
        "#;
        let symbols = extract(content);
        let run = symbols
            .iter()
            .find(|s| s.qualified_name.as_deref() == Some("Base.run"))
            .expect("Base.run");
        assert_eq!(run.kind, SymbolKind::Method);
        assert!(run.metadata.contains_key(METADATA_SIGNATURE_SHAPE));
    }

    #[test]
    fn function_signature_captured() {
        let content = "declare function helper(a: number): string;\n";
        let symbols = extract(content);
        let h = symbols.iter().find(|s| s.name == "helper").expect("helper");
        assert_eq!(h.kind, SymbolKind::Function);
        let shape = h.metadata.get(METADATA_SIGNATURE_SHAPE).expect("shape");
        assert!(shape.contains("params=number"), "{shape}");
        assert!(shape.contains("ret=string"), "{shape}");
    }

    #[test]
    fn jsx_component_yields_symbol_with_tsx_grammar() {
        let content = r#"
            export function Widget(props: { label: string }) {
                return <div>{props.label}</div>;
            }
        "#;
        let symbols = extract_at(content, "src/Widget.tsx");
        let w = symbols.iter().find(|s| s.name == "Widget").expect("Widget");
        assert_eq!(w.kind, SymbolKind::Function);
        assert!(w.metadata.contains_key(METADATA_SIGNATURE_SHAPE));
    }

    #[test]
    fn destructuring_arrow_host_skipped() {
        let content = "const [a, b] = [(x: number) => x, (y: number) => y];\n";
        let symbols = extract(content);
        // No symbols from the arrows (destructuring host).
        assert!(
            symbols
                .iter()
                .all(|s| s.name != "a" && s.name != "b" && s.name != "x" && s.name != "y"),
            "got: {:?}",
            symbols.iter().map(|s| &s.name).collect::<Vec<_>>()
        );
    }
}
