//! Embedded-material lookup for `verify --scope fast` (0549).
//!
//! A changed non-code file (document, template, fixture, data file) can be
//! compiled into or read by Rust tests. This module walks the repo's `.rs`
//! files under hard bounds, finds `include_str!` / `include_bytes!` /
//! `include!` sites and literal runtime path reads, and maps each changed
//! path to nextest `test()` **function names** (never file or binary stems:
//! those match zero tests or nearly all of them).
//!
//! The scan is lexical and independent of index freshness. Comments are
//! skipped, so every hit lands on a real item; a match inside a string is an
//! over-approximation that only adds real tests. Anything the scan cannot
//! prove (bound exceeded, unreadable file, non-literal include form) is
//! reported as unresolved so the caller refuses or falls back to a full run.

use super::rust_lex::{
    consume_skipped, find_matching_delimiter, is_ident_byte, skip_trivia, take_string_literal,
};
use crate::impact::packet::ImpactPacket;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// Maximum `.rs` files read per scan.
pub(crate) const MAX_FILES_SCANNED: usize = 2500;
/// Maximum size of a single scanned `.rs` file.
pub(crate) const MAX_FILE_BYTES: u64 = 512 * 1024;
/// Maximum total bytes read per scan.
pub(crate) const MAX_TOTAL_BYTES: u64 = 64 * 1024 * 1024;

/// Directory names never walked.
const SKIP_DIRS: &[&str] = &["target", ".git", ".ledgerful", "node_modules"];

/// Extensions the index maps as source; every other changed path is a
/// non-code candidate for the embedded lookup.
const SOURCE_EXTS: &[&str] = &[
    "rs", "ts", "tsx", "js", "jsx", "mjs", "cjs", "py", "go", "java", "kt", "kts", "c", "h", "cpp",
    "cc", "cxx", "hpp", "hh", "hxx", "h++",
];

/// How a reference maps to tests.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Resolution {
    /// Sorted, deduped nextest `test()` function names.
    Tests(Vec<String>),
    /// Production includer with no inline tests: use `test_mapping`.
    NeedsMapping,
    /// Includer found but no `#[test]` function can be tied to it.
    NoTests,
}

/// One includer of a material path.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct EmbedRef {
    /// Repo-relative, `/`-separated includer path.
    pub includer: String,
    pub resolution: Resolution,
}

#[derive(Debug, Default)]
struct RuntimeFile {
    crate_root: String,
    literals: BTreeSet<String>,
    resolution: Option<Resolution>,
}

/// Result of one bounded scan of the repo's Rust sources.
#[derive(Debug, Default)]
pub(crate) struct EmbeddedScan {
    refs: BTreeMap<String, BTreeSet<EmbedRef>>,
    runtime: BTreeMap<String, RuntimeFile>,
    /// Set when a cap was hit; the reason names the cap.
    pub bounded: Option<String>,
    /// Unreadable / unlexable files (sorted).
    pub errors: BTreeSet<String>,
    /// Non-literal compile-time include forms (e.g. `OUT_DIR`) and
    /// non-literal runtime reads in test scope (sorted, `file: form`).
    pub unresolved_reads: BTreeSet<String>,
}

fn fold(path: &str) -> String {
    if cfg!(windows) {
        path.to_ascii_lowercase()
    } else {
        path.to_string()
    }
}

/// Normalize a repo-relative path: `\` → `/`, strip `./`, collapse `.`/`..`.
/// `None` when the path escapes the repo root.
pub(crate) fn normalize_repo_path(raw: &str) -> Option<String> {
    let s = raw.replace('\\', "/");
    let mut parts: Vec<&str> = Vec::new();
    for p in s.split('/') {
        match p {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            other => parts.push(other),
        }
    }
    Some(parts.join("/"))
}

fn join_norm(base_dir: &str, rel: &str) -> Option<String> {
    if rel.starts_with('/') || rel.starts_with('\\') || rel.as_bytes().get(1) == Some(&b':') {
        return None;
    }
    if base_dir.is_empty() {
        normalize_repo_path(rel)
    } else {
        normalize_repo_path(&format!("{base_dir}/{rel}"))
    }
}

fn parent_dir(rel: &str) -> &str {
    rel.rsplit_once('/').map(|(d, _)| d).unwrap_or("")
}

