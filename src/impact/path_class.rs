//! Path classification for code-default impact demotion (track 0173).
//!
//! Distinguishes **Code**, **Governance** (process/conductor docs), and
//! **Contract** (product/agent contracts that must never demote) so temporal
//! risk weight and `readSet` p3 can prefer code blast radius over process
//! co-evolution.

use std::path::Path;

/// Classification of a repository-relative path for agent impact budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PathClass {
    /// Source, tests, manifests — primary agent read budget.
    Code,
    /// Process / conductor / deferred docs — demoted under default pathMode=code.
    Governance,
    /// Product/agent contracts — never demoted from risk or readSet.
    Contract,
}

impl PathClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Code => "code",
            Self::Governance => "governance",
            Self::Contract => "contract",
        }
    }
}

/// Normalize path separators and strip a leading `./`.
pub fn normalize_path(path: &str) -> String {
    let mut s = path.replace('\\', "/");
    while s.starts_with("./") {
        s = s[2..].to_string();
    }
    s
}

/// Classify a repository-relative path.
///
/// Evaluation order (hard):
/// 1. Contract allowlist (exact normalized path, plus SKILL.md under `.agents/skills/`)
/// 2. Code-root / source-extension fast path (`src/`, `tests/`, source extensions)
/// 3. Scoped governance matchers
/// 4. Unmatched `.md`/`.txt` → Governance; else Code
pub fn classify_path(path: &str) -> PathClass {
    let norm = normalize_path(path);
    if norm.is_empty() {
        return PathClass::Code;
    }

    if is_contract(&norm) {
        return PathClass::Contract;
    }

    if is_code_root_or_source(&norm) {
        return PathClass::Code;
    }

    if is_governance(&norm) {
        return PathClass::Governance;
    }

    if is_markdown_or_txt(&norm) {
        return PathClass::Governance;
    }

    PathClass::Code
}

/// Classify a [`Path`].
pub fn classify_path_buf(path: &Path) -> PathClass {
    classify_path(&path.to_string_lossy())
}

/// Whether a temporal pair should be demoted under the given path mode.
///
/// Order is load-bearing (0202 F9): `pathMode=all` never; CHANGELOG.md
/// basename demotes; either Contract keeps; strict ancestor-path demotes;
/// either Governance demotes; else keep.
pub fn should_demote_pair(path_a: &str, path_b: &str, path_mode: &str) -> bool {
    // 1. pathMode=all never demotes (including F1/F2).
    if path_mode.eq_ignore_ascii_case("all") {
        return false;
    }
    let na = normalize_path(path_a);
    let nb = normalize_path(path_b);

    // 2. F1: CHANGELOG.md basename (case-insensitive, after normalize).
    if basename_eq(&na, "CHANGELOG.md") || basename_eq(&nb, "CHANGELOG.md") {
        return true;
    }

    // 3. Either Contract → keep (T16 openapi ancestor).
    let a = classify_path(&na);
    let b = classify_path(&nb);
    if a == PathClass::Contract || b == PathClass::Contract {
        return false;
    }

    // 4. F2: strict ancestor-path with slash boundary (T6 packaging parent).
    if is_strict_path_prefix(&na, &nb) {
        return true;
    }

    // 5. Either Governance → demote (T7 rb↔docs after F3).
    // 6. Else keep (T5 siblings, T17 packaging siblings).
    a == PathClass::Governance || b == PathClass::Governance
}

/// Count temporal couplings at/above threshold that would be demoted under `path_mode`.
pub fn count_demoted_temporal(
    couplings: &[crate::impact::packet::TemporalCoupling],
    path_mode: &str,
    threshold: f32,
) -> u32 {
    let mut count = 0u32;
    for tc in couplings {
        if tc.score < threshold {
            continue;
        }
        let a = tc.file_a.to_string_lossy();
        let b = tc.file_b.to_string_lossy();
        if should_demote_pair(&a, &b, path_mode) {
            count = count.saturating_add(1);
        }
    }
    count
}

/// Resolve pathMode string from `--include-governance` flag.
pub fn path_mode_from_include_governance(include_governance: bool) -> &'static str {
    if include_governance { "all" } else { "code" }
}

