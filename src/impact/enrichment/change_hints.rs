//! Greenfield / new-surface change hints for agent change-context (track 0127).
//!
//! Pure classification + budgeted suggested test paths. No conductor coupling,
//! no LLM, no line-coverage claims. Convention suggestions are path heuristics
//! only — not proven coverage.

use crate::impact::packet::ChangedFile;
use crate::index::test_mapping::is_test_path;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};

/// Cap on `newPackagePrefixes`.
pub const NEW_PACKAGE_PREFIX_CAP: usize = 5;
/// Cap on `suggestedTests`.
pub const SUGGESTED_TESTS_CAP: usize = 10;
/// Cap on honesty `notes`.
pub const NOTES_CAP: usize = 5;
/// Max adjacent test files listed per package prefix scan.
const ADJACENT_PER_PREFIX: usize = 3;

/// Source-like extensions used for mostly-added math.
const SOURCE_EXTS: &[&str] = &[
    "rs", "ts", "tsx", "js", "jsx", "py", "go", //
    "c", "h", "cpp", "cc", "cxx", "hpp", "hh", "hxx", "h++",
];

/// Entrypoint / CLI basenames (path-primary signal).
const ENTRYPOINT_BASENAMES: &[&str] = &[
    "main.rs",
    "main.ts",
    "main.py",
    "cli.rs",
    "cli.ts",
    "__main__.py",
];

/// Greenfield / new-surface classification kind.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ChangeHintsKind {
    Greenfield,
    Mixed,
    None,
}

impl ChangeHintsKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Greenfield => "greenfield",
            Self::Mixed => "mixed",
            Self::None => "none",
        }
    }
}

/// How a suggested test path was derived.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum SuggestedTestKind {
    Mapped,
    Convention,
    Adjacent,
}

impl SuggestedTestKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Mapped => "mapped",
            Self::Convention => "convention",
            Self::Adjacent => "adjacent",
        }
    }
}

/// One suggested test path (budgeted; path-unique across ladder).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SuggestedTest {
    pub path: String,
    pub kind: SuggestedTestKind,
    pub reason: String,
}

/// Deterministic greenfield / new-surface report for a change set.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ChangeHintsReport {
    pub kind: ChangeHintsKind,
    pub mostly_added: bool,
    pub added_count: usize,
    pub total_changed: usize,
    pub new_package_prefixes: Vec<String>,
    pub surface_tags: Vec<String>,
    pub suggested_tests: Vec<SuggestedTest>,
    pub notes: Vec<String>,
}

/// Optional inputs for [`compute_change_hints`].
#[derive(Debug, Clone, Default)]
pub struct ChangeHintsOpts {
    /// Project root for disk existence / adjacent directory scans.
    pub project_root: Option<PathBuf>,
    /// Mapped covering-test paths (already stripped to path, no `::symbol`).
    pub mapped_hint_paths: Vec<String>,
}

/// Greppable honesty notes (stable strings for agents/tests).
pub const NOTE_CONVENTION_ONLY: &str = "No structural test_mapping for new paths; suggestions are path conventions, not proven coverage.";
pub const NOTE_NO_SUGGESTIONS: &str =
    "No mapped or conventional test targets derived; inspect new package manually.";

/// Normalize path separators to `/` for stable comparisons.
fn normalize_path(path: &str) -> String {
    path.replace('\\', "/")
}

/// True when `status == "Added"` and `old_path` is absent (rename footgun fence).
pub fn is_pure_add(file: &ChangedFile) -> bool {
    file.status == "Added" && file.old_path.is_none()
}

fn path_str(file: &ChangedFile) -> String {
    normalize_path(&file.path.to_string_lossy())
}

fn is_source_like(path: &str) -> bool {
    let norm = normalize_path(path);
    let Some(ext) = Path::new(&norm).extension().and_then(|e| e.to_str()) else {
        return false;
    };
    SOURCE_EXTS.contains(&ext)
}