/// True for paths the index maps as source (not an embedded-material candidate).
pub(crate) fn is_source_path(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| SOURCE_EXTS.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

fn is_test_file_path(rel: &str) -> bool {
    let name = rel.rsplit('/').next().unwrap_or(rel);
    name == "tests.rs" || rel.split('/').any(|c| c == "tests")
}

// ---------------------------------------------------------------- test layout

#[derive(Debug, Default)]
struct TestLayout {
    /// `(name, body_start, body_end)` for every `#[test]`-like fn.
    fns: Vec<(String, usize, usize)>,
    /// `(start, end, is_mod)` for every `#[cfg(test)]` item.
    cfg_items: Vec<(usize, usize, bool)>,
}

fn attr_is_test(compact: &str) -> bool {
    let head = compact.split('(').next().unwrap_or(compact);
    let last = head.rsplit("::").next().unwrap_or(head);
    last == "test" || last == "rstest" || last == "test_case" || last == "case"
}

/// End (exclusive) of the `[` ... `]` attribute body starting at `open`.
fn end_of_bracket(src: &str, open: usize, rel: &str) -> Result<Option<usize>, String> {
    let b = src.as_bytes();
    let mut depth = 0i32;
    let mut i = open;
    while i < b.len() {
        if i != open
            && let Some(next) = consume_skipped(src, i, rel)?
        {
            i = next.max(i + 1);
            continue;
        }
        match b[i] {
            b'[' => depth += 1,
            b']' => {
                depth -= 1;
                if depth == 0 {
                    return Ok(Some(i));
                }
            }
            _ => {}
        }
        i += 1;
    }
    Ok(None)
}

/// Item end (exclusive byte offset) for the item starting at `from`: the
/// matching `}` of the first top-level `{`, or the first top-level `;`.
fn item_end(src: &str, from: usize, rel: &str) -> Result<usize, String> {
    let b = src.as_bytes();
    let mut i = from;
    let mut depth = 0i32;
    while i < b.len() {
        if let Some(next) = consume_skipped(src, i, rel)? {
            i = next.max(i + 1);
            continue;
        }
        match b[i] {
            b'(' | b'[' => depth += 1,
            b')' | b']' => depth -= 1,
            b'{' if depth <= 0 => {
                return Ok(find_matching_delimiter(src, i, rel)?
                    .map(|e| e + 1)
                    .unwrap_or(b.len()));
            }
            b';' if depth <= 0 => return Ok(i + 1),
            _ => {}
        }
        i += 1;
    }
    Ok(b.len())
}

fn analyze_tests(src: &str, rel: &str) -> Result<TestLayout, String> {
    let b = src.as_bytes();
    let mut layout = TestLayout::default();
    let mut i = 0;
    while i < b.len() {
        if let Some(next) = consume_skipped(src, i, rel)? {
            i = next.max(i + 1);
            continue;
        }
        if b[i] != b'#' {
            i += src[i..].chars().next().map(char::len_utf8).unwrap_or(1);
            continue;
        }
        // Attribute group: `#[..]` (optionally `#![..]`, ignored).
        let mut j = skip_trivia(src, i + 1, rel)?;
        if j < b.len() && b[j] == b'!' {
            i = j + 1;
            continue;
        }
        if j >= b.len() || b[j] != b'[' {
            i += 1;
            continue;
        }
        let mut is_test = false;
        let mut is_cfg_test = false;
        let mut group_end = i;
        while let Some(close) = end_of_bracket(src, j, rel)? {
            let compact: String = src[j + 1..close]
                .chars()
                .filter(|c| !c.is_whitespace())
                .collect();
            if attr_is_test(&compact) {
                is_test = true;
            }
            if compact == "cfg(test)" {
                is_cfg_test = true;
            }
            group_end = close + 1;
            let next = skip_trivia(src, group_end, rel)?;
            if next < b.len() && b[next] == b'#' {
                let k = skip_trivia(src, next + 1, rel)?;
                if k < b.len() && b[k] == b'[' {
                    j = k;
                    continue;
                }
            }
            break;
        }
        let item_start = skip_trivia(src, group_end, rel)?;
        if is_test || is_cfg_test {
            let end = item_end(src, item_start, rel)?;
            let header_end = src[item_start..end]
                .find(['{', ';'])
                .map(|p| item_start + p)
                .unwrap_or(end);
            let header = &src[item_start..header_end];
            let words: Vec<&str> = header
                .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .filter(|w| !w.is_empty())
                .collect();
            let is_mod =
                words.first().copied() == Some("mod") || words.iter().take(3).any(|w| *w == "mod");
            if is_test
                && let Some(pos) = words.iter().position(|w| *w == "fn")
                && let Some(name) = words.get(pos + 1)
            {
                layout.fns.push(((*name).to_string(), item_start, end));
            }
            if is_cfg_test {
                layout.cfg_items.push((item_start, end, is_mod));
            }
        }
        i = group_end.max(i + 1);
    }
    Ok(layout)
}

fn names_in(layout: &TestLayout, lo: usize, hi: usize) -> Vec<String> {
    let set: BTreeSet<String> = layout
        .fns
        .iter()
        .filter(|(_, s, e)| *s >= lo && *e <= hi)
        .map(|(n, _, _)| n.clone())
        .collect();
    set.into_iter().collect()
}

fn all_names(layout: &TestLayout) -> Vec<String> {
    names_in(layout, 0, usize::MAX)
}

fn tests_or_none(names: Vec<String>) -> Resolution {
    if names.is_empty() {
        Resolution::NoTests
    } else {
        Resolution::Tests(names)
    }
}

/// Resolve an include at byte `offset` to tests (0549 D3 a–f).
fn resolve_offset(layout: &TestLayout, is_test_file: bool, offset: usize) -> Resolution {
    // (a) inside a `#[test]` fn.
    let enclosing: BTreeSet<String> = layout
        .fns
        .iter()
        .filter(|(_, s, e)| *s <= offset && offset < *e)
        .map(|(n, _, _)| n.clone())
        .collect();
    if !enclosing.is_empty() {
        return Resolution::Tests(enclosing.into_iter().collect());
    }
    // (b) inside a `#[cfg(test)]` item.
    let containing: Vec<&(usize, usize, bool)> = layout
        .cfg_items
        .iter()
        .filter(|(s, e, _)| *s <= offset && offset < *e)
        .collect();
    if let Some((s, e, _)) = containing.iter().find(|(_, _, m)| *m) {
        return tests_or_none(names_in(layout, *s, *e));
    }
    if !containing.is_empty() {
        return tests_or_none(all_names(layout));
    }
    // Test-target file: every test fn in it.
    if is_test_file {
        return tests_or_none(all_names(layout));
    }
    // (c) production code with an inline `#[cfg(test)]` module.
    let mut inline: Vec<String> = Vec::new();
    for (s, e, is_mod) in &layout.cfg_items {
        if *is_mod {
            inline.extend(names_in(layout, *s, *e));
        }
    }
    inline.sort();
    inline.dedup();
    if !inline.is_empty() {
        return Resolution::Tests(inline);
    }
    // (d) production code, no inline tests.
    Resolution::NeedsMapping
}

fn resolve_whole_file(layout: &TestLayout, is_test_file: bool) -> Resolution {
    if is_test_file {
        return tests_or_none(all_names(layout));
    }
    let mut inline: Vec<String> = Vec::new();
    for (s, e, is_mod) in &layout.cfg_items {
        if *is_mod {
            inline.extend(names_in(layout, *s, *e));
        }
    }
    inline.sort();
    inline.dedup();
    if inline.is_empty() {
        Resolution::NeedsMapping
    } else {
        Resolution::Tests(inline)
    }
}

// ------------------------------------------------------------ include scanning

#[derive(Debug)]
enum IncludeArg {
    Literal(String),
    ManifestRel(String),
    Unresolved(String),
}

fn find_code_macro(src: &str, from: usize, name: &str, rel: &str) -> Result<Option<usize>, String> {
    let b = src.as_bytes();
    let pat = format!("{name}!");
    let mut i = from;
    while i < b.len() {
        if let Some(next) = consume_skipped(src, i, rel)? {
            i = next.max(i + 1);
            continue;
        }
        if src[i..].starts_with(&pat) && (i == 0 || !is_ident_byte(b[i - 1])) {
            return Ok(Some(i));
        }
        i += src[i..].chars().next().map(char::len_utf8).unwrap_or(1);
    }
    Ok(None)
}

fn parse_include_arg(inner: &str, rel: &str) -> Result<IncludeArg, String> {
    let start = skip_trivia(inner, 0, rel)?;
    if let Some((lit, end)) = take_string_literal(inner, start, rel)? {
        let after = skip_trivia(inner, end, rel)?;
        if after == inner.len() {
            return Ok(IncludeArg::Literal(lit));
        }
    }
    if inner[start..].starts_with("concat!") {
        let compact: String = inner[start..]
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();
        let body = compact
            .strip_prefix("concat!(")
            .and_then(|r| r.strip_suffix(')'));
        if let Some(body) = body
            && let Some(rest) = body.strip_prefix("env!(\"CARGO_MANIFEST_DIR\"),\"")
            && let Some(relp) = rest.strip_suffix('"')
            && !relp.contains('"')
        {
            return Ok(IncludeArg::ManifestRel(
                relp.trim_start_matches(['/', '\\']).replace('\\', "/"),
            ));
        }
    }
    Ok(IncludeArg::Unresolved(inner.trim().to_string()))
}

/// Nearest ancestor directory (repo-relative, `""` = root) holding a `Cargo.toml`.
fn nearest_crate_root(repo_root: &Path, rel: &str, cache: &mut BTreeMap<String, String>) -> String {
    let mut dir = parent_dir(rel).to_string();
    let start = dir.clone();
    if let Some(hit) = cache.get(&start) {
        return hit.clone();
    }
    let found = loop {
        let manifest = if dir.is_empty() {
            repo_root.join("Cargo.toml")
        } else {
            repo_root.join(&dir).join("Cargo.toml")
        };
        if manifest.is_file() {
            break dir.clone();
        }
        if dir.is_empty() {
            break String::new();
        }
        dir = parent_dir(&dir).to_string();
    };
    cache.insert(start, found.clone());
    found
}

const RUNTIME_PREFILTER: &[&str] = &["read_to_string", "fs::read", "File::open"];
/// Substrings of file-consuming macros the scan does not resolve
/// (`sqlx::query_file!`, askama `#[template(path = ..)]`, `include_dir!`, rust-embed).
const UNMODELED_FILE_CONSUMERS: &[&str] = &[
    "_file!(",
    "_file_as!(",
    "template(path",
    "include_dir!",
    "RustEmbed",
];
const READ_CALLS: &[&str] = &["read_to_string", "fs::read", "File::open"];

/// String literal contents (comments skipped) inside `sub`.
fn literals_in(sub: &str, rel: &str, out: &mut BTreeSet<String>) -> Result<(), String> {
    let b = sub.as_bytes();
    let mut i = 0;
    while i < b.len() {
        let ident_start = i == 0 || !is_ident_byte(b[i - 1]);
        if (b[i] == b'"' || (b[i] == b'r' && ident_start))
            && let Some((lit, end)) = take_string_literal(sub, i, rel)?
        {
            out.insert(lit);
            i = end.max(i + 1);
            continue;
        }
        if let Some(next) = consume_skipped(sub, i, rel)? {
            i = next.max(i + 1);
            continue;
        }
        i += sub[i..].chars().next().map(char::len_utf8).unwrap_or(1);
    }
    Ok(())
}

/// True when `arg` is built only from string literals and plain path
/// builders (`Path::new("a").join("b")`, `concat!(env!("CARGO_MANIFEST_DIR"), ..)`).
/// Any other identifier (a variable, `format!`, a helper call) makes the read
/// non-literal, so the scan cannot attribute it to a specific path.
fn is_pure_path_literal_expr(arg: &str, rel: &str) -> Result<bool, String> {
    const ALLOWED: &[&str] = &[
        "Path",
        "PathBuf",
        "new",
        "from",
        "join",
        "std",
        "path",
        "env",
        "concat",
        "CARGO_MANIFEST_DIR",
    ];
    let b = arg.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if let Some((_, end)) = take_string_literal(arg, i, rel)?
            && (b[i] == b'"' || (b[i] == b'r' && (i == 0 || !is_ident_byte(b[i - 1]))))
        {
            i = end.max(i + 1);
            continue;
        }
        if let Some(next) = consume_skipped(arg, i, rel)? {
            i = next.max(i + 1);
            continue;
        }
        if b[i].is_ascii_alphabetic() || b[i] == b'_' {
            let start = i;
            while i < b.len() && is_ident_byte(b[i]) {
                i += 1;
            }
            if !ALLOWED.contains(&&arg[start..i]) {
                return Ok(false);
            }
            continue;
        }
        i += arg[i..].chars().next().map(char::len_utf8).unwrap_or(1);
    }
    Ok(true)
}

