//! Function/method signature extraction and normalization.
//!
//! # Naming collision
//!
//! Throughout Ledgerful, "signature" almost always means **Ed25519 ledger
//! crypto** (`verify --signatures`, `sig_version`, `src/ledger/crypto.rs`).
//! This module is about **function signatures** (arity, parameter types, return
//! type, behavioural modifiers). All public identifiers are prefixed
//! (`SymbolSignature`, `signature_shape`, `SignatureChangeClass`,
//! `SignatureParam`) so a security reviewer grepping for crypto surfaces is not
//! led into the indexer. **Do not introduce a bare module-level `signature`
//! identifier.**
//!
//! # Three change classes
//!
//! | Class | Trigger | Risk reason? |
//! |-------|---------|--------------|
//! | **Shape** | arity · ordered param types · return type · modifiers | yes |
//! | **Cosmetic** | parameter *rename* only (same shape) | no (recorded) |
//! | **Unknown** | language states no static type at a position | never inferred into risk |
//!
//! # Floor, not completeness
//!
//! This is a *textual shape* diff, not a type checker. Absence of a signature
//! change is **not** absence of a breaking change. Surfaces must never emit
//! "no breaking changes detected" or equivalent. Unannotated positions
//! normalize to `_`; do not infer types from defaults, call sites, or docs.

use serde::{Deserialize, Serialize};

/// A single parameter in a function/method signature.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SignatureParam {
    /// Parameter name when the language states one; excluded from the shape.
    pub name: Option<String>,
    /// Type text when the language states one; `None` → `_` in the shape.
    pub type_text: Option<String>,
}

/// Language-agnostic parts used to build readable text and shape strings.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SymbolSignatureParts {
    /// Function/method name (appears in readable text only).
    pub name: String,
    /// Behavioural modifiers in declaration order (`async`, `unsafe`, `const`, …).
    pub modifiers: Vec<String>,
    pub params: Vec<SignatureParam>,
    /// Return type text; `None` when the language states no return type.
    pub return_type: Option<String>,
}

/// Normalized signature pair written into `Symbol.metadata`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SymbolSignature {
    /// Human-readable signature including parameter names.
    pub text: String,
    /// Risk-bearing shape: modifiers + arity + ordered types + return (names excluded).
    pub shape: String,
}

/// Classification of a before/after signature comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SignatureChangeClass {
    Shape,
    Cosmetic,
    Unknown,
}

impl SignatureChangeClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Shape => "shape",
            Self::Cosmetic => "cosmetic",
            Self::Unknown => "unknown",
        }
    }
}

/// Build readable text + shape from language-agnostic parts.
///
/// Shape format (stable, parse-friendly):
/// `mods=<csv>;arity=<n>;params=<t1,t2,...>;ret=<t>`
/// where missing types are `_` and an empty modifier list yields `mods=`.
pub fn build_symbol_signature(parts: &SymbolSignatureParts) -> SymbolSignature {
    let text = format_readable(parts);
    let shape = format_shape(parts);
    SymbolSignature { text, shape }
}

/// Format the human-readable signature text.
fn format_readable(parts: &SymbolSignatureParts) -> String {
    let mut out = String::new();
    for m in &parts.modifiers {
        out.push_str(m);
        out.push(' ');
    }
    out.push_str("fn ");
    out.push_str(&parts.name);
    out.push('(');
    for (i, p) in parts.params.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        match (&p.name, &p.type_text) {
            (Some(n), Some(t)) => {
                out.push_str(n);
                out.push_str(": ");
                out.push_str(t);
            }
            (Some(n), None) => out.push_str(n),
            (None, Some(t)) => out.push_str(t),
            (None, None) => out.push('_'),
        }
    }
    out.push(')');
    if let Some(ret) = &parts.return_type {
        out.push_str(" -> ");
        out.push_str(ret);
    }
    out
}

/// Format the risk-bearing shape (names excluded; unannotated → `_`).
fn format_shape(parts: &SymbolSignatureParts) -> String {
    let mods = parts.modifiers.join(",");
    let arity = parts.params.len();
    let param_types: Vec<&str> = parts
        .params
        .iter()
        .map(|p| p.type_text.as_deref().unwrap_or("_"))
        .collect();
    let ret = parts.return_type.as_deref().unwrap_or("_");
    format!(
        "mods={mods};arity={arity};params={params};ret={ret}",
        params = param_types.join(",")
    )
}

/// Classify a before/after pair.
///
/// - Identical text and shape → `None` (body-only / no signature change)
/// - Same shape, different text → `Cosmetic` (rename)
/// - Different shape → `Shape`
/// - Either side empty/malformed with no usable shape → `Unknown`
pub fn classify_signature_change(
    previous: &SymbolSignature,
    current: &SymbolSignature,
) -> Option<SignatureChangeClass> {
    if previous.text == current.text && previous.shape == current.shape {
        return None;
    }
    if previous.shape.is_empty() || current.shape.is_empty() {
        return Some(SignatureChangeClass::Unknown);
    }
    if previous.shape == current.shape {
        return Some(SignatureChangeClass::Cosmetic);
    }
    Some(SignatureChangeClass::Shape)
}

/// Metadata keys written by language extractors (camelCase to match packet serde).
pub const METADATA_SIGNATURE: &str = "signature";
pub const METADATA_SIGNATURE_SHAPE: &str = "signatureShape";