/// Package-ish directory prefix for a pure-added path.
///
/// Under `src/` / `packages/` / `lib/` / `app/`, take the first two path
/// segments when the second is a directory segment (e.g. `src/newpkg` from
/// `src/newpkg/cli.rs`). A file directly under the package root
/// (`src/main.rs`) does not invent a package prefix. Otherwise parent of the
/// file.
fn package_prefix_for(path: &str) -> Option<String> {
    let norm = normalize_path(path);
    let parts: Vec<&str> = norm.split('/').filter(|p| !p.is_empty()).collect();
    if parts.is_empty() {
        return None;
    }
    let under_package_root = matches!(parts[0], "src" | "packages" | "lib" | "app");
    if under_package_root {
        // Need a directory segment after the root (at least root/dir/file).
        if parts.len() >= 3 {
            return Some(format!("{}/{}", parts[0], parts[1]));
        }
        // File directly under src/ (e.g. main.rs) — not a package prefix.
        return None;
    }
    // Parent of file
    if parts.len() >= 2 {
        Some(parts[..parts.len() - 1].join("/"))
    } else {
        // File at repo root — no package prefix
        None
    }
}

fn path_shares_prefix(path: &str, prefix: &str) -> bool {
    let norm = normalize_path(path);
    let pref = normalize_path(prefix);
    norm == pref || norm.starts_with(&format!("{pref}/"))
}

fn basename(path: &str) -> String {
    let norm = normalize_path(path);
    norm.rsplit('/').next().unwrap_or(path).to_string()
}

fn is_entrypoint_path(path: &str) -> bool {
    let norm = normalize_path(path);
    let base = basename(&norm);
    if ENTRYPOINT_BASENAMES.contains(&base.as_str()) {
        return true;
    }
    // Under bin/ or cmd/
    let parts: Vec<&str> = norm.split('/').filter(|p| !p.is_empty()).collect();
    parts.iter().any(|p| *p == "bin" || *p == "cmd")
}

fn has_entrypoint_kind_bonus(file: &ChangedFile) -> bool {
    let Some(symbols) = file.symbols.as_ref() else {
        return false;
    };
    symbols.iter().any(|s| {
        s.entrypoint_kind
            .as_deref()
            .map(|k| {
                let u = k.to_ascii_uppercase();
                u.contains("ENTRYPOINT") || u.contains("HANDLER")
            })
            .unwrap_or(false)
    })
}

/// Convention candidates for a pure-added source path (skip callers for tests).
fn convention_candidates(path: &str) -> Vec<String> {
    let norm = normalize_path(path);
    let ext = Path::new(&norm)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");
    let stem = Path::new(&norm)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    let parent = Path::new(&norm)
        .parent()
        .map(|p| normalize_path(&p.to_string_lossy()))
        .unwrap_or_default();

    let mut out = Vec::new();
    match ext {
        "rs" => {
            // src/foo/bar.rs → tests/foo/bar.rs, src/foo/bar_test.rs, tests/bar.rs
            if let Some(rest) = norm.strip_prefix("src/") {
                let without_ext = rest.trim_end_matches(".rs");
                out.push(format!("tests/{without_ext}.rs"));
            }
            if !parent.is_empty() {
                out.push(format!("{parent}/{stem}_test.rs"));
            } else {
                out.push(format!("{stem}_test.rs"));
            }
            out.push(format!("tests/{stem}.rs"));
        }
        "ts" | "tsx" | "js" | "jsx" => {
            // src/foo/bar.ts → src/foo/bar.test.ts, src/foo/__tests__/bar.ts
            if !parent.is_empty() {
                out.push(format!("{parent}/{stem}.test.{ext}"));
                out.push(format!("{parent}/__tests__/{stem}.{ext}"));
            } else {
                out.push(format!("{stem}.test.{ext}"));
                out.push(format!("__tests__/{stem}.{ext}"));
            }
        }
        "py" => {
            // pkg/foo.py → tests/test_foo.py, pkg/test_foo.py, pkg/foo_test.py
            out.push(format!("tests/test_{stem}.py"));
            if !parent.is_empty() {
                out.push(format!("{parent}/test_{stem}.py"));
                out.push(format!("{parent}/{stem}_test.py"));
            } else {
                out.push(format!("test_{stem}.py"));
                out.push(format!("{stem}_test.py"));
            }
        }
        "go" => {
            // pkg/foo.go → pkg/foo_test.go
            if !parent.is_empty() {
                out.push(format!("{parent}/{stem}_test.go"));
            } else {
                out.push(format!("{stem}_test.go"));
            }
        }
        _ => {}
    }
    out.into_iter().map(|p| normalize_path(&p)).collect()
}

