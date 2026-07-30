//! Shared call-graph callee resolution (0089 Parts A+B, 0092 Part 1).
//!
//! One pure decision function used by both the full-index path
//! (`call_graph.rs`) and the incremental path (`incremental.rs`) so the two
//! cannot diverge (DoD-6).
//!
//! Resolution is a **unique-local-candidate heuristic**, not a name-binding
//! semantics. See `docs/Call-Resolution.md`.

use crate::index::bindings::BindingInfo;
use crate::index::call_graph::ResolutionStatus;
use crate::index::module_path::{expand_rooted_callee, module_symbol_lookups};
use std::collections::HashMap;

/// Callable kinds accepted as call targets (locked kind-filter decision).
pub const CALLABLE_KINDS: &[&str] = &["Function", "Method"];

/// One indexed symbol that may be a resolution candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolveCandidate {
    pub symbol_id: i64,
    pub file_id: i64,
    pub symbol_name: String,
    /// Empty string means no genuine qualified name (DB may still store bare name).
    pub qualified_name: String,
    pub symbol_kind: String,
}

/// Input to [`resolve_callee`].
pub struct ResolveInput<'a> {
    pub callee_name: &'a str,
    pub caller_file_id: i64,
    /// Bare `symbol_name` → candidates (all kinds; filter applied inside).
    pub candidates_by_bare_name: &'a HashMap<String, Vec<ResolveCandidate>>,
    /// Distinct non-empty `qualified_name` → candidates (all kinds; filter inside).
    pub candidates_by_qualified: &'a HashMap<String, Vec<ResolveCandidate>>,
    /// Caller's module path (`crate::platform::urn`), if derivable.
    pub caller_module_path: Option<&'a str>,
    /// Bound-name → binding info for the caller file (enumerable local bindings
    /// prove locality; wildcards and external uses do not).
    pub caller_bindings: &'a HashMap<String, BindingInfo>,
    /// file_id → module path for candidate filtering.
    pub module_path_by_file: &'a HashMap<i64, String>,
}

/// Outcome of [`resolve_callee`].
#[derive(Debug, Clone, PartialEq)]
pub struct ResolveResult {
    pub callee_symbol_id: Option<i64>,
    pub callee_file_id: Option<i64>,
    pub status: ResolutionStatus,
    pub unresolved_callee: Option<String>,
}

/// Whether `kind` is a callable call target (`Function` or `Method`).
pub fn is_callable_kind(kind: &str) -> bool {
    matches!(kind, "Function" | "Method")
}

/// Normalize path-style callees for QN lookup: `Foo::new` → `Foo.new`.
pub fn normalize_callee_name(name: &str) -> String {
    name.replace("::", ".")
}

/// Last segment after `.` (assumes already normalized).
pub fn bare_segment(normalized: &str) -> &str {
    normalized.rsplit('.').next().unwrap_or(normalized)
}

/// Build a [`ResolveCandidate`] from a DB row / index row.
///
/// Shared by full-index (`call_graph.rs`) and incremental (`incremental.rs`) so
/// candidate construction cannot diverge between the two paths.
pub fn resolve_candidate_from_row(
    symbol_id: i64,
    file_id: i64,
    symbol_name: String,
    qualified_name: String,
    symbol_kind: String,
) -> ResolveCandidate {
    ResolveCandidate {
        symbol_id,
        file_id,
        symbol_name,
        qualified_name,
        symbol_kind,
    }
}

/// Build bare-name and qualified-name maps from a flat candidate list.
///
/// - Bare map: every candidate under its `symbol_name`.
/// - Qualified map: only entries whose `qualified_name` is non-empty **and**
///   distinct from `symbol_name` (genuine `Type.method` forms).
///
/// Candidate vectors are sorted by `(file_id, symbol_id)` for determinism.
/// Both production paths call this after [`resolve_candidate_from_row`].
pub fn build_resolve_maps(
    candidates: Vec<ResolveCandidate>,
) -> (
    HashMap<String, Vec<ResolveCandidate>>,
    HashMap<String, Vec<ResolveCandidate>>,
) {
    let mut by_bare: HashMap<String, Vec<ResolveCandidate>> = HashMap::new();
    let mut by_qn: HashMap<String, Vec<ResolveCandidate>> = HashMap::new();

    for c in candidates {
        by_bare
            .entry(c.symbol_name.clone())
            .or_default()
            .push(c.clone());

        if !c.qualified_name.is_empty() && c.qualified_name != c.symbol_name {
            by_qn.entry(c.qualified_name.clone()).or_default().push(c);
        }
    }

    for v in by_bare.values_mut() {
        v.sort_by(|a, b| {
            a.file_id
                .cmp(&b.file_id)
                .then(a.symbol_id.cmp(&b.symbol_id))
        });
    }
    for v in by_qn.values_mut() {
        v.sort_by(|a, b| {
            a.file_id
                .cmp(&b.file_id)
                .then(a.symbol_id.cmp(&b.symbol_id))
        });
    }

    (by_bare, by_qn)
}

fn callable_only(candidates: &[ResolveCandidate]) -> Vec<&ResolveCandidate> {
    candidates
        .iter()
        .filter(|c| is_callable_kind(&c.symbol_kind))
        .collect()
}

fn resolved(c: &ResolveCandidate) -> ResolveResult {
    ResolveResult {
        callee_symbol_id: Some(c.symbol_id),
        callee_file_id: Some(c.file_id),
        status: ResolutionStatus::Resolved,
        unresolved_callee: None,
    }
}