/// Insert signature + shape into a symbol metadata map.
pub fn write_signature_metadata(
    metadata: &mut std::collections::BTreeMap<String, String>,
    sig: &SymbolSignature,
) {
    metadata.insert(METADATA_SIGNATURE.to_string(), sig.text.clone());
    metadata.insert(METADATA_SIGNATURE_SHAPE.to_string(), sig.shape.clone());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parts(
        name: &str,
        mods: &[&str],
        params: &[(&str, Option<&str>)],
        ret: Option<&str>,
    ) -> SymbolSignatureParts {
        SymbolSignatureParts {
            name: name.to_string(),
            modifiers: mods.iter().map(|s| s.to_string()).collect(),
            params: params
                .iter()
                .map(|(n, t)| SignatureParam {
                    name: Some(n.to_string()),
                    type_text: t.map(|s| s.to_string()),
                })
                .collect(),
            return_type: ret.map(|s| s.to_string()),
        }
    }

    #[test]
    fn rename_only_is_cosmetic() {
        // Write rename-only and body-only first — they make the signal trustworthy.
        let a = build_symbol_signature(&parts("foo", &[], &[("a", Some("u32"))], Some("u64")));
        let b = build_symbol_signature(&parts("foo", &[], &[("x", Some("u32"))], Some("u64")));
        assert_eq!(a.shape, b.shape, "shape must exclude parameter names");
        assert_ne!(a.text, b.text);
        assert_eq!(
            classify_signature_change(&a, &b),
            Some(SignatureChangeClass::Cosmetic)
        );
    }

    #[test]
    fn body_only_yields_no_change() {
        let a = build_symbol_signature(&parts("foo", &[], &[("a", Some("u32"))], Some("u64")));
        let b = a.clone();
        assert_eq!(classify_signature_change(&a, &b), None);
    }

    #[test]
    fn shape_change_on_param_type() {
        let a = build_symbol_signature(&parts("foo", &[], &[("a", Some("u32"))], Some("u64")));
        let b = build_symbol_signature(&parts("foo", &[], &[("a", Some("u64"))], Some("u64")));
        assert_ne!(a.shape, b.shape);
        assert_eq!(
            classify_signature_change(&a, &b),
            Some(SignatureChangeClass::Shape)
        );
    }

    #[test]
    fn shape_change_on_arity() {
        let a = build_symbol_signature(&parts("foo", &[], &[("a", Some("u32"))], None));
        let b = build_symbol_signature(&parts(
            "foo",
            &[],
            &[("a", Some("u32")), ("b", Some("u32"))],
            None,
        ));
        assert_eq!(
            classify_signature_change(&a, &b),
            Some(SignatureChangeClass::Shape)
        );
    }

    #[test]
    fn async_modifier_moves_shape() {
        // In Rust the AST return_type is unchanged by `async`; modifiers must be in the shape.
        let sync_fn = build_symbol_signature(&parts("foo", &[], &[], None));
        let async_fn = build_symbol_signature(&parts("foo", &["async"], &[], None));
        assert_ne!(
            sync_fn.shape, async_fn.shape,
            "async must change signature_shape"
        );
        assert_eq!(
            classify_signature_change(&sync_fn, &async_fn),
            Some(SignatureChangeClass::Shape)
        );
        assert!(async_fn.shape.contains("async"));
        assert!(async_fn.text.starts_with("async "));
    }

    #[test]
    fn unknown_types_normalize_to_underscore() {
        let sig = build_symbol_signature(&parts("foo", &[], &[("a", None), ("b", None)], None));
        assert!(sig.shape.contains("params=_,_"));
        assert!(sig.shape.contains("arity=2"));
        assert!(sig.shape.contains("ret=_"));
        // Readable keeps names when present.
        assert!(sig.text.contains("a") && sig.text.contains("b"));
    }

    #[test]
    fn empty_shape_classifies_unknown() {
        let a = SymbolSignature {
            text: "fn a()".into(),
            shape: String::new(),
        };
        let b = SymbolSignature {
            text: "fn b()".into(),
            shape: "mods=;arity=0;params=;ret=_".into(),
        };
        assert_eq!(
            classify_signature_change(&a, &b),
            Some(SignatureChangeClass::Unknown)
        );
    }

    #[test]
    fn shape_excludes_parameter_names() {
        let sig = build_symbol_signature(&parts(
            "greet",
            &["async"],
            &[("name", Some("String")), ("age", Some("u32"))],
            Some("bool"),
        ));
        assert!(
            !sig.shape.contains("name") && !sig.shape.contains("age"),
            "shape must not contain param names: {}",
            sig.shape
        );
        assert!(sig.shape.contains("params=String,u32"));
        assert!(sig.text.contains("name: String"));
        assert!(sig.text.contains("age: u32"));
    }

    #[test]
    fn write_metadata_uses_canonical_keys() {
        let sig = build_symbol_signature(&parts("f", &[], &[], Some("()")));
        let mut meta = std::collections::BTreeMap::new();
        write_signature_metadata(&mut meta, &sig);
        assert!(meta.contains_key(METADATA_SIGNATURE));
        assert!(meta.contains_key(METADATA_SIGNATURE_SHAPE));
        assert_eq!(meta.get(METADATA_SIGNATURE).unwrap(), &sig.text);
        assert_eq!(meta.get(METADATA_SIGNATURE_SHAPE).unwrap(), &sig.shape);
    }
}