fn is_contract(norm: &str) -> bool {
    // Exact allowlist (normalized, forward slashes).
    const EXACT: &[&str] = &[
        "docs/agent-output-contract.md",
        "docs/Engineering.md",
        "docs/architecture.md",
        "docs/Features.md",
        "docs/api/openapi.json",
        "docs/Signature-Diff.md",
        "docs/chain-checkpoint.md",
        "docs/reviewer-readonly.md",
        "docs/testing.md",
        "docs/verify-performance.md",
        "docs/team-sync.md",
        "docs/Ledgerful/skill.md",
        "AGENTS.md",
        "Agents.md",
        "Claude.md",
        "CHANGELOG.md",
        "SECURITY.md",
        "Cargo.toml",
        "Cargo.lock",
        "package.json",
        "package-lock.json",
        "pnpm-lock.yaml",
        "yarn.lock",
        "go.mod",
        "go.sum",
        "pyproject.toml",
        "requirements.txt",
        "Pipfile",
        "Pipfile.lock",
    ];

    for exact in EXACT {
        if norm.eq_ignore_ascii_case(exact) {
            return true;
        }
    }

    // SKILL.md under .agents/skills/**
    if basename_eq(norm, "SKILL.md")
        && (norm.starts_with(".agents/skills/") || norm.contains("/.agents/skills/"))
    {
        return true;
    }

    false
}

fn is_code_root_or_source(norm: &str) -> bool {
    if under_prefix(norm, "src/") || under_prefix(norm, "tests/") {
        return true;
    }
    // Bare roots
    if norm == "src" || norm == "tests" {
        return true;
    }
    has_source_extension(norm)
}

fn has_source_extension(norm: &str) -> bool {
    let Some(ext) = extension_of(norm) else {
        return false;
    };
    matches!(
        ext,
        "rs" | "ts"
            | "tsx"
            | "js"
            | "jsx"
            | "mjs"
            | "cjs"
            | "py"
            | "go"
            | "c"
            | "h"
            | "cpp"
            | "cc"
            | "cxx"
            | "hpp"
            | "hh"
            | "hxx"
            | "java"
            | "kt"
            | "kts"
            | "cs"
            | "rb"
            | "php"
            | "swift"
            | "scala"
            | "rs.in"
            | "toml" // build scripts / crate manifests under non-src trees still Code-ish
    )
}

fn is_governance(norm: &str) -> bool {
    // Registry basenames (any directory)
    const GOVERNANCE_BASENAMES: &[&str] = &[
        "deferred.md",
        "conductor.md",
        "coordination.md",
        "sequencing.md",
        "sequencing2.md",
    ];
    for base in GOVERNANCE_BASENAMES {
        if basename_eq(norm, base) {
            return true;
        }
    }

    // Scoped process roots (not under code roots — already handled above)
    if under_prefix(norm, "coordinated/conductor/") || norm == "coordinated/conductor" {
        return true;
    }
    // conductor/** at repo root (not src/conductor — code-root wins first)
    if under_prefix(norm, "conductor/") || norm == "conductor" {
        return true;
    }

    // F3 (0202): docs directory and docs/** unless already Contract / source-ext.
    if norm.eq_ignore_ascii_case("docs") || under_prefix(norm, "docs/") {
        return true;
    }

    // .agents/** process dumps, but SKILL.md already classified as Contract
    if under_prefix(norm, ".agents/") {
        return true;
    }

    false
}

fn is_markdown_or_txt(norm: &str) -> bool {
    matches!(extension_of(norm), Some("md" | "txt"))
}

fn under_prefix(norm: &str, prefix: &str) -> bool {
    norm.starts_with(prefix)
}

/// True when one path is a strict ancestor of the other with a `/` boundary.
/// Trailing slashes are stripped here only (do not change [`normalize_path`]).
/// Does not match `src/foo` vs `src/foobar`.
fn is_strict_path_prefix(a: &str, b: &str) -> bool {
    let a = a.trim_end_matches('/');
    let b = b.trim_end_matches('/');
    if a.is_empty() || b.is_empty() || a == b {
        return false;
    }
    fn ancestor_of(prefix: &str, path: &str) -> bool {
        path.len() > prefix.len()
            && path.starts_with(prefix)
            && path.as_bytes().get(prefix.len()) == Some(&b'/')
    }
    ancestor_of(a, b) || ancestor_of(b, a)
}

