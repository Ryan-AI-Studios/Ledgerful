use crate::index::signature::{
    SignatureParam, SymbolSignatureParts, build_symbol_signature, write_signature_metadata,
};
use crate::index::symbols::{Symbol, SymbolKind};
use miette::{IntoDiagnostic, Result};
use tree_sitter::{Node, Parser, Query, QueryCursor, StreamingIterator};

pub fn extract_symbols(content: &str) -> Result<Option<Vec<Symbol>>> {
    let mut parser = Parser::new();
    let language = tree_sitter_python::LANGUAGE;
    parser.set_language(&language.into()).into_diagnostic()?;

    let tree = parser
        .parse(content, None)
        .ok_or_else(|| miette::miette!("Failed to parse Python content"))?;

    let query_str = r#"
        (function_definition name: (identifier) @name) @symbol
        (class_definition name: (identifier) @name) @symbol
    "#;

    let query = Query::new(&language.into(), query_str).into_diagnostic()?;
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&query, tree.root_node(), content.as_bytes());

    let mut symbols = Vec::new();

    while let Some(m) = matches.next() {
        let mut name = String::new();
        let mut kind = SymbolKind::Function;
        let mut symbol_node: Option<Node<'_>> = None;

        for capture in m.captures {
            let capture_name = query.capture_names()[capture.index as usize];
            if capture_name == "name" {
                name = capture
                    .node
                    .utf8_text(content.as_bytes())
                    .into_diagnostic()?
                    .to_string();
            } else if capture_name == "symbol" {
                symbol_node = Some(capture.node);
                match capture.node.kind() {
                    "function_definition" => kind = SymbolKind::Function,
                    "class_definition" => kind = SymbolKind::Class,
                    _ => {}
                }
            }
        }

        if name.is_empty() {
            continue;
        }

        // Methods: function_definition nested under class_definition without an
        // intervening function_definition (nested defs stay free Functions).
        let mut qualified_name = None;
        if kind == SymbolKind::Function
            && let Some(node) = symbol_node
            && let Some(class_name) = enclosing_class_name(node, content)
        {
            kind = SymbolKind::Method;
            qualified_name = Some(format!("{class_name}.{name}"));
        }

        let is_public = !name.starts_with('_');
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
            && let Some(sig) = extract_python_signature(node, content, &name)
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

/// Walk parents of a `function_definition`. If a `class_definition` is found
/// before another `function_definition`, return the class name.
fn enclosing_class_name(function_node: Node<'_>, content: &str) -> Option<String> {
    let mut current = function_node.parent();
    while let Some(parent) = current {
        match parent.kind() {
            "function_definition" => return None,
            "class_definition" => {
                let mut cursor = parent.walk();
                for child in parent.children(&mut cursor) {
                    if child.kind() == "identifier" {
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

/// Extract a normalized signature from a Python `function_definition`.
fn extract_python_signature(
    node: Node<'_>,
    content: &str,
    name: &str,
) -> Option<crate::index::signature::SymbolSignature> {
    let (params, mut modifiers) = extract_python_params(node, content);
    // `async` is an anonymous token child (optional('async')), not a field.
    if has_async_modifier(node, content) {
        modifiers.insert(0, "async".to_string());
    }
    // Binding decorators live on parent `decorated_definition`, not on the fn node.
    for deco in binding_decorators(node, content) {
        if !modifiers.iter().any(|m| m == &deco) {
            modifiers.push(deco);
        }
    }
    let return_type = node
        .child_by_field_name("return_type")
        .and_then(|n| node_text_owned(n, content))
        .map(|s| normalize_type_text(&s))
        .filter(|s| !s.is_empty());

    let parts = SymbolSignatureParts {
        name: name.to_string(),
        modifiers,
        params,
        return_type,
    };
    Some(build_symbol_signature(&parts))
}

fn has_async_modifier(node: Node<'_>, content: &str) -> bool {
    let mut c = node.walk();
    for child in node.children(&mut c) {
        if child.kind() == "async" {
            return true;
        }
        // Defensive: some layouts may surface the token as text "async".
        if let Some(t) = node_text_owned(child, content)
            && t == "async"
            && !child.is_named()
        {
            return true;
        }
    }
    false
}

/// Binding-decorator allowlist only (`staticmethod`, `classmethod`, `property`,
/// `abstractmethod`). Matched on the decorator's trailing identifier so
/// `@abc.abstractmethod` hits. Other decorators (routes, pytest, wraps) are
/// deliberately ignored — they are not calling-contract and would false-positive
/// Shape. See docs/Signature-Diff.md and 0091 §2.6b.
fn binding_decorators(function_node: Node<'_>, content: &str) -> Vec<String> {
    const ALLOW: &[&str] = &["staticmethod", "classmethod", "property", "abstractmethod"];
    let Some(parent) = function_node.parent() else {
        return Vec::new();
    };
    if parent.kind() != "decorated_definition" {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut c = parent.walk();
    for child in parent.children(&mut c) {
        if child.kind() != "decorator" {
            continue;
        }
        if let Some(trailing) = trailing_identifier(child, content)
            && ALLOW.contains(&trailing.as_str())
            && !out.iter().any(|m| m == &trailing)
        {
            out.push(trailing);
        }
    }
    out
}

fn trailing_identifier(node: Node<'_>, content: &str) -> Option<String> {
    // Depth-first last identifier under the decorator expression.
    if node.kind() == "identifier" {
        return node_text_owned(node, content);
    }
    let mut last = None;
    let mut c = node.walk();
    for child in node.children(&mut c) {
        if let Some(id) = trailing_identifier(child, content) {
            last = Some(id);
        }
    }
    last
}

/// Parameter extraction for Python's nine `parameter` subtypes.
///
/// Separator encoding (not params — do not inflate arity):
/// - `positional_separator` (`/`) after N real params → modifier `posonly-after=N`
/// - `keyword_separator` (bare `*`) after N real params → modifier `kwonly-after=N`
///
/// Variadics (`*args` / `**kwargs`, including typed wrappers) **are** params and
/// count toward arity; their type_text is prefixed with `*` / `**`.
fn extract_python_params(node: Node<'_>, content: &str) -> (Vec<SignatureParam>, Vec<String>) {
    let Some(params_node) = node.child_by_field_name("parameters") else {
        return (Vec::new(), Vec::new());
    };
    let mut params = Vec::new();
    let mut modifiers = Vec::new();
    let mut c = params_node.walk();
    for child in params_node.children(&mut c) {
        match child.kind() {
            "positional_separator" => {
                // `/` — params before this are positional-only.
                modifiers.push(format!("posonly-after={}", params.len()));
            }
            "keyword_separator" => {
                // bare `*` — params after this are keyword-only.
                modifiers.push(format!("kwonly-after={}", params.len()));
            }
            "identifier" => {
                let name = node_text_owned(child, content);
                params.push(SignatureParam {
                    name,
                    type_text: None,
                });
            }
            "typed_parameter" => {
                // Trap 1: no `name` field — first named child that is not `type`.
                // Trap 2: may wrap list_splat_pattern / dictionary_splat_pattern.
                let type_text = child
                    .child_by_field_name("type")
                    .and_then(|t| node_text_owned(t, content))
                    .map(|s| normalize_type_text(&s))
                    .filter(|s| !s.is_empty());
                let (name, type_text) = typed_parameter_name_and_type(child, content, type_text);
                params.push(SignatureParam { name, type_text });
            }
            "default_parameter" => {
                // Default value is not part of the shape.
                let name = child.child_by_field_name("name").and_then(|n| {
                    if n.kind() == "identifier" {
                        node_text_owned(n, content)
                    } else {
                        // tuple_pattern as name — no single name.
                        None
                    }
                });
                params.push(SignatureParam {
                    name,
                    type_text: None,
                });
            }
            "typed_default_parameter" => {
                let name = child
                    .child_by_field_name("name")
                    .and_then(|n| node_text_owned(n, content));
                let type_text = child
                    .child_by_field_name("type")
                    .and_then(|t| node_text_owned(t, content))
                    .map(|s| normalize_type_text(&s))
                    .filter(|s| !s.is_empty());
                params.push(SignatureParam { name, type_text });
            }
            "tuple_pattern" => {
                params.push(SignatureParam {
                    name: None,
                    type_text: None,
                });
            }
            "list_splat_pattern" => {
                let name = first_identifier(child, content);
                params.push(SignatureParam {
                    name,
                    type_text: Some("*".to_string()),
                });
            }
            "dictionary_splat_pattern" => {
                let name = first_identifier(child, content);
                params.push(SignatureParam {
                    name,
                    type_text: Some("**".to_string()),
                });
            }
            _ => {}
        }
    }
    (params, modifiers)
}

/// Resolve name + type for `typed_parameter`, handling variadic wrappers.
fn typed_parameter_name_and_type(
    node: Node<'_>,
    content: &str,
    base_type: Option<String>,
) -> (Option<String>, Option<String>) {
    let mut c = node.walk();
    for child in node.children(&mut c) {
        // Skip the type field node.
        if node.child_by_field_name("type").map(|t| t.id()) == Some(child.id()) {
            continue;
        }
        if !child.is_named() {
            continue;
        }
        match child.kind() {
            "identifier" => {
                return (node_text_owned(child, content), base_type);
            }
            "list_splat_pattern" => {
                let name = first_identifier(child, content);
                let type_text = match base_type {
                    Some(t) if !t.is_empty() => Some(format!("*{t}")),
                    _ => Some("*".to_string()),
                };
                return (name, type_text);
            }
            "dictionary_splat_pattern" => {
                let name = first_identifier(child, content);
                let type_text = match base_type {
                    Some(t) if !t.is_empty() => Some(format!("**{t}")),
                    _ => Some("**".to_string()),
                };
                return (name, type_text);
            }
            _ => {}
        }
    }
    (None, base_type)
}

fn first_identifier(node: Node<'_>, content: &str) -> Option<String> {
    if node.kind() == "identifier" {
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

/// Strip surrounding quotes (forward-ref `"MyClass"`) and collapse
/// formatting-only whitespace so `dict[str, int]` and `dict[str,int]` share a
/// shape, while preserving word-separating spaces (`x is Y` style tokens if
/// they appear in annotations).
///
/// `split_whitespace().join(" ")` is not enough: it keeps the space after
/// commas inside brackets, which would false-positive as Shape risk on a pure
/// formatting edit (codex R1 P1). Blanking *all* whitespace is too aggressive
/// for multi-word type forms (codex R3 / type-predicate lesson on TS).
fn normalize_type_text(raw: &str) -> String {
    let mut s = raw.trim().to_string();
    // Strip matching surrounding quotes (single or double).
    if s.len() >= 2 {
        let bytes = s.as_bytes();
        let first = bytes[0];
        let last = bytes[bytes.len() - 1];
        if (first == b'"' && last == b'"') || (first == b'\'' && last == b'\'') {
            s = s[1..s.len() - 1].to_string();
        }
    }
    collapse_type_whitespace(&s)
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

    fn sig_of(content: &str, name: &str) -> SymbolSignature {
        let symbols = extract_symbols(content).unwrap().unwrap();
        let s = symbols
            .iter()
            .find(|s| s.name == name)
            .unwrap_or_else(|| panic!("symbol {name} not found in {symbols:?}"));
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
    fn test_extract_python_symbols() {
        let content = r#"
    def public_fn():
        pass

    def _private_fn():
        pass

    class PublicClass:
        pass

    class _PrivateClass:
        pass
    "#;

        let symbols = extract_symbols(content).unwrap().unwrap();

        assert!(
            symbols
                .iter()
                .any(|s| s.name == "public_fn" && s.kind == SymbolKind::Function && s.is_public)
        );
        assert!(
            symbols
                .iter()
                .any(|s| s.name == "_private_fn" && s.kind == SymbolKind::Function && !s.is_public)
        );
        assert!(
            symbols
                .iter()
                .any(|s| s.name == "PublicClass" && s.kind == SymbolKind::Class && s.is_public)
        );
        assert!(
            symbols
                .iter()
                .any(|s| s.name == "_PrivateClass" && s.kind == SymbolKind::Class && !s.is_public)
        );
    }

    #[test]
    fn test_python_method_qualified_name() {
        let content = r#"
class Service:
    def process(self):
        pass

    def _private(self):
        pass

def free_fn():
    pass

class Other:
    def process(self):
        def nested():
            pass
        pass
"#;

        let symbols = extract_symbols(content).unwrap().unwrap();

        let process = symbols
            .iter()
            .find(|s| s.qualified_name.as_deref() == Some("Service.process"))
            .expect("Service.process Method");
        assert_eq!(process.kind, SymbolKind::Method);
        assert_eq!(process.name, "process");
        assert!(process.is_public);

        let private = symbols
            .iter()
            .find(|s| s.qualified_name.as_deref() == Some("Service._private"))
            .expect("Service._private Method");
        assert_eq!(private.kind, SymbolKind::Method);
        assert!(!private.is_public);

        let free = symbols
            .iter()
            .find(|s| s.name == "free_fn")
            .expect("free_fn");
        assert_eq!(free.kind, SymbolKind::Function);
        assert_eq!(free.qualified_name, None);

        let other = symbols
            .iter()
            .find(|s| s.qualified_name.as_deref() == Some("Other.process"))
            .expect("Other.process");
        assert_eq!(other.kind, SymbolKind::Method);

        // Nested function inside method stays Function without QN.
        let nested = symbols.iter().find(|s| s.name == "nested").expect("nested");
        assert_eq!(nested.kind, SymbolKind::Function);
        assert_eq!(nested.qualified_name, None);

        // Two process methods are distinguished by QN (DoD-5 shape).
        let process_qns: Vec<_> = symbols
            .iter()
            .filter(|s| s.name == "process")
            .filter_map(|s| s.qualified_name.as_deref())
            .collect();
        assert!(process_qns.contains(&"Service.process"));
        assert!(process_qns.contains(&"Other.process"));
    }

    #[test]
    fn signature_hash_non_null_via_project_symbol() {
        use crate::index::types::symbol_to_project_symbol;
        let content = "def add(a: int, b: int) -> int:\n    return a + b\n";
        let symbols = extract_symbols(content).unwrap().unwrap();
        let add = symbols.iter().find(|s| s.name == "add").expect("add");
        assert!(add.metadata.contains_key(METADATA_SIGNATURE));
        assert!(add.metadata.contains_key(METADATA_SIGNATURE_SHAPE));
        let ps = symbol_to_project_symbol(add, 1, "now");
        assert!(
            ps.signature_hash.is_some(),
            "Python function must yield non-null signature_hash"
        );
    }

    #[test]
    fn variadic_is_not_arity_collapse() {
        // f(a, b) → f(*args) must not look like arity 2→0; both sides have arity > 0
        // and shapes differ (variadic type marker).
        let fixed = sig_of("def f(a, b):\n    pass\n", "f");
        let variadic = sig_of("def f(*args):\n    pass\n", "f");
        assert!(
            fixed.shape.contains("arity=2"),
            "fixed arity: {}",
            fixed.shape
        );
        assert!(
            variadic.shape.contains("arity=1"),
            "variadic arity: {}",
            variadic.shape
        );
        assert!(
            variadic.shape.contains("params=*") || variadic.shape.contains("*"),
            "variadic type in shape: {}",
            variadic.shape
        );
        assert_eq!(
            classify_signature_change(&fixed, &variadic),
            Some(SignatureChangeClass::Shape)
        );
    }

    #[test]
    fn keyword_separator_is_shape_not_arity() {
        // f(a, b) → f(a, *, b): arity stays 2; separator lives in modifiers.
        let plain = sig_of("def f(a, b):\n    pass\n", "f");
        let kwonly = sig_of("def f(a, *, b):\n    pass\n", "f");
        assert!(plain.shape.contains("arity=2"), "{}", plain.shape);
        assert!(kwonly.shape.contains("arity=2"), "{}", kwonly.shape);
        assert!(
            kwonly.shape.contains("kwonly-after=1"),
            "separator encoding: {}",
            kwonly.shape
        );
        assert_eq!(
            classify_signature_change(&plain, &kwonly),
            Some(SignatureChangeClass::Shape)
        );
    }

    #[test]
    fn typed_parameter_has_named_param_in_text() {
        // Trap 1: typed_parameter has no name field — must still put name in text.
        let s = sig_of("def f(a: int):\n    pass\n", "f");
        assert!(
            s.text.contains("a: int") || s.text.contains("a:"),
            "readable text must include param name: {}",
            s.text
        );
        assert!(s.shape.contains("params=int"), "shape type: {}", s.shape);
    }

    #[test]
    fn annotated_variadic_detected() {
        // Trap 2: *args: int is typed_parameter wrapping list_splat_pattern.
        let s = sig_of("def f(*args: int):\n    pass\n", "f");
        assert!(
            s.shape.contains("*") && s.shape.contains("int"),
            "variadic annotated shape: {}",
            s.shape
        );
        assert!(
            s.text.contains("args") || s.shape.contains("*int") || s.shape.contains("*int"),
            "text/shape: {} / {}",
            s.text,
            s.shape
        );
    }

    #[test]
    fn unannotated_params_are_underscore_in_shape() {
        let s = sig_of("def f(a, b):\n    pass\n", "f");
        assert!(
            s.shape.contains("params=_,") || s.shape.contains("params=_,_"),
            "unannotated → _: {}",
            s.shape
        );
    }

    #[test]
    fn async_def_moves_shape() {
        let sync_s = sig_of("def f():\n    pass\n", "f");
        let async_s = sig_of("async def f():\n    pass\n", "f");
        assert_ne!(sync_s.shape, async_s.shape);
        assert!(
            async_s.shape.contains("async"),
            "async modifier: {}",
            async_s.shape
        );
        assert_eq!(
            classify_signature_change(&sync_s, &async_s),
            Some(SignatureChangeClass::Shape)
        );
    }

    #[test]
    fn quoted_return_type_same_shape_as_bare() {
        let bare = sig_of("def f() -> MyClass:\n    pass\n", "f");
        let quoted = sig_of("def f() -> \"MyClass\":\n    pass\n", "f");
        assert_eq!(
            bare.shape, quoted.shape,
            "quote strip: bare={} quoted={}",
            bare.shape, quoted.shape
        );
        assert!(
            !quoted.shape.contains('"'),
            "no quotes in shape: {}",
            quoted.shape
        );
    }

    #[test]
    fn type_whitespace_normalization_dict_bracket_spacing() {
        // codex R1 P1: dict[str, int] vs dict[str,int] must share a shape.
        let spaced = sig_of("def f(d: dict[str, int]) -> None:\n    pass\n", "f");
        let tight = sig_of("def f(d: dict[str,int]) -> None:\n    pass\n", "f");
        assert_eq!(
            spaced.shape, tight.shape,
            "spacing-only type text must not move shape: spaced={} tight={}",
            spaced.shape, tight.shape
        );
        assert!(
            spaced.shape.contains("dict[str,int]") || spaced.shape.contains("params=dict[str,int]"),
            "expected collapsed type in shape: {}",
            spaced.shape
        );
    }

    #[test]
    fn binding_decorator_staticmethod_to_classmethod_is_shape() {
        let static_s = sig_of("@staticmethod\ndef f(x):\n    pass\n", "f");
        let class_s = sig_of("@classmethod\ndef f(x):\n    pass\n", "f");
        assert!(
            static_s.shape.contains("staticmethod"),
            "{}",
            static_s.shape
        );
        assert!(class_s.shape.contains("classmethod"), "{}", class_s.shape);
        assert_eq!(
            classify_signature_change(&static_s, &class_s),
            Some(SignatureChangeClass::Shape)
        );
    }

    #[test]
    fn non_binding_decorator_not_in_shape() {
        // @app.route("/a") → @app.route("/b") must NOT be a shape change.
        let a = sig_of("@app.route(\"/a\")\ndef handler():\n    pass\n", "handler");
        let b = sig_of("@app.route(\"/b\")\ndef handler():\n    pass\n", "handler");
        assert_eq!(a.shape, b.shape, "route decorator must not enter shape");
        assert_eq!(classify_signature_change(&a, &b), None);
        assert!(
            !a.shape.contains("route") && !a.shape.contains("/a"),
            "route text must not appear: {}",
            a.shape
        );
    }

    #[test]
    fn abstractmethod_via_abc_module_hits_allowlist() {
        let s = sig_of(
            "import abc\nclass C:\n    @abc.abstractmethod\n    def m(self):\n        pass\n",
            "m",
        );
        assert!(
            s.shape.contains("abstractmethod"),
            "trailing id match: {}",
            s.shape
        );
    }

    #[test]
    fn positional_separator_encoding() {
        let s = sig_of("def f(a, /, b):\n    pass\n", "f");
        assert!(s.shape.contains("arity=2"), "{}", s.shape);
        assert!(
            s.shape.contains("posonly-after=1"),
            "posonly encoding: {}",
            s.shape
        );
    }
}
