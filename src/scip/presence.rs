//! SCIP product-path presence (0448).
//!
//! Language bits and doctor SCIP findings. CLI orchestration stays in
//! `crate::commands::doctor`.

use crate::doctor::finding::{DoctorCategory, DoctorFinding};
use crate::scip::ScipToolchain;
use camino::Utf8Path;

/// Product-path language bits for doctor SCIP emission (0296).
///
/// Presence is **tracked git index** when readable, else a gitignore-respecting
/// walk. Untracked product files are silent until `git add`. A directory
/// component **exactly** named `test`/`tests`/`vendor`/… is invisible to SCIP
/// hints (fixtures and vendored sqlite on this engine). Do **not** reuse
/// `is_test_path` / `is_test_or_example_path`: those omit vendor/deps_src
/// and the latter is `.rs`-stem-coupled.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ScipLangs {
    rust: bool,
    typescript: bool,
    python: bool,
    go: bool,
    cpp: bool,
}

const SCIP_SKIP_SEGS: &[&str] = &[
    "vendor",
    "deps_src",
    "node_modules",
    "target",
    "third_party",
    "tests",
    "test",
    "examples",
    "benches",
];

fn normalize_rel_path(rel: &str) -> String {
    rel.replace('\\', "/")
}

pub(crate) fn skip_scip_rel_path(rel: &str) -> bool {
    normalize_rel_path(rel)
        .to_ascii_lowercase()
        .split('/')
        .filter(|s| !s.is_empty())
        .any(|seg| SCIP_SKIP_SEGS.contains(&seg))
}

/// Classify relative paths into SCIP languages. Slash-normalize first so
/// Windows `tests\fixtures\foo.go` still skips `tests`.
pub(crate) fn scip_langs_from_rel_paths<'a>(paths: impl Iterator<Item = &'a str>) -> ScipLangs {
    let mut langs = ScipLangs::default();
    for raw in paths {
        if skip_scip_rel_path(raw) {
            continue;
        }
        let n = normalize_rel_path(raw);
        let basename = n.rsplit('/').next().unwrap_or(n.as_str());
        let ext = match basename.rsplit_once('.') {
            Some((_, e)) => e.to_ascii_lowercase(),
            None => continue,
        };
        match ext.as_str() {
            "rs" => langs.rust = true,
            "ts" | "tsx" => langs.typescript = true,
            "py" | "pyi" => langs.python = true,
            "go" => langs.go = true,
            "c" | "h" | "cc" | "cpp" | "cxx" | "hpp" | "hxx" | "hh" => langs.cpp = true,
            _ => {}
        }
    }
    langs
}

fn paths_from_gix_index(index: &gix::worktree::Index) -> Vec<String> {
    let mut out = Vec::new();
    for entry in index.entries() {
        let path = entry.path(index);
        if let Ok(s) = std::str::from_utf8(path.as_ref()) {
            out.push(normalize_rel_path(s));
        }
    }
    out.sort();
    out
}

fn paths_from_walk(work_root: &Utf8Path) -> Option<Vec<String>> {
    let root = work_root.as_std_path();
    let walker = ignore::WalkBuilder::new(root)
        .hidden(true)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .build();
    let mut out = Vec::new();
    for entry in walker {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                tracing::debug!("doctor SCIP walk entry failed: {e}");
                continue;
            }
        };
        if !entry.file_type().is_some_and(|ft| ft.is_file()) {
            continue;
        }
        let Ok(rel) = entry.path().strip_prefix(root) else {
            continue;
        };
        let Some(s) = rel.to_str() else {
            continue;
        };
        out.push(normalize_rel_path(s));
    }
    out.sort();
    Some(out)
}

pub(crate) fn list_scip_rel_paths(work_root: &Utf8Path) -> Option<Vec<String>> {
    match crate::git::repo::open_repo(work_root.as_std_path()) {
        Ok(repo) => match repo.index() {
            Ok(index) => Some(paths_from_gix_index(&index)),
            Err(e) => {
                tracing::debug!("doctor SCIP gix index failed ({e}); falling back to walk");
                paths_from_walk(work_root)
            }
        },
        Err(_) => paths_from_walk(work_root),
    }
}

fn toolchain_present(tool: ScipToolchain, langs: ScipLangs) -> bool {
    match tool {
        ScipToolchain::RustAnalyzer => langs.rust,
        ScipToolchain::ScipTypescript => langs.typescript,
        ScipToolchain::ScipPython => langs.python,
    }
}