fn basename_eq(norm: &str, name: &str) -> bool {
    let base = norm.rsplit('/').next().unwrap_or(norm);
    base.eq_ignore_ascii_case(name)
}

fn extension_of(norm: &str) -> Option<&str> {
    let base = norm.rsplit('/').next().unwrap_or(norm);
    // Handle multi-part like .rs.in lightly: last segment after final '.'
    let dot = base.rfind('.')?;
    if dot == 0 || dot + 1 >= base.len() {
        return None;
    }
    Some(&base[dot + 1..])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::impact::packet::TemporalCoupling;
    use std::path::PathBuf;

    #[test]
    fn path_class_matrix() {
        let cases: &[(&str, PathClass)] = &[
            ("src/lib.rs", PathClass::Code),
            ("src/conductor/mod.rs", PathClass::Code), // 0173-B
            ("tests/integration/foo.rs", PathClass::Code),
            ("conductor/0173-x/spec.md", PathClass::Governance),
            ("coordinated/conductor/deferred.md", PathClass::Governance),
            ("deferred.md", PathClass::Governance),
            ("docs/agent-output-contract.md", PathClass::Contract),
            (".agents/skills/ledgerful/SKILL.md", PathClass::Contract),
            ("docs/Engineering.md", PathClass::Contract),
            ("docs/random-note.md", PathClass::Governance),
            ("Cargo.toml", PathClass::Contract), // never Governance
            ("src\\windows\\path.rs", PathClass::Code),
            ("AGENTS.md", PathClass::Contract),
            ("conductor.md", PathClass::Governance),
            ("package.json", PathClass::Contract),
            ("README.md", PathClass::Governance),
            ("docs/api/openapi.json", PathClass::Contract),
            ("scripts/setup.sh", PathClass::Code), // unmatched non-md → Code
        ];

        for (path, expected) in cases {
            assert_eq!(
                classify_path(path),
                *expected,
                "path {path:?} expected {expected:?}"
            );
        }
    }

    #[test]
    fn should_demote_governance_pairs_under_code_mode() {
        assert!(should_demote_pair(
            "conductor/0173/spec.md",
            "deferred.md",
            "code"
        ));
        assert!(should_demote_pair(
            "src/foo.rs",
            "conductor/0173/spec.md",
            "code"
        ));
        assert!(!should_demote_pair("src/a.rs", "src/b.rs", "code"));
        assert!(!should_demote_pair(
            "src/a.rs",
            "docs/agent-output-contract.md",
            "code"
        ));
        assert!(!should_demote_pair(
            "src/foo.rs",
            "conductor/0173/spec.md",
            "all"
        ));
    }

    #[test]
    fn count_demoted_temporal_pure_governance() {
        let couplings = vec![
            TemporalCoupling {
                file_a: PathBuf::from("conductor/a.md"),
                file_b: PathBuf::from("deferred.md"),
                score: 0.9,
            },
            TemporalCoupling {
                file_a: PathBuf::from("src/a.rs"),
                file_b: PathBuf::from("src/b.rs"),
                score: 0.95,
            },
            TemporalCoupling {
                file_a: PathBuf::from("src/a.rs"),
                file_b: PathBuf::from("conductor/x.md"),
                score: 0.8,
            },
            TemporalCoupling {
                file_a: PathBuf::from("src/a.rs"),
                file_b: PathBuf::from("src/c.rs"),
                score: 0.5, // below threshold
            },
        ];
        assert_eq!(count_demoted_temporal(&couplings, "code", 0.7), 2);
        assert_eq!(count_demoted_temporal(&couplings, "all", 0.7), 0);
    }

    #[test]
    fn path_mode_from_flag() {
        assert_eq!(path_mode_from_include_governance(false), "code");
        assert_eq!(path_mode_from_include_governance(true), "all");
    }

    #[test]
    fn changelog_pairs_demote_under_code_mode() {
        // T9 CHANGELOG ↔ docs, T10 CHANGELOG ↔ skill.md (F1 before Contract),
        // T11 CHANGELOG ↔ src.
        assert!(should_demote_pair(
            "CHANGELOG.md",
            "docs/Call-Resolution.md",
            "code"
        ));
        assert!(should_demote_pair(
            "CHANGELOG.md",
            "docs/Ledgerful/skill.md",
            "code"
        ));
        assert!(should_demote_pair(
            "CHANGELOG.md",
            "src/commands/doctor.rs",
            "code"
        ));
        assert!(!should_demote_pair(
            "CHANGELOG.md",
            "docs/Ledgerful/skill.md",
            "all"
        ));
    }

    #[test]
    fn changelog_still_classifies_as_contract() {
        // T12: CHANGELOG stays Contract (class counts / p1-when-changed).
        assert_eq!(classify_path("CHANGELOG.md"), PathClass::Contract);
        assert_eq!(classify_path("changelog.md"), PathClass::Contract);
    }

    #[test]
    fn docs_dir_and_docs_ledgerful_are_governance() {
        // T13
        assert_eq!(classify_path("docs"), PathClass::Governance);
        assert_eq!(classify_path("docs/"), PathClass::Governance);
        assert_eq!(classify_path("docs/Ledgerful"), PathClass::Governance);
        // Source-ext under docs/ stays Code (contract → code-root → F3).
        assert_eq!(classify_path("docs/foo.rs"), PathClass::Code);
        // T8: md under docs already Governance; dir is now Governance too.
        assert!(should_demote_pair("docs/installation.md", "docs", "code"));
    }

    #[test]
    fn skill_md_stays_contract() {
        assert_eq!(
            classify_path("docs/Ledgerful/skill.md"),
            PathClass::Contract
        );
        // Pair with CHANGELOG still DEM via F1 (before Contract guard).
        assert!(should_demote_pair(
            "CHANGELOG.md",
            "docs/Ledgerful/skill.md",
            "code"
        ));
    }

    #[test]
    fn ancestor_path_demotes_packaging_parent() {
        // T6
        assert!(should_demote_pair(
            "packaging/homebrew/ledgerful.rb",
            "packaging",
            "code"
        ));
        assert!(should_demote_pair(
            "packaging/homebrew/ledgerful.rb",
            "packaging/homebrew",
            "code"
        ));
    }

    #[test]
    fn slash_boundary_does_not_match_foobar() {
        assert!(!is_strict_path_prefix("src/foo", "src/foobar"));
        assert!(is_strict_path_prefix("docs", "docs/installation.md"));
        assert!(is_strict_path_prefix("docs/", "docs/foo"));
        assert!(is_strict_path_prefix(
            "packaging",
            "packaging/homebrew/ledgerful.rb"
        ));
        assert!(!should_demote_pair("src/foo", "src/foobar", "code"));
    }

    #[test]
    fn rb_vs_docs_dir_demotes_after_f3() {
        // T7: packaging rb ↔ docs dir (docs was Code; F3 makes it Governance).
        assert!(should_demote_pair(
            "packaging/homebrew/ledgerful.rb",
            "docs",
            "code"
        ));
    }

    #[test]
    fn docs_dir_vs_openapi_stays_keep() {
        // T16: F2 after Contract — ancestor + Contract keeps.
        assert!(!should_demote_pair("docs", "docs/api/openapi.json", "code"));
        assert_eq!(classify_path("docs/api/openapi.json"), PathClass::Contract);
    }

    #[test]
    fn packaging_siblings_stay_keep() {
        // T17: F2 slash-prefix does not treat packaging siblings as ancestors.
        assert!(!should_demote_pair(
            "packaging/homebrew/ledgerful.rb",
            "packaging/scoop/ledgerful.json",
            "code"
        ));
        assert!(!is_strict_path_prefix(
            "packaging/homebrew/ledgerful.rb",
            "packaging/scoop/ledgerful.json"
        ));
    }
}