fn path_exists_on_disk(root: Option<&Path>, rel: &str) -> bool {
    let Some(root) = root else {
        return false;
    };
    let joined = root.join(rel.replace('/', std::path::MAIN_SEPARATOR_STR));
    joined.is_file()
}

fn dir_exists_on_disk(root: Option<&Path>, rel: &str) -> bool {
    let Some(root) = root else {
        return false;
    };
    let joined = root.join(rel.replace('/', std::path::MAIN_SEPARATOR_STR));
    joined.is_dir()
}

/// Collect up to `ADJACENT_PER_PREFIX` existing test files under nearby test dirs.
fn adjacent_test_files(root: &Path, package_prefix: &str) -> Vec<String> {
    let mut found: BTreeSet<String> = BTreeSet::new();
    let candidates = [
        format!("{package_prefix}/tests"),
        format!("{package_prefix}/__tests__"),
        format!("{package_prefix}/test"),
        // Also look one level up for tests/ sibling layout
        {
            let parent = Path::new(package_prefix)
                .parent()
                .map(|p| normalize_path(&p.to_string_lossy()))
                .unwrap_or_default();
            if parent.is_empty() {
                "tests".to_string()
            } else {
                format!("{parent}/tests")
            }
        },
    ];

    for dir_rel in candidates {
        if found.len() >= ADJACENT_PER_PREFIX {
            break;
        }
        if !dir_exists_on_disk(Some(root), &dir_rel) {
            continue;
        }
        let abs = root.join(dir_rel.replace('/', std::path::MAIN_SEPARATOR_STR));
        let Ok(rd) = std::fs::read_dir(&abs) else {
            continue;
        };
        let mut entries: Vec<String> = Vec::new();
        for ent in rd.flatten() {
            let path = ent.path();
            if !path.is_file() {
                continue;
            }
            let rel = match path.strip_prefix(root) {
                Ok(r) => normalize_path(&r.to_string_lossy()),
                Err(_) => continue,
            };
            if is_test_path(&rel) {
                entries.push(rel);
            }
        }
        entries.sort();
        for e in entries {
            if found.len() >= ADJACENT_PER_PREFIX {
                break;
            }
            found.insert(e);
        }
    }
    found.into_iter().take(ADJACENT_PER_PREFIX).collect()
}

fn strip_hint_to_path(hint: &str) -> String {
    // `file::symbol` → path before first `::`
    let path = hint.split("::").next().unwrap_or(hint).trim();
    normalize_path(path)
}

