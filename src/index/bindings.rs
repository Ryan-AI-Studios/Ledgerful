//! File-scope binding rows for call resolution (0092 Part 1).
//!
//! A binding answers "does segment `X` enter this file's scope, and is it
//! local?" Keys are **bound names** (aliases / last segments), not raw import
//! paths. Wildcards are stored as non-enumerable and never prove locality.

use serde::{Deserialize, Serialize};

/// One binding that enters a file's scope (pre-persist; no file_id yet).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct FileBinding {
    /// Name that enters scope (alias or last path segment; `*` for wildcards).
    pub bound_name: String,
    /// Original import / module path text (e.g. `std::fs`, `crate::util::fs`).
    pub source_path: String,
    /// `use` | `mod` | `mod_inline` | `import` | `from_import` | …
    pub binding_kind: String,
    /// 0 for wildcards — never proves locality for any segment.
    pub is_enumerable: bool,
    /// 1 when proven local (`mod` decl; `use` of `crate`/`self`/`super` path).
    pub is_local: bool,
}

/// Runtime view used by [`crate::index::resolve::resolve_callee`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BindingInfo {
    pub source_path: String,
    pub binding_kind: String,
    pub is_enumerable: bool,
    pub is_local: bool,
}

/// Sort bindings deterministically for insert (DoD-7).
pub fn sort_bindings(bindings: &mut [FileBinding]) {
    bindings.sort_by(|a, b| {
        a.bound_name
            .cmp(&b.bound_name)
            .then(a.source_path.cmp(&b.source_path))
            .then(a.binding_kind.cmp(&b.binding_kind))
    });
}

/// Whether a Rust `use` source path is proven local (crate/self/super rooted).
pub fn rust_use_is_local(source_path: &str) -> bool {
    let p = source_path.trim();
    p == "crate"
        || p == "self"
        || p == "super"
        || p.starts_with("crate::")
        || p.starts_with("self::")
        || p.starts_with("super::")
}