/// Literals that are arguments of a runtime file read (`read_to_string`,
/// `fs::read`, `File::open`), including nested `join("..")` / `Path::new("..")`
/// pieces, plus the count of reads with no literal at all in their argument.
fn collect_literals(src: &str, rel: &str) -> Result<(BTreeSet<String>, usize), String> {
    let b = src.as_bytes();
    let mut lits = BTreeSet::new();
    let mut dynamic = 0usize;
    let mut i = 0;
    while i < b.len() {
        if let Some(next) = consume_skipped(src, i, rel)? {
            i = next.max(i + 1);
            continue;
        }
        let ident_start = i == 0 || !is_ident_byte(b[i - 1]);
        if ident_start {
            for call in READ_CALLS {
                if src[i..].starts_with(call) {
                    let after = skip_trivia(src, i + call.len(), rel)?;
                    if after < b.len()
                        && b[after] == b'('
                        && let Some(end) = find_matching_delimiter(src, after, rel)?
                    {
                        let arg = &src[after + 1..end];
                        let mut found = BTreeSet::new();
                        literals_in(arg, rel, &mut found)?;
                        if found.is_empty() || !is_pure_path_literal_expr(arg, rel)? {
                            dynamic += 1;
                        }
                        lits.extend(found);
                    }
                    break;
                }
            }
        }
        i += src[i..].chars().next().map(char::len_utf8).unwrap_or(1);
    }
    Ok((lits, dynamic))
}