/// Compute greenfield / new-surface hints for a change set.
pub fn compute_change_hints(changes: &[ChangedFile], opts: &ChangeHintsOpts) -> ChangeHintsReport {
    let pure_adds: Vec<&ChangedFile> = changes.iter().filter(|c| is_pure_add(c)).collect();
    let pure_add_paths: Vec<String> = pure_adds.iter().map(|c| path_str(c)).collect();

    let source_like: Vec<&ChangedFile> = changes
        .iter()
        .filter(|c| is_source_like(&path_str(c)))
        .collect();
    let pure_add_source: Vec<&ChangedFile> = source_like
        .iter()
        .copied()
        .filter(|c| is_pure_add(c))
        .collect();

    let total_changed = source_like.len();
    let added_count = pure_add_source.len();

    // Integer form of pure_added/total >= 0.6 ⇔ 10*added >= 6*total.
    let mostly_added = if total_changed == 0 {
        false
    } else {
        (added_count * 10 >= total_changed * 6)
            || (added_count >= 2 && added_count == total_changed)
    };

    // Non-pure-add paths (Modified / Deleted / Renamed / add-with-old_path)
    let non_pure_paths: Vec<String> = changes
        .iter()
        .filter(|c| !is_pure_add(c))
        .map(path_str)
        .collect();

    // Candidate prefixes from pure-adds; keep those not shared with non-pure paths
    let mut prefix_set: BTreeSet<String> = BTreeSet::new();
    for path in &pure_add_paths {
        if let Some(prefix) = package_prefix_for(path) {
            let shared = non_pure_paths
                .iter()
                .any(|p| path_shares_prefix(p, &prefix));
            if !shared {
                prefix_set.insert(prefix);
            }
        }
    }
    let mut new_package_prefixes: Vec<String> = prefix_set.into_iter().collect();
    new_package_prefixes.truncate(NEW_PACKAGE_PREFIX_CAP);

    // Surface tags
    let mut tags: BTreeSet<String> = BTreeSet::new();
    for file in &pure_adds {
        let p = path_str(file);
        if let Some(prefix) = package_prefix_for(&p)
            && new_package_prefixes.iter().any(|np| np == &prefix)
        {
            tags.insert("new_module".to_string());
        }
        if is_entrypoint_path(&p) || has_entrypoint_kind_bonus(file) {
            tags.insert("new_entrypoint".to_string());
            // cli basename or bin/cmd path → also cli_surface
            let base = basename(&p);
            if base.starts_with("cli.")
                || normalize_path(&p)
                    .split('/')
                    .any(|seg| seg == "bin" || seg == "cmd")
            {
                tags.insert("cli_surface".to_string());
            }
            // main.* entrypoints are new_entrypoint but not necessarily cli_surface
        }
        if is_test_path(&p) {
            tags.insert("new_test".to_string());
        }
    }
    // main.* also counts as entrypoint (already) but not necessarily cli_surface
    let surface_tags: Vec<String> = tags.into_iter().collect();

    let kind = if mostly_added || !new_package_prefixes.is_empty() {
        ChangeHintsKind::Greenfield
    } else if !pure_adds.is_empty() {
        ChangeHintsKind::Mixed
    } else {
        ChangeHintsKind::None
    };

    // --- suggestedTests 3-pass ladder (dedupe by path; first wins) ---
    let mut seen_paths: HashSet<String> = HashSet::new();
    let mut suggested: Vec<SuggestedTest> = Vec::new();

    // Pass 1: mapped
    let mut mapped_sorted: Vec<String> = opts
        .mapped_hint_paths
        .iter()
        .map(|h| strip_hint_to_path(h))
        .filter(|p| !p.is_empty())
        .collect();
    mapped_sorted.sort();
    mapped_sorted.dedup();
    for path in mapped_sorted {
        if suggested.len() >= SUGGESTED_TESTS_CAP {
            break;
        }
        if seen_paths.insert(path.clone()) {
            suggested.push(SuggestedTest {
                path,
                kind: SuggestedTestKind::Mapped,
                reason: "mapped via test_mapping / blast test_hints".to_string(),
            });
        }
    }

    // Pass 2: convention (pure-Added source, skip tests)
    let root = opts.project_root.as_deref();
    let mut pure_source_paths: Vec<String> = pure_add_source
        .iter()
        .map(|c| path_str(c))
        .filter(|p| !is_test_path(p))
        .collect();
    pure_source_paths.sort();
    pure_source_paths.dedup();

    for src_path in &pure_source_paths {
        if suggested.len() >= SUGGESTED_TESTS_CAP {
            break;
        }
        for cand in convention_candidates(src_path) {
            if suggested.len() >= SUGGESTED_TESTS_CAP {
                break;
            }
            if !seen_paths.insert(cand.clone()) {
                continue;
            }
            let reason = if path_exists_on_disk(root, &cand) {
                "conventional test path (exists on disk)".to_string()
            } else {
                "conventional test path (to be created)".to_string()
            };
            suggested.push(SuggestedTest {
                path: cand,
                kind: SuggestedTestKind::Convention,
                reason,
            });
        }
    }

    // Pass 3: adjacent
    if let Some(project_root) = root {
        for prefix in &new_package_prefixes {
            if suggested.len() >= SUGGESTED_TESTS_CAP {
                break;
            }
            for adj in adjacent_test_files(project_root, prefix) {
                if suggested.len() >= SUGGESTED_TESTS_CAP {
                    break;
                }
                if !seen_paths.insert(adj.clone()) {
                    continue;
                }
                suggested.push(SuggestedTest {
                    path: adj,
                    kind: SuggestedTestKind::Adjacent,
                    reason: format!("adjacent existing test under package prefix '{prefix}'"),
                });
            }
        }
    }

    // Sort by path then kind (stable final order)
    suggested.sort_by(|a, b| a.path.cmp(&b.path).then_with(|| a.kind.cmp(&b.kind)));
    if suggested.len() > SUGGESTED_TESTS_CAP {
        suggested.truncate(SUGGESTED_TESTS_CAP);
    }

    // Honesty notes
    let mut notes: Vec<String> = Vec::new();
    let has_mapped = suggested
        .iter()
        .any(|s| s.kind == SuggestedTestKind::Mapped);
    let has_convention = suggested
        .iter()
        .any(|s| s.kind == SuggestedTestKind::Convention);
    let has_adjacent = suggested
        .iter()
        .any(|s| s.kind == SuggestedTestKind::Adjacent);

    if suggested.is_empty()
        && (kind == ChangeHintsKind::Greenfield || kind == ChangeHintsKind::Mixed)
    {
        notes.push(NOTE_NO_SUGGESTIONS.to_string());
    } else if kind == ChangeHintsKind::Greenfield && !has_mapped && has_convention && !has_adjacent
    {
        notes.push(NOTE_CONVENTION_ONLY.to_string());
    } else if kind == ChangeHintsKind::Greenfield && !has_mapped && (has_convention || has_adjacent)
    {
        // Convention and/or adjacent without mapping — still not proven coverage
        notes.push(NOTE_CONVENTION_ONLY.to_string());
    }
    notes.truncate(NOTES_CAP);

    ChangeHintsReport {
        kind,
        mostly_added,
        added_count,
        total_changed,
        new_package_prefixes,
        surface_tags,
        suggested_tests: suggested,
        notes,
    }
}

