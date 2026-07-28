//! Derive a Rust-style module path from a source file path (0092 Part 1).
//!
//! Pure function over paths — no DB, no index. Used by call-graph resolution
//! to expand `crate::` / `self::` / `super::` and to place candidate symbols
//! under their declaring module.

use std::path::{Component, Path};

/// Source-root directory names recognized when deriving module paths.
const SOURCE_ROOTS: &[&str] = &["src", "lib", "bin"];

/// Derive `crate::…` module path for a file relative to the repository root.
///
/// Examples (relative path, assumed under a Rust source root):
/// - `src/platform/urn.rs` → `Some("crate::platform::urn")`
/// - `src/index/mod.rs` → `Some("crate::index")`
/// - `src/lib.rs` / `src/main.rs` → `Some("crate")`
/// - `README.md` or path outside a source root → `None`
///
/// `source_root` is optional; when `None`, the first path component matching
/// a known source root (`src`/`lib`/`bin`) is treated as the root and stripped.
/// When `Some("src")`, only paths under that root are accepted.
pub fn derive_module_path(file_path: &str, source_root: Option<&str>) -> Option<String> {
    let normalized = file_path.replace('\\', "/");
    let path = Path::new(&normalized);

    let components: Vec<&str> = path
        .components()
        .filter_map(|c| match c {
            Component::Normal(s) => s.to_str(),
            _ => None,
        })
        .collect();

    if components.is_empty() {
        return None;
    }

    // Locate and strip the source root segment.
    let after_root: &[&str] = if let Some(root) = source_root {
        let root_norm = root.trim_matches('/').replace('\\', "/");
        let root_parts: Vec<&str> = root_norm.split('/').filter(|s| !s.is_empty()).collect();
        if components.len() < root_parts.len() {
            return None;
        }
        if components[..root_parts.len()]
            .iter()
            .zip(root_parts.iter())
            .any(|(a, b)| *a != *b)
        {
            return None;
        }
        &components[root_parts.len()..]
    } else {
        let root_idx = components.iter().position(|c| SOURCE_ROOTS.contains(c))?;
        &components[root_idx + 1..]
    };

    if after_root.is_empty() {
        // Bare `src/` with no file — not a module.
        return None;
    }

    let file_name = *after_root.last()?;
    let parent_segs = &after_root[..after_root.len() - 1];

    // Stem without extension.
    let stem = Path::new(file_name)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(file_name);

    let mut segs: Vec<&str> = parent_segs.to_vec();

    // mod.rs / lib.rs / main.rs collapse into the parent directory module.
    match stem {
        "mod" | "lib" | "main" => {
            // segs already are the parent path; empty → crate root.
        }
        _ => {
            segs.push(stem);
        }
    }

    if segs.is_empty() {
        return Some("crate".to_string());
    }

    let mut out = String::from("crate");
    for s in segs {
        if s.is_empty() {
            continue;
        }
        out.push_str("::");
        out.push_str(s);
    }
    Some(out)
}

/// Normalize a module path to dotted form for comparison with callees.
pub fn module_path_to_dots(module_path: &str) -> String {
    module_path.replace("::", ".")
}

/// Expand a path-qualified callee rooted at `crate` / `self` / `super` into an
/// absolute module path (dotted) plus remaining segments for the symbol.
///
/// Returns `None` when expansion is impossible (e.g. `super` without a caller
/// module path, or too many `super` segments).
pub fn expand_rooted_callee(
    segments: &[&str],
    caller_module_path: Option<&str>,
) -> Option<(String, Vec<String>)> {
    if segments.is_empty() {
        return None;
    }

    let first = segments[0];
    let rest: Vec<String> = segments[1..].iter().map(|s| (*s).to_string()).collect();

    match first {
        "crate" => {
            // Absolute from crate root: remaining segs are module… + symbol.
            if rest.is_empty() {
                return None;
            }
            Some(("crate".to_string(), rest))
        }
        "self" => {
            let base = caller_module_path?;
            if rest.is_empty() {
                return None;
            }
            Some((base.to_string(), rest))
        }
        "super" => {
            let base = caller_module_path?;
            // Count leading super segments.
            let mut supers = 1usize;
            let mut idx = 1usize;
            while idx < segments.len() && segments[idx] == "super" {
                supers += 1;
                idx += 1;
            }
            let remaining: Vec<String> = segments[idx..].iter().map(|s| (*s).to_string()).collect();
            if remaining.is_empty() {
                return None;
            }
            let mut base_segs: Vec<&str> = base.split("::").collect();
            if base_segs.is_empty() || base_segs[0] != "crate" {
                return None;
            }
            for _ in 0..supers {
                if base_segs.len() <= 1 {
                    // Cannot go above crate.
                    return None;
                }
                base_segs.pop();
            }
            Some((base_segs.join("::"), remaining))
        }
        _ => None,
    }
}