impl EmbeddedScan {
    /// Walk `repo_root`, bounded and sorted, and build the reference maps.
    pub(crate) fn scan(repo_root: &Path) -> EmbeddedScan {
        let mut scan = EmbeddedScan::default();
        let mut files: Vec<String> = Vec::new();
        let walker = ignore::WalkBuilder::new(repo_root)
            .require_git(false)
            .sort_by_file_path(|a, b| a.cmp(b))
            .filter_entry(|e| {
                e.file_name()
                    .to_str()
                    .map(|n| !(e.depth() > 0 && SKIP_DIRS.contains(&n)))
                    .unwrap_or(true)
            })
            .build();
        for entry in walker {
            let Ok(entry) = entry else {
                continue;
            };
            if !entry.file_type().is_some_and(|t| t.is_file()) {
                continue;
            }
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            let Ok(relp) = path.strip_prefix(repo_root) else {
                continue;
            };
            let rel = relp.to_string_lossy().replace('\\', "/");
            files.push(rel);
        }
        files.sort();
        if files.len() > MAX_FILES_SCANNED {
            scan.bounded = Some(format!(
                "embedded scan bound exceeded: {} .rs files (max {MAX_FILES_SCANNED})",
                files.len()
            ));
            return scan;
        }
        let mut total: u64 = 0;
        let mut crate_cache: BTreeMap<String, String> = BTreeMap::new();
        for rel in &files {
            let full = repo_root.join(rel);
            let len = match std::fs::metadata(&full) {
                Ok(m) => m.len(),
                Err(e) => {
                    scan.errors.insert(format!("{rel}: {e}"));
                    continue;
                }
            };
            if len > MAX_FILE_BYTES {
                scan.bounded = Some(format!(
                    "embedded scan bound exceeded: {rel} is {len} bytes (max {MAX_FILE_BYTES})"
                ));
                return scan;
            }
            total += len;
            if total > MAX_TOTAL_BYTES {
                scan.bounded = Some(format!(
                    "embedded scan bound exceeded: more than {MAX_TOTAL_BYTES} bytes of .rs sources"
                ));
                return scan;
            }
            let src = match std::fs::read_to_string(&full) {
                Ok(s) => s,
                Err(e) => {
                    scan.errors.insert(format!("{rel}: {e}"));
                    continue;
                }
            };
            if let Err(e) = scan.scan_file(repo_root, rel, &src, &mut crate_cache) {
                scan.errors.insert(e);
            }
        }
        scan
    }