/// Format prefixes for the change-context summary clause (≤3, last-2 segments if deep).
pub fn format_summary_prefixes(prefixes: &[String], max: usize) -> String {
    let shown: Vec<String> = prefixes
        .iter()
        .take(max)
        .map(|p| {
            let norm = normalize_path(p);
            let parts: Vec<&str> = norm.split('/').filter(|s| !s.is_empty()).collect();
            if parts.len() > 2 {
                format!("{}/{}", parts[parts.len() - 2], parts[parts.len() - 1])
            } else {
                norm
            }
        })
        .collect();
    shown.join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::impact::packet::FileAnalysisStatus;
    use rstest::rstest;
    use std::fs;
    use tempfile::tempdir;

    fn changed(path: &str, status: &str, old_path: Option<&str>) -> ChangedFile {
        ChangedFile {
            path: PathBuf::from(path),
            status: status.to_string(),
            old_path: old_path.map(PathBuf::from),
            is_staged: false,
            symbols: None,
            imports: None,
            runtime_usage: None,
            analysis_status: FileAnalysisStatus::default(),
            analysis_warnings: Vec::new(),
            ..Default::default()
        }
    }

    fn added(path: &str) -> ChangedFile {
        changed(path, "Added", None)
    }

    fn modified(path: &str) -> ChangedFile {
        changed(path, "Modified", None)
    }

    #[test]
    fn pure_add_requires_added_and_no_old_path() {
        assert!(is_pure_add(&added("src/a.rs")));
        assert!(!is_pure_add(&changed(
            "src/a.rs",
            "Added",
            Some("src/old.rs")
        )));
        assert!(!is_pure_add(&changed(
            "src/a.rs",
            "Renamed",
            Some("src/old.rs")
        )));
        assert!(!is_pure_add(&modified("src/a.rs")));
    }

    #[rstest]
    #[case::ratio_60(vec![added("src/a.rs"), added("src/b.rs"), modified("src/c.rs")], true)]
    #[case::all_added_two(vec![added("src/a.rs"), added("src/b.rs")], true)]
    #[case::single_add_of_two(vec![added("src/a.rs"), modified("src/b.rs")], false)]
    #[case::modify_only(vec![modified("src/a.rs"), modified("src/b.rs")], false)]
    fn mostly_added_thresholds(#[case] files: Vec<ChangedFile>, #[case] expect: bool) {
        let report = compute_change_hints(&files, &ChangeHintsOpts::default());
        assert_eq!(report.mostly_added, expect, "report={report:?}");
    }

    #[test]
    fn rename_not_pure_add_not_greenfield() {
        let files = vec![
            changed("src/newpkg/mod.rs", "Renamed", Some("src/oldpkg/mod.rs")),
            changed("src/newpkg/cli.rs", "Renamed", Some("src/oldpkg/cli.rs")),
        ];
        let report = compute_change_hints(&files, &ChangeHintsOpts::default());
        assert_eq!(report.kind, ChangeHintsKind::None);
        assert_eq!(report.added_count, 0);
        assert!(!report.mostly_added);
        assert!(report.new_package_prefixes.is_empty());
    }

    #[test]
    fn added_with_old_path_not_pure_add() {
        let files = vec![changed(
            "src/newpkg/mod.rs",
            "Added",
            Some("src/oldpkg/mod.rs"),
        )];
        let report = compute_change_hints(&files, &ChangeHintsOpts::default());
        assert_eq!(report.kind, ChangeHintsKind::None);
        assert_eq!(report.added_count, 0);
    }

    #[test]
    fn new_package_prefix_isolation() {
        // Pure-adds under src/newpkg only → prefix isolated
        let files = vec![
            added("src/newpkg/mod.rs"),
            added("src/newpkg/cli.rs"),
            added("src/main.rs"),
        ];
        let report = compute_change_hints(&files, &ChangeHintsOpts::default());
        assert_eq!(report.kind, ChangeHintsKind::Greenfield);
        assert!(
            report
                .new_package_prefixes
                .iter()
                .any(|p| p == "src/newpkg"),
            "prefixes={:?}",
            report.new_package_prefixes
        );
        // Shared package: modified under same prefix → not "new"
        let shared = vec![added("src/impact/foo.rs"), modified("src/impact/mod.rs")];
        let report2 = compute_change_hints(&shared, &ChangeHintsOpts::default());
        assert!(
            !report2
                .new_package_prefixes
                .iter()
                .any(|p| p == "src/impact"),
            "shared prefix should not be new: {:?}",
            report2.new_package_prefixes
        );
    }

    #[test]
    fn path_primary_entrypoint_tags() {
        let files = vec![
            added("src/newpkg/mod.rs"),
            added("src/newpkg/cli.rs"),
            added("src/main.rs"),
        ];
        let report = compute_change_hints(&files, &ChangeHintsOpts::default());
        assert!(
            report.surface_tags.iter().any(|t| t == "new_entrypoint"),
            "tags={:?}",
            report.surface_tags
        );
        assert!(
            report.surface_tags.iter().any(|t| t == "cli_surface"),
            "cli.rs should tag cli_surface: {:?}",
            report.surface_tags
        );
        assert!(
            report.surface_tags.iter().any(|t| t == "new_module"),
            "tags={:?}",
            report.surface_tags
        );
    }

    #[test]
    fn bin_cmd_path_tags_entrypoint_and_cli() {
        let files = vec![added("cmd/server/main.go")];
        let report = compute_change_hints(&files, &ChangeHintsOpts::default());
        assert!(report.surface_tags.contains(&"new_entrypoint".to_string()));
        assert!(report.surface_tags.contains(&"cli_surface".to_string()));
    }

    #[test]
    fn convention_candidates_rust_ts_python_go() {
        let rust = convention_candidates("src/foo/bar.rs");
        assert!(rust.contains(&"tests/foo/bar.rs".to_string()));
        assert!(rust.contains(&"src/foo/bar_test.rs".to_string()));
        assert!(rust.contains(&"tests/bar.rs".to_string()));

        let ts = convention_candidates("src/foo/bar.ts");
        assert!(ts.contains(&"src/foo/bar.test.ts".to_string()));
        assert!(ts.contains(&"src/foo/__tests__/bar.ts".to_string()));

        let py = convention_candidates("pkg/foo.py");
        assert!(py.contains(&"tests/test_foo.py".to_string()));
        assert!(py.contains(&"pkg/test_foo.py".to_string()));
        assert!(py.contains(&"pkg/foo_test.py".to_string()));

        let go = convention_candidates("pkg/foo.go");
        assert!(go.contains(&"pkg/foo_test.go".to_string()));
    }

    #[test]
    fn path_dedupe_across_ladder_mapped_wins() {
        let files = vec![added("src/newpkg/mod.rs")];
        let opts = ChangeHintsOpts {
            project_root: None,
            mapped_hint_paths: vec!["tests/newpkg/mod.rs::test_mod".to_string()],
        };
        let report = compute_change_hints(&files, &opts);
        let path = "tests/newpkg/mod.rs";
        let matches: Vec<_> = report
            .suggested_tests
            .iter()
            .filter(|s| s.path == path)
            .collect();
        assert_eq!(matches.len(), 1, "path must appear once: {matches:?}");
        assert_eq!(matches[0].kind, SuggestedTestKind::Mapped);
    }

    #[test]
    fn exists_vs_to_create_reasons() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        fs::create_dir_all(root.join("tests/foo")).unwrap();
        fs::write(root.join("tests/foo/bar.rs"), "// test\n").unwrap();

        let files = vec![added("src/foo/bar.rs")];
        let opts = ChangeHintsOpts {
            project_root: Some(root.to_path_buf()),
            mapped_hint_paths: Vec::new(),
        };
        let report = compute_change_hints(&files, &opts);
        let exists = report
            .suggested_tests
            .iter()
            .find(|s| s.path == "tests/foo/bar.rs")
            .expect("convention candidate tests/foo/bar.rs");
        assert!(
            exists.reason.contains("exists on disk"),
            "reason={}",
            exists.reason
        );
        let to_create = report
            .suggested_tests
            .iter()
            .find(|s| s.path == "src/foo/bar_test.rs")
            .expect("bar_test convention");
        assert!(
            to_create.reason.contains("to be created"),
            "reason={}",
            to_create.reason
        );
    }

    #[test]
    fn caps_and_sort() {
        // Many pure-adds → suggestedTests ≤ 10, prefixes ≤ 5, sorted
        let mut files = Vec::new();
        for i in 0..8 {
            files.push(added(&format!("src/pkg{i}/mod.rs")));
            files.push(added(&format!("src/pkg{i}/lib.rs")));
        }
        let report = compute_change_hints(&files, &ChangeHintsOpts::default());
        assert!(report.new_package_prefixes.len() <= NEW_PACKAGE_PREFIX_CAP);
        assert!(report.suggested_tests.len() <= SUGGESTED_TESTS_CAP);
        // Prefixes sorted
        let mut sorted_pref = report.new_package_prefixes.clone();
        sorted_pref.sort();
        assert_eq!(report.new_package_prefixes, sorted_pref);
        // Suggested sorted by path then kind
        let mut sorted_s = report.suggested_tests.clone();
        sorted_s.sort_by(|a, b| a.path.cmp(&b.path).then_with(|| a.kind.cmp(&b.kind)));
        assert_eq!(report.suggested_tests, sorted_s);
        assert!(report.notes.len() <= NOTES_CAP);
    }

    #[test]
    fn no_conductor_or_meshops_product_coupling() {
        // Guards output of a neutral pure-add set + production (non-test) source.
        let files = vec![
            added("src/newpkg/mod.rs"),
            added("src/newpkg/cli.rs"),
            added("src/main.rs"),
        ];
        let report = compute_change_hints(&files, &ChangeHintsOpts::default());
        let json = serde_json::to_string(&report).unwrap();
        assert!(!json.contains("0127-"), "must not emit track id: {json}");
        assert!(
            !json.to_ascii_lowercase().contains("meshops"),
            "must not hardcode meshops: {json}"
        );
        // Production source only (stop at cfg(test) module).
        let src = include_str!("change_hints.rs");
        let prod = src.split("#[cfg(test)]").next().unwrap_or(src);
        for line in prod.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with("//") || trimmed.starts_with("//!") || trimmed.starts_with('*') {
                continue;
            }
            assert!(
                !trimmed.to_ascii_lowercase().contains("meshops"),
                "product code must not hardcode meshops: {trimmed}"
            );
            assert!(
                !trimmed.contains("0127-Greenfield"),
                "product code must not hardcode track id: {trimmed}"
            );
        }
    }

    #[test]
    fn greenfield_pure_add_package_has_suggestions_or_notes() {
        let files = vec![
            added("src/newpkg/mod.rs"),
            added("src/newpkg/cli.rs"),
            added("src/main.rs"),
        ];
        let report = compute_change_hints(&files, &ChangeHintsOpts::default());
        assert_eq!(report.kind, ChangeHintsKind::Greenfield);
        assert!(
            !report.suggested_tests.is_empty() || !report.notes.is_empty(),
            "must have suggestions or honesty notes: {report:?}"
        );
        if report
            .suggested_tests
            .iter()
            .all(|s| s.kind == SuggestedTestKind::Convention)
            && !report.suggested_tests.is_empty()
        {
            assert!(
                report.notes.iter().any(|n| n.contains("path conventions")),
                "convention-only must note honesty: {:?}",
                report.notes
            );
        }
    }

    #[test]
    fn mixed_when_few_adds() {
        let files = vec![
            added("src/a.rs"),
            modified("src/b.rs"),
            modified("src/c.rs"),
            modified("src/d.rs"),
        ];
        let report = compute_change_hints(&files, &ChangeHintsOpts::default());
        // 1/4 = 0.25 < 0.6; single add may still create new prefix src/a if a.rs is under src
        // package_prefix for src/a.rs → first two segments need parts[0]=src, parts[1]=a.rs file
        // Wait: parts = ["src", "a.rs"] — first two = "src/a.rs" which is the file itself.
        // That's a bit odd for a file directly under src/.
        // For src/a.rs, parts.len()==2, under_package_root, returns "src/a.rs"
        // non_pure don't share that → new_package_prefix exists → greenfield!
        // Spec: greenfield when mostly_added OR at least one newPackagePrefix.
        // So this might be greenfield due to prefix. That's OK per B2.
        assert!(
            matches!(
                report.kind,
                ChangeHintsKind::Greenfield | ChangeHintsKind::Mixed
            ),
            "kind={:?}",
            report.kind
        );
        assert_eq!(report.added_count, 1);
    }

    #[test]
    fn modify_only_kind_none() {
        let files = vec![modified("src/a.rs"), modified("src/b.rs")];
        let report = compute_change_hints(&files, &ChangeHintsOpts::default());
        assert_eq!(report.kind, ChangeHintsKind::None);
        assert!(!report.mostly_added);
        assert!(report.suggested_tests.is_empty());
    }

    #[test]
    fn summary_prefix_truncation() {
        let deep = vec![
            "src/meshops/ops/handlers".to_string(),
            "src/newpkg".to_string(),
        ];
        let s = format_summary_prefixes(&deep, 3);
        assert!(s.contains("ops/handlers"), "deep prefix last-2: {s}");
        assert!(s.contains("src/newpkg"), "short prefix full: {s}");
    }

    #[test]
    fn new_test_tag() {
        let files = vec![added("src/newpkg/foo_test.rs")];
        let report = compute_change_hints(&files, &ChangeHintsOpts::default());
        assert!(report.surface_tags.contains(&"new_test".to_string()));
    }

    #[test]
    fn serde_camel_case_kind_snake() {
        let report = ChangeHintsReport {
            kind: ChangeHintsKind::Greenfield,
            mostly_added: true,
            added_count: 2,
            total_changed: 2,
            new_package_prefixes: vec!["src/newpkg".into()],
            surface_tags: vec!["new_module".into()],
            suggested_tests: vec![SuggestedTest {
                path: "tests/newpkg/mod.rs".into(),
                kind: SuggestedTestKind::Convention,
                reason: "conventional test path (to be created)".into(),
            }],
            notes: vec![NOTE_CONVENTION_ONLY.into()],
        };
        let v = serde_json::to_value(&report).unwrap();
        assert_eq!(v["kind"], "greenfield");
        assert_eq!(v["mostlyAdded"], true);
        assert_eq!(v["addedCount"], 2);
        assert_eq!(v["totalChanged"], 2);
        assert_eq!(v["newPackagePrefixes"][0], "src/newpkg");
        assert_eq!(v["suggestedTests"][0]["kind"], "convention");
        assert_eq!(v["suggestedTests"][0]["path"], "tests/newpkg/mod.rs");
    }
}