/// Split absolute module base + remaining path segs into (module_path, symbol lookup).
///
/// For remaining `[a, b, c]`:
/// - free-fn try: module = base::a::b, bare = c
/// - method try: module = base::a, qn = b.c
///
/// Returns candidates to try in order: `(module_path, bare_or_qn, is_qn)`.
pub fn module_symbol_lookups(
    base_module: &str,
    remaining: &[String],
) -> Vec<(String, String, bool)> {
    let mut out = Vec::new();
    if remaining.is_empty() {
        return out;
    }

    // Free function / single last segment: full module + bare name.
    if remaining.len() == 1 {
        out.push((base_module.to_string(), remaining[0].clone(), false));
        return out;
    }

    // module = base + all but last; bare = last
    {
        let mut mod_segs: Vec<&str> = if base_module.is_empty() {
            Vec::new()
        } else {
            base_module.split("::").collect()
        };
        for s in &remaining[..remaining.len() - 1] {
            mod_segs.push(s.as_str());
        }
        let module = mod_segs.join("::");
        out.push((module, remaining[remaining.len() - 1].clone(), false));
    }

    // Type.method: module = base + all but last two; qn = Type.method
    if remaining.len() >= 2 {
        let mut mod_segs: Vec<&str> = if base_module.is_empty() {
            Vec::new()
        } else {
            base_module.split("::").collect()
        };
        for s in &remaining[..remaining.len() - 2] {
            mod_segs.push(s.as_str());
        }
        let module = mod_segs.join("::");
        let qn = format!(
            "{}.{}",
            remaining[remaining.len() - 2],
            remaining[remaining.len() - 1]
        );
        out.push((module, qn, true));
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_module_path() {
        assert_eq!(
            derive_module_path("src/platform/urn.rs", None).as_deref(),
            Some("crate::platform::urn")
        );
        assert_eq!(
            derive_module_path("src/index/languages/rust/calls.rs", None).as_deref(),
            Some("crate::index::languages::rust::calls")
        );
    }

    #[test]
    fn mod_rs_collapses() {
        assert_eq!(
            derive_module_path("src/index/mod.rs", None).as_deref(),
            Some("crate::index")
        );
        assert_eq!(
            derive_module_path("src/util/mod.rs", None).as_deref(),
            Some("crate::util")
        );
    }

    #[test]
    fn lib_and_main_are_crate_root() {
        assert_eq!(
            derive_module_path("src/lib.rs", None).as_deref(),
            Some("crate")
        );
        assert_eq!(
            derive_module_path("src/main.rs", None).as_deref(),
            Some("crate")
        );
    }

    #[test]
    fn outside_source_root_returns_none() {
        assert_eq!(derive_module_path("README.md", None), None);
        assert_eq!(derive_module_path("docs/Call-Resolution.md", None), None);
        assert_eq!(derive_module_path("tests/integration/foo.rs", None), None);
    }

    #[test]
    fn explicit_source_root() {
        assert_eq!(
            derive_module_path("src/foo/bar.rs", Some("src")).as_deref(),
            Some("crate::foo::bar")
        );
        assert_eq!(derive_module_path("lib/foo.rs", Some("src")), None);
    }

    #[test]
    fn windows_separators_normalized() {
        assert_eq!(
            derive_module_path("src\\platform\\urn.rs", None).as_deref(),
            Some("crate::platform::urn")
        );
    }

    #[test]
    fn expand_crate_self_super() {
        let (base, rem) =
            expand_rooted_callee(&["crate", "platform", "urn", "build_urn"], None).unwrap();
        assert_eq!(base, "crate");
        assert_eq!(rem, vec!["platform", "urn", "build_urn"]);

        let (base, rem) =
            expand_rooted_callee(&["self", "helper"], Some("crate::index::resolve")).unwrap();
        assert_eq!(base, "crate::index::resolve");
        assert_eq!(rem, vec!["helper"]);

        let (base, rem) =
            expand_rooted_callee(&["super", "helper"], Some("crate::index::resolve")).unwrap();
        assert_eq!(base, "crate::index");
        assert_eq!(rem, vec!["helper"]);

        assert!(
            expand_rooted_callee(&["super", "x"], Some("crate")).is_none(),
            "cannot super above crate"
        );
        assert!(expand_rooted_callee(&["self", "x"], None).is_none());
    }

    #[test]
    fn module_symbol_lookups_free_and_method() {
        let lookups = module_symbol_lookups(
            "crate",
            &["platform".into(), "urn".into(), "build_urn".into()],
        );
        assert_eq!(lookups[0].0, "crate::platform::urn");
        assert_eq!(lookups[0].1, "build_urn");
        assert!(!lookups[0].2);
        assert_eq!(lookups[1].0, "crate::platform");
        assert_eq!(lookups[1].1, "urn.build_urn");
        assert!(lookups[1].2);

        let lookups = module_symbol_lookups(
            "crate",
            &[
                "config".into(),
                "model".into(),
                "Config".into(),
                "default".into(),
            ],
        );
        assert_eq!(lookups[0].0, "crate::config::model::Config");
        assert_eq!(lookups[0].1, "default");
        assert_eq!(lookups[1].0, "crate::config::model");
        assert_eq!(lookups[1].1, "Config.default");
    }
}