    fn scan_file(
        &mut self,
        repo_root: &Path,
        rel: &str,
        src: &str,
        crate_cache: &mut BTreeMap<String, String>,
    ) -> Result<(), String> {
        let has_include = src.contains("include_str!")
            || src.contains("include_bytes!")
            || src.contains("include!");
        let has_runtime = RUNTIME_PREFILTER.iter().any(|p| src.contains(p));
        if UNMODELED_FILE_CONSUMERS.iter().any(|m| src.contains(m)) {
            // Over-approximation on purpose: a consumer we do not model could
            // read any path, so non-cheap unreferenced paths must not pass.
            self.unresolved_reads
                .insert(format!("{rel}: file-consuming macro or derive not modeled"));
        }
        if !has_include && !has_runtime {
            return Ok(());
        }
        let is_test_file = is_test_file_path(rel);
        let layout = analyze_tests(src, rel)?;
        let in_test_scope = is_test_file || !layout.fns.is_empty() || !layout.cfg_items.is_empty();
        let crate_root = nearest_crate_root(repo_root, rel, crate_cache);

        if has_include {
            for mac in ["include_str", "include_bytes", "include"] {
                let mut from = 0;
                while let Some(start) = find_code_macro(src, from, mac, rel)? {
                    let after = start + mac.len() + 1;
                    let j = skip_trivia(src, after, rel)?;
                    let opener = src.as_bytes().get(j).copied();
                    let Some(end) = (match opener {
                        Some(b'(' | b'{') => find_matching_delimiter(src, j, rel)?,
                        _ => None,
                    }) else {
                        self.unresolved_reads
                            .insert(format!("{rel}: unresolved {mac}! form"));
                        from = after;
                        continue;
                    };
                    let inner = &src[j + 1..end];
                    let target = match parse_include_arg(inner, rel)? {
                        IncludeArg::Literal(l) => join_norm(parent_dir(rel), &l),
                        IncludeArg::ManifestRel(r) => join_norm(&crate_root, &r),
                        IncludeArg::Unresolved(form) => {
                            self.unresolved_reads
                                .insert(format!("{rel}: unresolved {mac}! form: {form}"));
                            None
                        }
                    };
                    if let Some(target) = target {
                        let resolution = resolve_offset(&layout, is_test_file, start);
                        self.refs
                            .entry(fold(&target))
                            .or_default()
                            .insert(EmbedRef {
                                includer: rel.to_string(),
                                resolution,
                            });
                    }
                    from = end + 1;
                }
            }
        }
        if has_runtime {
            let (literals, dynamic) = collect_literals(src, rel)?;
            if dynamic > 0 && in_test_scope {
                self.unresolved_reads.insert(format!(
                    "{rel}: {dynamic} non-literal runtime read(s) in test scope"
                ));
            }
            if !literals.is_empty() {
                self.runtime.insert(
                    rel.to_string(),
                    RuntimeFile {
                        crate_root,
                        literals,
                        resolution: Some(resolve_whole_file(&layout, is_test_file)),
                    },
                );
            }
        }
        Ok(())
    }

    /// All includers of `path` (repo-relative, `/`-separated), sorted.
    pub(crate) fn refs_for(&self, path: &str) -> Vec<EmbedRef> {
        let key = fold(path);
        let mut out: BTreeSet<EmbedRef> = self.refs.get(&key).cloned().unwrap_or_default();
        let comps: Vec<&str> = path.split('/').filter(|c| !c.is_empty()).collect();
        for (includer, file) in &self.runtime {
            if self.runtime_file_reads(file, path, &comps)
                && let Some(res) = &file.resolution
            {
                out.insert(EmbedRef {
                    includer: includer.clone(),
                    resolution: res.clone(),
                });
            }
        }
        out.into_iter().collect()
    }

    fn runtime_file_reads(&self, file: &RuntimeFile, path: &str, comps: &[&str]) -> bool {
        let want = fold(path);
        let matches_lit = |lit: &str| -> bool {
            let Some(norm) = normalize_repo_path(lit) else {
                return false;
            };
            if norm.is_empty() {
                return false;
            }
            if fold(&norm) == want {
                return true;
            }
            !file.crate_root.is_empty() && fold(&format!("{}/{}", file.crate_root, norm)) == want
        };
        if file.literals.iter().any(|l| matches_lit(l)) {
            return true;
        }
        // `Path::new("dir").join("name")` style: every path component appears
        // as its own literal in the file.
        comps.len() >= 2
            && comps.iter().all(|c| {
                file.literals
                    .iter()
                    .any(|l| fold(l.trim_matches('/')) == fold(c))
            })
    }
}

// ------------------------------------------------------------------- decision

/// What the lookup concluded for one fast-scope plan.
#[derive(Debug, Default)]
pub(crate) struct EmbeddedDecision {
    /// Sorted, deduped nextest `test()` function names.
    pub stems: Vec<String>,
    /// Production includers needing `test_mapping` (repo-relative `.rs`).
    pub needs_mapping: Vec<String>,
    /// Explicit refusal / full-fallback trigger.
    pub refuse: Option<String>,
    /// `path -> includer` evidence for the plan description (sorted).
    pub evidence: Vec<String>,
    /// Candidate paths with no dependency found (cheap or annotated).
    pub no_dependency: BTreeSet<String>,
}

/// Candidate paths for the lookup: every changed non-source path, plus the
/// old path of renames. Sorted, deduped, normalized.
pub(crate) fn candidate_paths(packet: &ImpactPacket) -> Vec<String> {
    let mut out: BTreeSet<String> = BTreeSet::new();
    for c in &packet.changes {
        for p in std::iter::once(&c.path).chain(c.old_path.iter()) {
            if !is_source_path(p)
                && let Some(n) = normalize_repo_path(&p.to_string_lossy())
            {
                out.insert(n);
            }
        }
    }
    out.into_iter().collect()
}