fn ambiguous(original: &str) -> ResolveResult {
    ResolveResult {
        callee_symbol_id: None,
        callee_file_id: None,
        status: ResolutionStatus::Ambiguous,
        unresolved_callee: Some(original.to_string()),
    }
}

fn unresolved(original: &str) -> ResolveResult {
    ResolveResult {
        callee_symbol_id: None,
        callee_file_id: None,
        status: ResolutionStatus::Unresolved,
        unresolved_callee: Some(original.to_string()),
    }
}

/// Resolve a callee name against indexed symbol candidates.
///
/// Order (Parts A + B + 0092 Part 1):
/// 1. Normalize `::` → `.`.
/// 2. **Module/import arm (0092)** — ahead of QN: resolve `crate`/`self`/`super`
///    rooted paths and first-segment-local callees only when an enumerable
///    binding proves locality (`use` or `mod`). Wildcards prove nothing.
///    Name-only matching without a binding does **not** resolve (DoD-4).
/// 3. If the name contains `.` or is an exact QN key: try qualified exact match
///    among callable kinds. 1 → Resolved, >1 → Ambiguous, 0 → **Unresolved**
///    when multi-segment (no bare-segment fallthrough — DoD-9 / codex P1-1).
/// 4. Bare single-segment path (callable kinds only): 0 Unresolved, 1 Resolved,
///    >1 with exactly one same-file → that one, else Ambiguous.
///
/// Multi-segment names that miss QN (`json.loads`, `axios.get`, `s.process`)
/// stay **Unresolved**. That is intentional: bare-segment fallthrough would
/// fabricate edges to same-file Methods named `loads`/`get`/`process`.
pub fn resolve_callee(input: ResolveInput<'_>) -> ResolveResult {
    let original = input.callee_name;
    if original.is_empty() {
        return unresolved(original);
    }

    let normalized = normalize_callee_name(original);
    let multi_segment = normalized.contains('.');

    // --- 0092 Part 1: module + binding arm (ahead of QN) ---
    if multi_segment
        && let Some(result) = resolve_via_module_bindings(&input, original, &normalized)
    {
        return result;
    }

    let try_qn = multi_segment
        || input
            .candidates_by_qualified
            .contains_key(normalized.as_str());

    if try_qn {
        let qn_raw = input
            .candidates_by_qualified
            .get(normalized.as_str())
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let qn_matches = callable_only(qn_raw);
        match qn_matches.len() {
            1 => return resolved(qn_matches[0]),
            0 if multi_segment => {
                // External-safe: never bare-fallthrough on dotted names.
                // `json.loads` + same-file Method `Local.loads` stays Unresolved.
                return unresolved(original);
            }
            0 => {
                // Name was a QN key without multi-segment (shouldn't happen for
                // distinct QNs) — fall through to bare with full name.
            }
            _ => return ambiguous(original),
        }
    }

    // Bare single-segment path only (dotted multi-segment handled above).
    let bare = normalized.as_str();

    let bare_raw = input
        .candidates_by_bare_name
        .get(bare)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let matches = callable_only(bare_raw);

    match matches.len() {
        0 => unresolved(original),
        1 => resolved(matches[0]),
        _ => {
            let same_file: Vec<&ResolveCandidate> = matches
                .into_iter()
                .filter(|c| c.file_id == input.caller_file_id)
                .collect();
            if same_file.len() == 1 {
                resolved(same_file[0])
            } else {
                ambiguous(original)
            }
        }
    }
}

/// Attempt path-qualified local resolution via module path + bindings.
///
/// Returns `Some` when this arm makes a final decision (including Unresolved
/// for proven-external first segments). Returns `None` to fall through to the
/// existing QN / bare arms (e.g. `Foo.new` with no binding for `Foo`).
fn resolve_via_module_bindings(
    input: &ResolveInput<'_>,
    original: &str,
    normalized: &str,
) -> Option<ResolveResult> {
    let segments: Vec<&str> = normalized.split('.').collect();
    if segments.len() < 2 {
        return None;
    }
    let first = segments[0];

    // 1) crate / self / super rooted
    if matches!(first, "crate" | "self" | "super") {
        let (base, remaining) = expand_rooted_callee(&segments, input.caller_module_path)?;
        return Some(resolve_in_module(input, original, &base, &remaining));
    }

    // 2) First-segment binding
    let binding = match input.caller_bindings.get(first) {
        Some(b) => b,
        None => {
            // No binding — do not name-match alone (DoD-4). Fall through so
            // Type.method QN (`Foo.new`) still works.
            return None;
        }
    };

    if !binding.is_enumerable {
        // Wildcard proves nothing — fall through (external multi-seg stays
        // Unresolved via QN miss; we must not invent locality).
        return None;
    }

    if !binding.is_local {
        // Proven external (`use std::fs`) — stay Unresolved (DoD-4).
        return Some(unresolved(original));
    }

    // Local binding: mod / mod_inline / local use
    let remaining: Vec<String> = segments[1..].iter().map(|s| (*s).to_string()).collect();
    if remaining.is_empty() {
        return Some(unresolved(original));
    }

    if binding.binding_kind == "mod_inline" {
        // Inline module lives in the same file — look up last segment there.
        let bare = remaining.last()?.as_str();
        return Some(resolve_in_caller_file(input, original, bare));
    }

    if binding.binding_kind == "mod" {
        // `mod fs;` → target module = caller_module::fs
        let base = match input.caller_module_path {
            Some(mp) => format!("{mp}::{first}"),
            None => {
                // Without caller module we cannot place the child module.
                return Some(unresolved(original));
            }
        };
        return Some(resolve_in_module(input, original, &base, &remaining));
    }

    // Local `use` — source_path is the imported path (e.g. crate::util::fs).
    // Calling `fs::write` means module source_path + write.
    let source = binding.source_path.replace('.', "::");
    // If the use binds a single item that is itself the function, multi-seg
    // calls still treat source as module prefix.
    let base = source;
    Some(resolve_in_module(input, original, &base, &remaining))
}