/// Per-language SCIP capability + process-policy report for doctor (0095/0109/0296).
///
/// Structured findings with `scip-*` codes; severity Info, category Optional.
/// Emitted only when that **product** language is present (tracked git index
/// when readable; WalkBuilder only if the index cannot be read). Never blocks
/// publish readiness or dashboard failures.
pub(crate) fn collect_scip_findings(
    config: &crate::config::model::Config,
    work_root: &Utf8Path,
) -> Vec<DoctorFinding> {
    use crate::platform::process_policy::check_policy;

    let Some(paths) = list_scip_rel_paths(work_root) else {
        tracing::debug!("doctor SCIP presence listing failed; emitting no SCIP findings");
        return Vec::new();
    };
    let langs = scip_langs_from_rel_paths(paths.iter().map(String::as_str));

    let policy = config.verify.effective_process_policy();
    let mut findings = Vec::new();
    for (tool, available) in ScipToolchain::probe_all_languages() {
        if !toolchain_present(tool, langs) {
            continue;
        }
        let lang = tool.language_label().to_ascii_lowercase();
        if available {
            match check_policy(tool.exe_name(), &policy) {
                Ok(()) => findings.push(DoctorFinding::info(
                    format!("scip-{lang}-available"),
                    DoctorCategory::Optional,
                    format!(
                        "SCIP {}: {} available — `ledgerful index --auto-scip` can add reference edges on native symbols",
                        tool.language_label(),
                        tool.exe_name()
                    ),
                )),
                Err(e) => findings.push(DoctorFinding::info(
                    format!("scip-{lang}-policy-blocked"),
                    DoctorCategory::Optional,
                    format!(
                        "SCIP {}: {} present but blocked by process policy ({e}) — adjust verify.allowed_commands / denied_commands or install is not enough for --auto-scip",
                        tool.language_label(),
                        tool.exe_name()
                    ),
                )),
            }
        } else {
            findings.push(DoctorFinding::info(
                format!("scip-{lang}-missing"),
                DoctorCategory::Optional,
                format!(
                    "SCIP {}: {} not available (capability probe). Install with `{}` to enable cross-file references via --auto-scip",
                    tool.language_label(),
                    tool.exe_name(),
                    tool.install_hint()
                ),
            ));
        }
    }
    if langs.go {
        findings.push(DoctorFinding::info(
            "scip-go-not-wired",
            DoctorCategory::Optional,
            "SCIP Go: upstream scip-go exists, not wired here — native Go tree-sitter path only",
        ));
    }
    if langs.cpp {
        findings.push(DoctorFinding::info(
            "scip-clang-not-wired",
            DoctorCategory::Optional,
            "SCIP C/C++: upstream scip-clang exists (not wired; no Windows binary; needs compile_commands.json) — native C/C++ tree-sitter path only. Manual: run scip-clang externally then `ledgerful index --scip path/to/index.scip`",
        ));
    }
    findings.sort_by(|a, b| a.code.cmp(&b.code).then(a.message.cmp(&b.message)));
    findings
}

#[cfg(test)]
mod scip_presence_tests {
    use super::{ScipLangs, scip_langs_from_rel_paths};

    #[test]
    fn scip_langs_from_rel_paths_skips_tests_and_vendor() {
        let rust = scip_langs_from_rel_paths(["src/lib.rs"].into_iter());
        assert_eq!(
            rust,
            ScipLangs {
                rust: true,
                ..ScipLangs::default()
            }
        );

        let go = scip_langs_from_rel_paths(["src/main.go"].into_iter());
        assert!(go.go && !go.rust);

        let ts = scip_langs_from_rel_paths(["src/index.ts", "src/app.tsx"].into_iter());
        assert!(ts.typescript && !ts.python);

        let cpp =
            scip_langs_from_rel_paths(["src/native.c", "src/lib.cpp", "src/lib.h"].into_iter());
        assert!(cpp.cpp && !cpp.go);

        let skip_go = scip_langs_from_rel_paths(["tests/fixtures/foo.go"].into_iter());
        assert_eq!(skip_go, ScipLangs::default());

        let skip_win_go = scip_langs_from_rel_paths([r"tests\fixtures\foo.go"].into_iter());
        assert_eq!(skip_win_go, ScipLangs::default());

        let skip_vendor = scip_langs_from_rel_paths(["vendor/sqlite3-src/sqlite3.c"].into_iter());
        assert_eq!(skip_vendor, ScipLangs::default());

        let skip_win_vendor =
            scip_langs_from_rel_paths([r"vendor\sqlite3-src\sqlite3.c"].into_iter());
        assert_eq!(skip_win_vendor, ScipLangs::default());

        let test_utils = scip_langs_from_rel_paths(["src/test_utils/foo.go"].into_iter());
        assert!(test_utils.go, "exact skip-component, not starts_with(test)");

        let mix = scip_langs_from_rel_paths(["src/app.py", "src/lib.rs"].into_iter());
        assert!(mix.rust && mix.python && !mix.go);
    }
}