/// Decide stems / refusal for the non-source paths of `packet`.
///
/// `is_cheap` marks the 0203 cheap set (`docs/**`, `packaging/**`,
/// `CHEAP_EXACT`): those keep their fast path when nothing references them,
/// regardless of unresolved reads elsewhere in the repo.
pub(crate) fn decide(
    repo_root: &Path,
    candidates: &[String],
    is_cheap: impl Fn(&str) -> bool,
) -> EmbeddedDecision {
    let mut d = EmbeddedDecision::default();
    if candidates.is_empty() {
        return d;
    }
    let scan = EmbeddedScan::scan(repo_root);
    decide_with_scan(&scan, candidates, is_cheap, &mut d);
    d
}

pub(crate) fn decide_with_scan(
    scan: &EmbeddedScan,
    candidates: &[String],
    is_cheap: impl Fn(&str) -> bool,
    d: &mut EmbeddedDecision,
) {
    let mut stems: BTreeSet<String> = BTreeSet::new();
    let mut needs: BTreeSet<String> = BTreeSet::new();
    let mut evidence: BTreeSet<String> = BTreeSet::new();
    let mut refuse: Vec<String> = Vec::new();
    if let Some(b) = &scan.bounded {
        refuse.push(b.clone());
    }
    if let Some(e) = scan.errors.iter().next() {
        refuse.push(format!(
            "embedded scan could not read or lex {} file(s) (first: {e})",
            scan.errors.len()
        ));
    }
    if refuse.is_empty() {
        for path in candidates {
            let refs = scan.refs_for(path);
            if refs.is_empty() {
                if is_cheap(path) {
                    d.no_dependency.insert(path.clone());
                } else if let Some(u) = scan.unresolved_reads.iter().next() {
                    refuse.push(format!(
                        "{path} has no resolvable test dependency and the repo has unresolved reads ({u})"
                    ));
                } else {
                    d.no_dependency.insert(path.clone());
                }
                continue;
            }
            for r in refs {
                match r.resolution {
                    Resolution::Tests(names) => {
                        evidence.insert(format!("{path} -> {} -> {}", r.includer, names.join("|")));
                        stems.extend(names);
                    }
                    Resolution::NeedsMapping => {
                        evidence.insert(format!("{path} -> {} -> test_mapping", r.includer));
                        needs.insert(r.includer);
                    }
                    Resolution::NoTests => refuse.push(format!(
                        "{path} is embedded by {} but no #[test] function can be tied to it",
                        r.includer
                    )),
                }
            }
        }
    }
    d.stems = stems.into_iter().collect();
    d.needs_mapping = needs.into_iter().collect();
    d.evidence = evidence.into_iter().collect();
    refuse.sort();
    refuse.dedup();
    d.refuse = if refuse.is_empty() {
        None
    } else {
        Some(refuse.join("; "))
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn write(root: &Path, rel: &str, body: &str) {
        let p = root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, body).unwrap();
    }

    fn cheap(p: &str) -> bool {
        p.starts_with("docs/") || p.starts_with("packaging/")
    }

    fn decide_for(root: &Path, paths: &[&str]) -> EmbeddedDecision {
        let c: Vec<String> = paths.iter().map(|s| s.to_string()).collect();
        decide(root, &c, cheap)
    }

    const ACCESSOR: &str = r#"
pub fn contract() -> &'static str { include_str!("../docs/contract.md") }
pub fn message() -> &'static str { include_str!("../templates/message.txt") }
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn contracts_document() { assert!(contract().contains("v1")); }
    #[test]
    fn contracts_template() { assert!(message().contains("hi")); }
}
"#;

    #[test]
    fn production_accessor_resolves_inline_tests() {
        let dir = tempdir().unwrap();
        write(dir.path(), "Cargo.toml", "[package]\nname=\"x\"\n");
        write(dir.path(), "src/contracts.rs", ACCESSOR);
        write(dir.path(), "docs/contract.md", "v1");
        let d = decide_for(dir.path(), &["docs/contract.md", "templates/message.txt"]);
        assert_eq!(d.refuse, None);
        assert_eq!(
            d.stems,
            vec![
                "contracts_document".to_string(),
                "contracts_template".to_string()
            ]
        );
        assert!(d.needs_mapping.is_empty());
    }

    #[test]
    fn include_in_test_fn_resolves_only_that_fn() {
        let dir = tempdir().unwrap();
        write(
            dir.path(),
            "src/a.rs",
            "#[test]\nfn one() { let _ = include_str!(\"../data/x.txt\"); }\n#[test]\nfn two() {}\n",
        );
        let d = decide_for(dir.path(), &["data/x.txt"]);
        assert_eq!(d.stems, vec!["one".to_string()]);
    }

    #[test]
    fn module_scope_const_in_test_file_resolves_all_test_fns() {
        let dir = tempdir().unwrap();
        write(
            dir.path(),
            "tests/ci/docs_truth.rs",
            "const DOCS: &[&str] = &[include_str!(\"../../README.md\")];\n#[test]\nfn alpha() {}\n#[test]\nfn beta() {}\n",
        );
        let d = decide_for(dir.path(), &["README.md"]);
        assert_eq!(d.stems, vec!["alpha".to_string(), "beta".to_string()]);
        for forbidden in ["docs_truth", "tests", "mod", "lib", "main"] {
            assert!(!d.stems.iter().any(|s| s == forbidden));
        }
    }

    #[test]
    fn production_includer_without_inline_tests_needs_mapping() {
        let dir = tempdir().unwrap();
        write(
            dir.path(),
            "src/a.rs",
            "pub fn t() -> &'static str { include_str!(\"../t.txt\") }\n",
        );
        let d = decide_for(dir.path(), &["t.txt"]);
        assert_eq!(d.needs_mapping, vec!["src/a.rs".to_string()]);
        assert!(d.stems.is_empty());
    }

    #[test]
    fn include_forms_bytes_include_brace_raw_and_manifest_concat() {
        let dir = tempdir().unwrap();
        write(dir.path(), "crates/m/Cargo.toml", "[package]\nname=\"m\"\n");
        write(
            dir.path(),
            "crates/m/src/lib.rs",
            r##"
#[cfg(test)]
mod tests {
    #[test]
    fn t_bytes() { let _ = include_bytes!("../a.bin"); }
    #[test]
    fn t_brace() { let _ = include_str! { "../b.txt" }; }
    #[test]
    fn t_raw() { let _ = include_str!(r#"../c.txt"#); }
    #[test]
    fn t_concat() { let _ = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/d.txt")); }
    // include_str!("../commented.txt")
}
"##,
        );
        for (p, name) in [
            ("crates/m/a.bin", "t_bytes"),
            ("crates/m/b.txt", "t_brace"),
            ("crates/m/c.txt", "t_raw"),
            ("crates/m/d.txt", "t_concat"),
        ] {
            let d = decide_for(dir.path(), &[p]);
            assert_eq!(d.stems, vec![name.to_string()], "{p}");
        }
        let d = decide_for(dir.path(), &["crates/m/commented.txt"]);
        assert!(d.stems.is_empty() && d.refuse.is_none());
    }

    #[test]
    fn literal_runtime_read_resolves_against_crate_root_and_repo_root() {
        let dir = tempdir().unwrap();
        write(dir.path(), "Cargo.toml", "[package]\nname=\"x\"\n");
        write(
            dir.path(),
            "tests/service.rs",
            "#[test]\nfn reads() { let _ = std::fs::read_to_string(\"templates/message.txt\"); }\n",
        );
        let d = decide_for(dir.path(), &["templates/message.txt"]);
        assert_eq!(d.stems, vec!["reads".to_string()]);
    }

    #[test]
    fn joined_components_count_as_a_read() {
        let dir = tempdir().unwrap();
        write(
            dir.path(),
            "tests/j.rs",
            "#[test]\nfn joined() { let _ = std::fs::read_to_string(std::path::Path::new(\"templates\").join(\"message.txt\")); }\n",
        );
        let d = decide_for(dir.path(), &["templates/message.txt"]);
        assert_eq!(d.stems, vec!["joined".to_string()]);
    }

    #[test]
    fn unreferenced_cheap_doc_stays_clean_even_with_dynamic_reads() {
        let dir = tempdir().unwrap();
        write(
            dir.path(),
            "tests/dyn.rs",
            "#[test]\nfn d() { let p = std::env::var(\"X\").unwrap(); let _ = std::fs::read_to_string(p); }\n",
        );
        let d = decide_for(dir.path(), &["docs/guide.md"]);
        assert_eq!(d.refuse, None);
        assert!(d.no_dependency.contains("docs/guide.md"));
    }

    #[test]
    fn unreferenced_noncheap_asset_refuses_only_with_dynamic_reads() {
        let dir = tempdir().unwrap();
        write(dir.path(), "src/lib.rs", "pub fn f() {}\n");
        let d = decide_for(dir.path(), &["assets/logo.png"]);
        assert_eq!(d.refuse, None);
        assert!(d.no_dependency.contains("assets/logo.png"));
        write(
            dir.path(),
            "tests/dyn.rs",
            "#[test]\nfn d() { let p = std::env::var(\"X\").unwrap(); let _ = std::fs::read(p); }\n",
        );
        let d = decide_for(dir.path(), &["assets/logo.png"]);
        assert!(
            d.refuse
                .as_deref()
                .unwrap_or("")
                .contains("unresolved reads")
        );
    }

    #[test]
    fn out_dir_include_is_unresolved() {
        let dir = tempdir().unwrap();
        write(
            dir.path(),
            "src/gen.rs",
            "pub const G: &str = include_str!(concat!(env!(\"OUT_DIR\"), \"/g.rs\"));\n",
        );
        let d = decide_for(dir.path(), &["data/x.csv"]);
        assert!(d.refuse.is_some());
        let d = decide_for(dir.path(), &["docs/x.md"]);
        assert_eq!(d.refuse, None);
    }

    #[test]
    fn referenced_without_tests_refuses() {
        let dir = tempdir().unwrap();
        write(
            dir.path(),
            "tests/common/mod.rs",
            "pub fn d() -> &'static str { include_str!(\"../../x.txt\") }\n",
        );
        let d = decide_for(dir.path(), &["x.txt"]);
        assert!(d.refuse.as_deref().unwrap_or("").contains("no #[test]"));
    }

    #[test]
    fn escaping_include_is_not_attributed_and_does_not_panic() {
        let dir = tempdir().unwrap();
        write(
            dir.path(),
            "a.rs",
            "#[test]\nfn t() { let _ = include_str!(\"../../../etc/x\"); }\n",
        );
        let d = decide_for(dir.path(), &["etc/x"]);
        assert!(d.stems.is_empty());
    }

    #[test]
    fn spaces_unicode_and_backslash_paths_normalize() {
        let dir = tempdir().unwrap();
        write(
            dir.path(),
            "src/né.rs",
            "#[test]\nfn uni() { let _ = include_str!(\"../my docs/ünï.md\"); }\n",
        );
        let d = decide_for(dir.path(), &["my docs\\ünï.md".replace('\\', "/").as_str()]);
        assert_eq!(d.stems, vec!["uni".to_string()]);
        assert_eq!(
            normalize_repo_path(".\\a\\.\\b\\..\\c.md").as_deref(),
            Some("a/c.md")
        );
        assert_eq!(normalize_repo_path("../x"), None);
    }

    #[test]
    fn scan_is_deterministic() {
        let dir = tempdir().unwrap();
        write(dir.path(), "src/contracts.rs", ACCESSOR);
        let a = decide_for(dir.path(), &["docs/contract.md", "templates/message.txt"]);
        let b = decide_for(dir.path(), &["templates/message.txt", "docs/contract.md"]);
        assert_eq!(a.stems, b.stems);
        assert_eq!(a.evidence, b.evidence);
    }

    #[test]
    fn caps_yield_bounded_refusal() {
        let scan = EmbeddedScan {
            bounded: Some("embedded scan bound exceeded: test".to_string()),
            ..Default::default()
        };
        let mut d = EmbeddedDecision::default();
        decide_with_scan(&scan, &["docs/x.md".to_string()], cheap, &mut d);
        assert!(d.refuse.as_deref().unwrap_or("").contains("bound exceeded"));
    }

    #[test]
    fn oversized_file_trips_per_file_cap() {
        let dir = tempdir().unwrap();
        let big = "// x\n".repeat((MAX_FILE_BYTES as usize / 5) + 10);
        write(dir.path(), "src/big.rs", &big);
        let scan = EmbeddedScan::scan(dir.path());
        assert!(scan.bounded.as_deref().unwrap_or("").contains("bytes"));
    }

    #[test]
    fn unlexable_file_is_reported_not_swallowed() {
        let dir = tempdir().unwrap();
        write(
            dir.path(),
            "src/bad.rs",
            "fn f() { include_str!(\"a\"); /* never closed\n",
        );
        let d = decide_for(dir.path(), &["docs/a.md"]);
        assert!(
            d.refuse
                .as_deref()
                .unwrap_or("")
                .contains("could not read or lex")
        );
    }

    #[test]
    fn crlf_sources_resolve() {
        let dir = tempdir().unwrap();
        write(
            dir.path(),
            "src/c.rs",
            "#[test]\r\nfn crlf() {\r\n    let _ = include_str!(\"../x.txt\");\r\n}\r\n",
        );
        let d = decide_for(dir.path(), &["x.txt"]);
        assert_eq!(d.stems, vec!["crlf".to_string()]);
    }

    #[test]
    fn candidate_paths_skip_source_and_include_rename_old_path() {
        use crate::impact::packet::ChangedFile;
        use std::path::PathBuf;
        let mk = |p: &str, old: Option<&str>| ChangedFile {
            path: PathBuf::from(p),
            old_path: old.map(PathBuf::from),
            ..Default::default()
        };
        let packet = ImpactPacket {
            changes: vec![
                mk("src/a.rs", None),
                mk("docs/new.md", Some("docs/old.md")),
                mk("t\\m.txt", None),
            ],
            ..ImpactPacket::default()
        };
        assert_eq!(
            candidate_paths(&packet),
            vec![
                "docs/new.md".to_string(),
                "docs/old.md".to_string(),
                "t/m.txt".to_string()
            ]
        );
    }
}

#[cfg(test)]
mod engine_tests {
    use super::*;
    use crate::verify::plan::non_code::INCLUDE_STR_MATERIAL;

    /// D5: on the engine tree the lookup reproduces every hand-written
    /// `INCLUDE_STR_MATERIAL` stem (table ⊆ lookup) and never emits a vacuous
    /// file/binary stem. The engine itself keeps using the finer table.
    #[test]
    fn engine_lookup_covers_include_str_material_table() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let scan = EmbeddedScan::scan(root);
        assert!(scan.bounded.is_none(), "{:?}", scan.bounded);
        assert!(scan.errors.is_empty(), "{:?}", scan.errors);
        for (path, stems) in INCLUDE_STR_MATERIAL {
            let mut got: BTreeSet<String> = BTreeSet::new();
            for r in scan.refs_for(path) {
                if let Resolution::Tests(names) = r.resolution {
                    got.extend(names);
                }
            }
            for stem in *stems {
                assert!(
                    got.contains(*stem),
                    "{path}: lookup missed table stem {stem}"
                );
            }
            for forbidden in ["install_docs_truth", "tests", "mod", "lib", "main"] {
                assert!(!got.contains(forbidden), "{path}: vacuous stem {forbidden}");
            }
        }
    }

    /// D2: warm scan of the engine tree stays inside the latency budget
    /// (generous slack for debug builds and shared CI runners).
    #[test]
    fn engine_scan_latency_guard() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let _warm = EmbeddedScan::scan(root);
        let t = std::time::Instant::now();
        let _ = EmbeddedScan::scan(root);
        let ms = t.elapsed().as_millis();
        eprintln!("embedded engine scan warm ms={ms}");
        let budget = if cfg!(debug_assertions) { 6000 } else { 1500 };
        assert!(ms < budget, "scan took {ms} ms (budget {budget} ms)");
    }
}