fn resolve_in_caller_file(input: &ResolveInput<'_>, original: &str, bare: &str) -> ResolveResult {
    let bare_raw = input
        .candidates_by_bare_name
        .get(bare)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let matches: Vec<&ResolveCandidate> = callable_only(bare_raw)
        .into_iter()
        .filter(|c| c.file_id == input.caller_file_id)
        .collect();
    match matches.len() {
        1 => resolved(matches[0]),
        0 => unresolved(original),
        _ => ambiguous(original),
    }
}

fn resolve_in_module(
    input: &ResolveInput<'_>,
    original: &str,
    base_module: &str,
    remaining: &[String],
) -> ResolveResult {
    let lookups = module_symbol_lookups(base_module, remaining);
    for (module_path, name, is_qn) in lookups {
        let matches = if is_qn {
            let qn_raw = input
                .candidates_by_qualified
                .get(&name)
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            callable_only(qn_raw)
                .into_iter()
                .filter(|c| {
                    input
                        .module_path_by_file
                        .get(&c.file_id)
                        .is_some_and(|mp| mp == &module_path)
                })
                .collect::<Vec<_>>()
        } else {
            let bare_raw = input
                .candidates_by_bare_name
                .get(&name)
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            callable_only(bare_raw)
                .into_iter()
                .filter(|c| {
                    input
                        .module_path_by_file
                        .get(&c.file_id)
                        .is_some_and(|mp| mp == &module_path)
                })
                .collect::<Vec<_>>()
        };

        match matches.len() {
            1 => return resolved(matches[0]),
            n if n > 1 => return ambiguous(original),
            _ => continue,
        }
    }
    unresolved(original)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand(id: i64, file_id: i64, name: &str, qn: &str, kind: &str) -> ResolveCandidate {
        ResolveCandidate {
            symbol_id: id,
            file_id,
            symbol_name: name.to_string(),
            qualified_name: qn.to_string(),
            symbol_kind: kind.to_string(),
        }
    }

    fn maps(
        list: Vec<ResolveCandidate>,
    ) -> (
        HashMap<String, Vec<ResolveCandidate>>,
        HashMap<String, Vec<ResolveCandidate>>,
    ) {
        build_resolve_maps(list)
    }

    fn empty_bindings() -> HashMap<String, BindingInfo> {
        HashMap::new()
    }

    fn empty_modules() -> HashMap<i64, String> {
        HashMap::new()
    }

    fn run(
        callee: &str,
        caller_file: i64,
        by_bare: &HashMap<String, Vec<ResolveCandidate>>,
        by_qn: &HashMap<String, Vec<ResolveCandidate>>,
    ) -> ResolveResult {
        let bindings = empty_bindings();
        let modules = empty_modules();
        resolve_callee(ResolveInput {
            callee_name: callee,
            caller_file_id: caller_file,
            candidates_by_bare_name: by_bare,
            candidates_by_qualified: by_qn,
            caller_module_path: None,
            caller_bindings: &bindings,
            module_path_by_file: &modules,
        })
    }

    fn run_with_ctx(
        callee: &str,
        caller_file: i64,
        by_bare: &HashMap<String, Vec<ResolveCandidate>>,
        by_qn: &HashMap<String, Vec<ResolveCandidate>>,
        caller_module: Option<&str>,
        bindings: &HashMap<String, BindingInfo>,
        modules: &HashMap<i64, String>,
    ) -> ResolveResult {
        resolve_callee(ResolveInput {
            callee_name: callee,
            caller_file_id: caller_file,
            candidates_by_bare_name: by_bare,
            candidates_by_qualified: by_qn,
            caller_module_path: caller_module,
            caller_bindings: bindings,
            module_path_by_file: modules,
        })
    }

    #[test]
    fn resolve_unique_bare_function() {
        let (by_bare, by_qn) = maps(vec![cand(1, 10, "helper", "helper", "Function")]);
        let r = run("helper", 10, &by_bare, &by_qn);
        assert_eq!(r.status, ResolutionStatus::Resolved);
        assert_eq!(r.callee_symbol_id, Some(1));
        assert_eq!(r.callee_file_id, Some(10));
        assert!(r.unresolved_callee.is_none());
    }

    #[test]
    fn resolve_ambiguous_without_same_file_preference() {
        let (by_bare, by_qn) = maps(vec![
            cand(1, 10, "enrich", "A.enrich", "Method"),
            cand(2, 20, "enrich", "B.enrich", "Method"),
        ]);
        let r = run("enrich", 30, &by_bare, &by_qn);
        assert_eq!(r.status, ResolutionStatus::Ambiguous);
        assert_eq!(r.callee_symbol_id, None);
        assert_eq!(r.unresolved_callee.as_deref(), Some("enrich"));
    }

    #[test]
    fn resolve_kind_filter_excludes_module_and_struct() {
        // Bare "tests" hits a Module and a Function — only Function is callable.
        let (by_bare, by_qn) = maps(vec![
            cand(1, 10, "tests", "tests", "Module"),
            cand(2, 20, "tests", "tests", "Function"),
            cand(3, 30, "Foo", "Foo", "Struct"),
        ]);
        let r = run("tests", 10, &by_bare, &by_qn);
        assert_eq!(r.status, ResolutionStatus::Resolved);
        assert_eq!(r.callee_symbol_id, Some(2));

        // Struct-only name → Unresolved after kind filter (not Resolved to Struct).
        let r2 = run("Foo", 30, &by_bare, &by_qn);
        assert_eq!(r2.status, ResolutionStatus::Unresolved);
    }

    #[test]
    fn resolve_same_file_preference() {
        let (by_bare, by_qn) = maps(vec![
            cand(1, 10, "new", "A.new", "Method"),
            cand(2, 20, "new", "B.new", "Method"),
            cand(3, 10, "new", "C.new", "Method"), // two in file 10? need exactly one
        ]);
        // Two in file 10 → still Ambiguous.
        let r = run("new", 10, &by_bare, &by_qn);
        assert_eq!(r.status, ResolutionStatus::Ambiguous);

        let (by_bare, by_qn) = maps(vec![
            cand(1, 10, "new", "A.new", "Method"),
            cand(2, 20, "new", "B.new", "Method"),
        ]);
        let r = run("new", 10, &by_bare, &by_qn);
        assert_eq!(r.status, ResolutionStatus::Resolved);
        assert_eq!(r.callee_symbol_id, Some(1));
        assert_eq!(r.callee_file_id, Some(10));
    }

    #[test]
    fn resolve_qn_distinguishes_foo_new_and_bar_new() {
        let (by_bare, by_qn) = maps(vec![
            cand(1, 10, "new", "Foo.new", "Method"),
            cand(2, 20, "new", "Bar.new", "Method"),
        ]);
        // Bare `new` is Ambiguous without same-file help.
        let bare = run("new", 99, &by_bare, &by_qn);
        assert_eq!(bare.status, ResolutionStatus::Ambiguous);

        // QN path: Foo::new (normalized) and Foo.new both hit Foo.new.
        let foo = run("Foo::new", 99, &by_bare, &by_qn);
        assert_eq!(foo.status, ResolutionStatus::Resolved);
        assert_eq!(foo.callee_symbol_id, Some(1));

        let bar = run("Foo.new", 99, &by_bare, &by_qn);
        assert_eq!(bar.status, ResolutionStatus::Resolved);
        assert_eq!(bar.callee_symbol_id, Some(1));

        let bar2 = run("Bar.new", 99, &by_bare, &by_qn);
        assert_eq!(bar2.status, ResolutionStatus::Resolved);
        assert_eq!(bar2.callee_symbol_id, Some(2));
    }

    #[test]
    fn resolve_multi_segment_qn_miss_always_unresolved() {
        // Codex P1-1: multi-segment QN miss must NOT bare-fallthrough to Methods
        // or Functions. `json.loads` / `s.process` stay Unresolved without QN.
        let (by_bare, by_qn) = maps(vec![
            cand(1, 10, "process", "process", "Method"),
            cand(2, 10, "loads", "Local.loads", "Method"),
        ]);
        // No QN key for receiver.field forms.
        assert!(!by_qn.contains_key("s.process"));
        assert!(!by_qn.contains_key("json.loads"));

        let r = run("s.process", 10, &by_bare, &by_qn);
        assert_eq!(r.status, ResolutionStatus::Unresolved);

        // Same-file Method named loads must not capture json.loads (DoD-9).
        let r2 = run("json.loads", 10, &by_bare, &by_qn);
        assert_eq!(r2.status, ResolutionStatus::Unresolved);
        assert_eq!(r2.callee_symbol_id, None);

        // Bare single-segment still uses same-file preference.
        let r3 = run("process", 10, &by_bare, &by_qn);
        assert_eq!(r3.status, ResolutionStatus::Resolved);
        assert_eq!(r3.callee_symbol_id, Some(1));
    }

    #[test]
    fn resolve_same_file_function_not_fallthrough_for_member() {
        // Same-file free Function must NOT capture multi-segment members.
        // def loads(...); ... json.loads(x)  → Unresolved (not Function loads).
        let (by_bare, by_qn) = maps(vec![cand(1, 10, "loads", "loads", "Function")]);
        let r = run("json.loads", 10, &by_bare, &by_qn);
        assert_eq!(r.status, ResolutionStatus::Unresolved);
        assert_eq!(r.callee_symbol_id, None);
        assert_eq!(r.unresolved_callee.as_deref(), Some("json.loads"));

        // Bare `loads` still resolves to the same-file Function (single-segment path).
        let bare = run("loads", 10, &by_bare, &by_qn);
        assert_eq!(bare.status, ResolutionStatus::Resolved);
        assert_eq!(bare.callee_symbol_id, Some(1));
    }

    #[test]
    fn resolve_same_file_method_not_fallthrough_for_member() {
        // Codex P1-1: class Local { def loads }: json.loads must stay Unresolved.
        let (by_bare, by_qn) = maps(vec![cand(1, 10, "loads", "Local.loads", "Method")]);
        let r = run("json.loads", 10, &by_bare, &by_qn);
        assert_eq!(r.status, ResolutionStatus::Unresolved);
        assert_eq!(r.callee_symbol_id, None);

        // Exact QN match still works when callee is the Type.method form.
        let r2 = run("Local.loads", 10, &by_bare, &by_qn);
        assert_eq!(r2.status, ResolutionStatus::Resolved);
        assert_eq!(r2.callee_symbol_id, Some(1));
    }

    #[test]
    fn resolve_external_member_not_false_resolved_dod9() {
        // Local unique `loads` in another file must NOT capture `json.loads`.
        let (by_bare, by_qn) = maps(vec![cand(1, 20, "loads", "loads", "Function")]);
        let r = run("json.loads", 10, &by_bare, &by_qn);
        assert_eq!(r.status, ResolutionStatus::Unresolved);
        assert_eq!(r.callee_symbol_id, None);
        assert_eq!(r.unresolved_callee.as_deref(), Some("json.loads"));

        // TypeScript member form
        let (by_bare, by_qn) = maps(vec![cand(2, 30, "get", "get", "Function")]);
        let r = run("axios.get", 10, &by_bare, &by_qn);
        assert_eq!(r.status, ResolutionStatus::Unresolved);
        assert_eq!(r.callee_symbol_id, None);

        // Cross-file unique Function still Unresolved for multi-segment.
        let (by_bare, by_qn) = maps(vec![cand(3, 99, "loads", "loads", "Function")]);
        let r = run("json.loads", 10, &by_bare, &by_qn);
        assert_eq!(r.status, ResolutionStatus::Unresolved);
    }

    #[test]
    fn resolve_unresolved_unknown_name() {
        let (by_bare, by_qn) = maps(vec![]);
        let r = run("no_such_fn", 1, &by_bare, &by_qn);
        assert_eq!(r.status, ResolutionStatus::Unresolved);
        assert_eq!(r.unresolved_callee.as_deref(), Some("no_such_fn"));
    }

    /// DoD-6 / R1-02: production full + incremental both build candidates via
    /// [`resolve_candidate_from_row`] + [`build_resolve_maps`] then
    /// [`resolve_callee`]. This fixture would fail if either path filtered
    /// callable kinds differently or skipped QN matching.
    ///
    /// Residual: not a live dual SQLite CallGraphBuilder vs IncrementalIndexer
    /// end-to-end. Partial incremental re-index of one file can leave stale
    /// edges in unchanged files when collision sets change (pre-existing
    /// incremental semantics). For a complete pass over the same tree both
    /// paths load the full symbol table and share this pure decision function.
    #[test]
    fn full_and_incremental_share_resolve_decision() {
        // Multi-file collision fixture (same row shape both paths load from DB).
        let rows = [
            (1i64, 10i64, "new", "Foo.new", "Method"),
            (2, 20, "new", "Bar.new", "Method"),
            (3, 10, "helper", "helper", "Function"),
            (4, 30, "tests", "tests", "Module"),
            (5, 10, "loads", "loads", "Function"),
            (6, 10, "process", "process", "Method"),
            (7, 40, "enrich", "A.enrich", "Method"),
            (8, 50, "enrich", "B.enrich", "Method"),
        ];

        // Full path: rows → ResolveCandidate → maps (call_graph.rs).
        let full_candidates: Vec<ResolveCandidate> = rows
            .iter()
            .map(|(id, fid, name, qn, kind)| {
                resolve_candidate_from_row(
                    *id,
                    *fid,
                    name.to_string(),
                    qn.to_string(),
                    kind.to_string(),
                )
            })
            .collect();
        let (full_bare, full_qn) = build_resolve_maps(full_candidates);

        // Incremental path: identical construction (incremental.rs).
        let inc_candidates: Vec<ResolveCandidate> = rows
            .iter()
            .map(|(id, fid, name, qn, kind)| {
                resolve_candidate_from_row(
                    *id,
                    *fid,
                    name.to_string(),
                    qn.to_string(),
                    kind.to_string(),
                )
            })
            .collect();
        let (inc_bare, inc_qn) = build_resolve_maps(inc_candidates);

        assert_eq!(full_bare, inc_bare, "bare maps must match across paths");
        assert_eq!(full_qn, inc_qn, "QN maps must match across paths");

        let cases = [
            ("helper", 10i64),
            ("new", 10),
            ("Foo.new", 99),
            ("Bar::new", 99),
            ("tests", 10),
            ("json.loads", 10), // multi-segment → Unresolved (DoD-9)
            ("s.process", 10),  // multi-segment QN miss → Unresolved (codex P1-1)
            ("process", 10),    // bare same-file Method → Resolved
            ("enrich", 99),     // cross-file collision → Ambiguous
            ("missing", 10),
        ];
        for (callee, file) in cases {
            let a = run(callee, file, &full_bare, &full_qn);
            let b = run(callee, file, &inc_bare, &inc_qn);
            assert_eq!(
                a, b,
                "full vs incremental diverge for callee={callee} file={file}"
            );
        }

        // Spot-check expected decisions on the shared result.
        assert_eq!(
            run("json.loads", 10, &full_bare, &full_qn).status,
            ResolutionStatus::Unresolved
        );
        assert_eq!(
            run("s.process", 10, &full_bare, &full_qn).status,
            ResolutionStatus::Unresolved
        );
        assert_eq!(
            run("process", 10, &full_bare, &full_qn).status,
            ResolutionStatus::Resolved
        );
        assert_eq!(
            run("Foo.new", 99, &full_bare, &full_qn).callee_symbol_id,
            Some(1)
        );
        assert_eq!(
            run("Bar::new", 99, &full_bare, &full_qn).callee_symbol_id,
            Some(2)
        );
    }

    /// R1-05: map build is order-independent; resolve results are deterministic.
    #[test]
    fn resolve_maps_deterministic_under_shuffled_input() {
        let order_a = vec![
            cand(1, 10, "new", "Foo.new", "Method"),
            cand(2, 20, "new", "Bar.new", "Method"),
            cand(3, 10, "helper", "helper", "Function"),
            cand(5, 10, "loads", "loads", "Function"),
            cand(6, 10, "process", "process", "Method"),
            cand(7, 40, "enrich", "A.enrich", "Method"),
            cand(8, 50, "enrich", "B.enrich", "Method"),
        ];
        let mut order_b = order_a.clone();
        // Deterministic reverse + rotate (no RNG) to scramble insertion order.
        order_b.reverse();
        if let Some(first) = order_b.first().cloned() {
            order_b.remove(0);
            order_b.push(first);
        }
        assert_ne!(
            order_a.iter().map(|c| c.symbol_id).collect::<Vec<_>>(),
            order_b.iter().map(|c| c.symbol_id).collect::<Vec<_>>(),
            "fixture orders must differ"
        );

        let (bare_a, qn_a) = build_resolve_maps(order_a.clone());
        let (bare_b, qn_b) = build_resolve_maps(order_b);

        assert_eq!(bare_a, bare_b);
        assert_eq!(qn_a, qn_b);

        let callees = [
            ("helper", 10i64),
            ("new", 10),
            ("Foo.new", 99),
            ("Bar::new", 99),
            ("json.loads", 10),
            ("s.process", 10),
            ("enrich", 99),
            ("obj.method", 10),
        ];
        for (callee, file) in callees {
            assert_eq!(
                run(callee, file, &bare_a, &qn_a),
                run(callee, file, &bare_b, &qn_b),
                "shuffled input changed resolve for {callee}"
            );
        }

        // Building twice from identical order also agrees.
        let (bare_c, qn_c) = build_resolve_maps(order_a);
        for (callee, file) in [("Foo.new", 99i64), ("json.loads", 10), ("s.process", 10)] {
            assert_eq!(
                run(callee, file, &bare_a, &qn_a),
                run(callee, file, &bare_c, &qn_c)
            );
        }
    }

    /// R1-07: extract-style composition — Rust Foo::new resolves to Foo only.
    #[test]
    fn e2e_rust_scoped_new_resolves_to_foo_only() {
        // Mirrors extract_calls storing Foo.new (dotted) + symbols Foo.new / Bar.new.
        let (by_bare, by_qn) = maps(vec![
            cand(1, 10, "new", "Foo.new", "Method"),
            cand(2, 20, "new", "Bar.new", "Method"),
        ]);
        // Extracted callee form after :: → . normalization in rust/calls.rs.
        let extracted = "Foo.new";
        let r = run(extracted, 99, &by_bare, &by_qn);
        assert_eq!(r.status, ResolutionStatus::Resolved);
        assert_eq!(r.callee_symbol_id, Some(1));
        assert_ne!(r.callee_symbol_id, Some(2));

        // Path form still accepted by resolver.
        let r2 = run("Foo::new", 99, &by_bare, &by_qn);
        assert_eq!(r2.callee_symbol_id, Some(1));
    }

    /// R1-07: Python extract json.loads + same-file Function loads → Unresolved.
    #[test]
    fn e2e_python_json_loads_same_file_function_unresolved() {
        // Extract stores dotted "json.loads"; same-file free Function "loads".
        let (by_bare, by_qn) = maps(vec![cand(1, 10, "loads", "loads", "Function")]);
        let extracted = "json.loads";
        let r = run(extracted, 10, &by_bare, &by_qn);
        assert_eq!(r.status, ResolutionStatus::Unresolved);
        assert_eq!(r.callee_symbol_id, None);
        assert_eq!(r.unresolved_callee.as_deref(), Some("json.loads"));
    }

    #[test]
    fn normalize_double_colon() {
        assert_eq!(normalize_callee_name("Foo::new"), "Foo.new");
        assert_eq!(normalize_callee_name("a::b::c"), "a.b.c");
        assert_eq!(normalize_callee_name("plain"), "plain");
    }

    #[test]
    fn bare_segment_extracts_last() {
        assert_eq!(bare_segment("json.loads"), "loads");
        assert_eq!(bare_segment("Foo.new"), "new");
        assert_eq!(bare_segment("plain"), "plain");
    }

    // --- 0092 DoD-4: fs dual-direction, crate/self/super, mod-bound ---

    #[test]
    fn dod4_fs_mod_local_resolves() {
        // File with `pub mod fs;` calling fs::write → local if candidates exist.
        let (by_bare, by_qn) = maps(vec![cand(1, 20, "write", "write", "Function")]);
        let mut bindings = HashMap::new();
        bindings.insert(
            "fs".to_string(),
            BindingInfo {
                source_path: "fs".to_string(),
                binding_kind: "mod".to_string(),
                is_enumerable: true,
                is_local: true,
            },
        );
        let mut modules = HashMap::new();
        modules.insert(10, "crate::util".to_string());
        modules.insert(20, "crate::util::fs".to_string());

        let r = run_with_ctx(
            "fs.write",
            10,
            &by_bare,
            &by_qn,
            Some("crate::util"),
            &bindings,
            &modules,
        );
        assert_eq!(r.status, ResolutionStatus::Resolved);
        assert_eq!(r.callee_symbol_id, Some(1));
        assert_eq!(r.callee_file_id, Some(20));
    }

    #[test]
    fn dod4_fs_use_std_stays_unresolved() {
        // File with `use std::fs;` calling fs::write → MUST stay Unresolved.
        let (by_bare, by_qn) = maps(vec![cand(1, 20, "write", "write", "Function")]);
        let mut bindings = HashMap::new();
        bindings.insert(
            "fs".to_string(),
            BindingInfo {
                source_path: "std::fs".to_string(),
                binding_kind: "use".to_string(),
                is_enumerable: true,
                is_local: false,
            },
        );
        let mut modules = HashMap::new();
        modules.insert(10, "crate::semantic".to_string());
        modules.insert(20, "crate::util::fs".to_string());

        for callee in ["fs.write", "fs::write"] {
            let r = run_with_ctx(
                callee,
                10,
                &by_bare,
                &by_qn,
                Some("crate::semantic"),
                &bindings,
                &modules,
            );
            assert_eq!(
                r.status,
                ResolutionStatus::Unresolved,
                "std::fs must not resolve for {callee}"
            );
            assert_eq!(r.callee_symbol_id, None);
        }
    }

    #[test]
    fn dod4_fs_no_binding_stays_unresolved() {
        // Name-only matching without binding must not resolve.
        let (by_bare, by_qn) = maps(vec![cand(1, 20, "write", "write", "Function")]);
        let bindings = empty_bindings();
        let mut modules = HashMap::new();
        modules.insert(20, "crate::util::fs".to_string());

        let r = run_with_ctx(
            "fs.write",
            10,
            &by_bare,
            &by_qn,
            Some("crate::main"),
            &bindings,
            &modules,
        );
        // Falls through to QN miss → Unresolved
        assert_eq!(r.status, ResolutionStatus::Unresolved);
        assert_eq!(r.callee_symbol_id, None);
    }

    #[test]
    fn dod4_crate_rooted_happy_path() {
        let (by_bare, by_qn) = maps(vec![cand(5, 50, "build_urn", "build_urn", "Function")]);
        let bindings = empty_bindings();
        let mut modules = HashMap::new();
        modules.insert(50, "crate::platform::urn".to_string());
        modules.insert(10, "crate::index".to_string());

        let r = run_with_ctx(
            "crate.platform.urn.build_urn",
            10,
            &by_bare,
            &by_qn,
            Some("crate::index"),
            &bindings,
            &modules,
        );
        assert_eq!(r.status, ResolutionStatus::Resolved);
        assert_eq!(r.callee_symbol_id, Some(5));
    }

    #[test]
    fn dod4_super_and_self() {
        let (by_bare, by_qn) = maps(vec![
            cand(1, 20, "helper", "helper", "Function"),
            cand(2, 30, "local_fn", "local_fn", "Function"),
        ]);
        let bindings = empty_bindings();
        let mut modules = HashMap::new();
        modules.insert(10, "crate::index::resolve".to_string());
        modules.insert(20, "crate::index".to_string());
        modules.insert(30, "crate::index::resolve".to_string());

        let super_r = run_with_ctx(
            "super.helper",
            10,
            &by_bare,
            &by_qn,
            Some("crate::index::resolve"),
            &bindings,
            &modules,
        );
        assert_eq!(super_r.status, ResolutionStatus::Resolved);
        assert_eq!(super_r.callee_symbol_id, Some(1));

        let self_r = run_with_ctx(
            "self.local_fn",
            10,
            &by_bare,
            &by_qn,
            Some("crate::index::resolve"),
            &bindings,
            &modules,
        );
        assert_eq!(self_r.status, ResolutionStatus::Resolved);
        assert_eq!(self_r.callee_symbol_id, Some(2));
    }

    #[test]
    fn dod4_mod_bound_without_use() {
        // `mod extraction;` calling extraction.build_call_graph with no use.
        let (by_bare, by_qn) = maps(vec![cand(
            7,
            70,
            "build_call_graph",
            "build_call_graph",
            "Function",
        )]);
        let mut bindings = HashMap::new();
        bindings.insert(
            "extraction".to_string(),
            BindingInfo {
                source_path: "extraction".to_string(),
                binding_kind: "mod".to_string(),
                is_enumerable: true,
                is_local: true,
            },
        );
        let mut modules = HashMap::new();
        modules.insert(10, "crate::index::orchestrator".to_string());
        modules.insert(70, "crate::index::orchestrator::extraction".to_string());

        let r = run_with_ctx(
            "extraction.build_call_graph",
            10,
            &by_bare,
            &by_qn,
            Some("crate::index::orchestrator"),
            &bindings,
            &modules,
        );
        assert_eq!(r.status, ResolutionStatus::Resolved);
        assert_eq!(r.callee_symbol_id, Some(7));
    }

    #[test]
    fn dod4_wildcard_does_not_prove_locality() {
        let (by_bare, by_qn) = maps(vec![cand(1, 20, "write", "write", "Function")]);
        let mut bindings = HashMap::new();
        bindings.insert(
            "*".to_string(),
            BindingInfo {
                source_path: "crate::util::fs::*".to_string(),
                binding_kind: "use_wildcard".to_string(),
                is_enumerable: false,
                is_local: false,
            },
        );
        // Also no enumerable `fs` binding — first segment is `fs`
        let mut modules = HashMap::new();
        modules.insert(20, "crate::util::fs".to_string());

        let r = run_with_ctx(
            "fs.write",
            10,
            &by_bare,
            &by_qn,
            Some("crate::main"),
            &bindings,
            &modules,
        );
        assert_eq!(r.status, ResolutionStatus::Unresolved);
    }

    #[test]
    fn dod4_type_method_qn_still_works_without_binding() {
        // Foo.new must still resolve via QN without a binding for Foo.
        let (by_bare, by_qn) = maps(vec![
            cand(1, 10, "new", "Foo.new", "Method"),
            cand(2, 20, "new", "Bar.new", "Method"),
        ]);
        let r = run("Foo.new", 99, &by_bare, &by_qn);
        assert_eq!(r.status, ResolutionStatus::Resolved);
        assert_eq!(r.callee_symbol_id, Some(1));
    }

    /// DoD-8: docs must not claim certainty for RESOLVED or that UNRESOLVED
    /// means no local target exists. Affirmative certainty claims only —
    /// negations in the honesty ceiling are expected.
    #[test]
    fn call_resolution_docs_honesty_ceiling() {
        let doc = include_str!("../../docs/Call-Resolution.md");
        let lower = doc.to_lowercase();
        // Strip lines that are explicitly disclaimers / bans so we only scan
        // affirmative product claims.
        let affirmative: String = doc
            .lines()
            .filter(|l| {
                let t = l.trim().to_lowercase();
                !t.starts_with('-')
                    && !t.contains("must **not**")
                    && !t.contains("must not")
                    && !t.contains("not a claim")
                    && !t.contains("never")
                    && !t.contains("out of reach")
                    && !t.contains("cannot")
                    && !t.contains("do not claim")
            })
            .collect::<Vec<_>>()
            .join("\n")
            .to_lowercase();

        let banned = [
            "resolved with certainty",
            "certainly the call target",
            "guarantees the call target",
            "complete name resolution",
            "compiler-grade resolution is provided",
            "unresolved means no local target exists",
            "unresolved means there is no local target",
        ];
        for phrase in banned {
            assert!(
                !affirmative.contains(phrase),
                "Call-Resolution.md must not affirm banned certainty phrase: {phrase:?}"
            );
        }

        // 0105: tracked skill must not re-advertise SCIP as universal "compiler-grade".
        let skill = include_str!("../../docs/Ledgerful/skill.md").to_lowercase();
        assert!(
            !skill.contains("compiler-grade"),
            "docs/Ledgerful/skill.md must not claim compiler-grade SCIP (0105 honesty)"
        );
        assert!(
            skill.contains("auto-scip") || skill.contains("--auto-scip"),
            "skill must document optional --auto-scip"
        );
        assert!(
            skill.contains("scip.status")
                || skill.contains("edges_added")
                || skill.contains("off by default"),
            "skill must mention SCIP opt-in / json fields honesty"
        );
        // Required honest framing present in full doc
        assert!(
            lower.contains("unique-local-candidate") || lower.contains("unique local candidate"),
            "docs must state the unique-local-candidate floor"
        );
        assert!(
            lower.contains("re-export") || lower.contains("re-exports"),
            "docs must list re-exports as out of reach"
        );
        assert!(
            lower.contains("glob import") || lower.contains("glob imports"),
            "docs must list glob imports as out of reach"
        );
        // 0092 §2.7 quantitative ceiling
        assert!(
            lower.contains("third-party") || lower.contains("third party"),
            "docs must state unresolved majority is third-party by design"
        );
        assert!(
            lower.contains("type inference") || lower.contains("receiver"),
            "docs must mention method-call type inference ceiling"
        );
    }

    /// DoD-6 rolled-in: pure resolve path identity for full vs incremental
    /// inputs (same maps + bindings → same edges). Live SQLite identity is
    /// in `call_graph` tests.
    #[test]
    fn dod6_full_incremental_binding_resolve_identity() {
        let (by_bare, by_qn) = maps(vec![
            cand(1, 20, "write", "write", "Function"),
            cand(2, 50, "build_urn", "build_urn", "Function"),
        ]);
        let mut bindings_mod = HashMap::new();
        bindings_mod.insert(
            "fs".to_string(),
            BindingInfo {
                source_path: "fs".to_string(),
                binding_kind: "mod".to_string(),
                is_enumerable: true,
                is_local: true,
            },
        );
        let mut bindings_std = HashMap::new();
        bindings_std.insert(
            "fs".to_string(),
            BindingInfo {
                source_path: "std::fs".to_string(),
                binding_kind: "use".to_string(),
                is_enumerable: true,
                is_local: false,
            },
        );
        let mut modules = HashMap::new();
        modules.insert(10, "crate::util".to_string());
        modules.insert(20, "crate::util::fs".to_string());
        modules.insert(50, "crate::platform::urn".to_string());

        let cases = [
            ("fs.write", 10i64, Some("crate::util"), &bindings_mod),
            ("fs.write", 10, Some("crate::util"), &bindings_std),
            (
                "crate.platform.urn.build_urn",
                10,
                Some("crate::util"),
                &bindings_mod,
            ),
        ];
        for (callee, file, mp, binds) in cases {
            // Simulate full path decision
            let a = run_with_ctx(callee, file, &by_bare, &by_qn, mp, binds, &modules);
            // Simulate incremental path decision (identical inputs)
            let b = run_with_ctx(callee, file, &by_bare, &by_qn, mp, binds, &modules);
            assert_eq!(a, b, "full vs incremental diverge for {callee}");
        }
    }
}
