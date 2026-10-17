//! Engine binary currency vs worktree (0137 / 0513).
//!
//! When doctor runs inside the Ledgerful engine checkout, compare the
//! executing binary with this worktree. Recommend `cargo install --path .`
//! only when the binary is behind that tree. Consumer repos stay silent.
//! Runtime HEAD is via gix only — no `Command::new("git")` at doctor runtime.

use super::finding::{DoctorCategory, DoctorFinding};
use crate::commands::release::pins::parse_ver;
use crate::git::repo::{
    CommitAncestry, commit_ancestry, sha_prefix_equal, shorten_sha_for_display,
};
use crate::platform::is_ledgerful_engine_worktree;
use crate::platform::worktree_package_version;
use std::cmp::Ordering;
use std::path::Path;

/// Stable finding code (greppable; distinct from `tool-git`).
pub const BINARY_BEHIND_TREE_CODE: &str = "binary-behind-tree";
pub const BINARY_AHEAD_OF_TREE_CODE: &str = "binary-ahead-of-tree";
pub const BINARY_DIVERGES_FROM_TREE_CODE: &str = "binary-diverges-from-tree";

/// Worktree install, then a version check. No `update --binary` and no Latest URL.
pub const BINARY_BEHIND_TREE_REMEDIATION: &str = "\
cargo install --path . --force
ledgerful --version";

/// Ahead and diverge name the running binary. They do not install this tree.
pub const BINARY_TREE_VERSION_REMEDIATION: &str = "ledgerful --version";

/// Which tree finding to emit. Semver behind and ahead do not consult ancestry.
///
/// The `Tree` suffix is the spec name for each direction. Clippy's shared-postfix
/// lint would rename them away from that contract.
#[allow(clippy::enum_variant_names)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryCurrencyDirection {
    BehindTree,
    AheadOfTree,
    DivergesFromTree,
}

/// How the two version strings compared. A parse failure is not "behind".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum VersionShape {
    TriplesDiffer,
    Equal,
    Unparsed,
}

/// One currency finding's facts. Unit tests inject ancestry and do not open a repo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BinaryCurrencyLag {
    pub direction: BinaryCurrencyDirection,
    pub shas_usable: bool,
    pub running_version: String,
    pub worktree_version: String,
    pub running_sha: String,
    pub worktree_head_sha: String,
    pub ancestry: CommitAncestry,
    pub(crate) version_shape: VersionShape,
}

/// Pure classifier with injected facts (unit-testable).
///
/// Returns [`Some`] when an engine worktree should emit one tree finding.
/// Semver [`Ordering::Less`] and [`Ordering::Greater`] ignore `ancestry`.
pub fn classify_binary_currency(
    running_version: &str,
    worktree_version: Option<&str>,
    running_sha: &str,
    worktree_head_sha: Option<&str>,
    is_engine: bool,
    ancestry: CommitAncestry,
) -> Option<BinaryCurrencyLag> {
    if !is_engine {
        return None;
    }

    let run_ver = running_version.trim();
    let wt_raw = worktree_version.map(str::trim).unwrap_or("");
    let run_sha = running_sha.trim();
    let head_raw = worktree_head_sha.map(str::trim).unwrap_or("");
    let usable = sha_token_usable(run_sha) && sha_token_usable(head_raw);
    let prefix_equal = usable && sha_prefix_equal(run_sha, head_raw);

    let (shape, ordering) = match version_shape(run_ver, wt_raw) {
        VersionCompare::SameRaw => return None,
        VersionCompare::Compared { shape, ordering } => (shape, ordering),
    };

    let direction = direction_for(shape, ordering, usable, prefix_equal, ancestry)?;
    let stored_ancestry = if shape == VersionShape::TriplesDiffer {
        CommitAncestry::NotConsulted
    } else {
        ancestry
    };

    Some(BinaryCurrencyLag {
        direction,
        shas_usable: usable,
        running_version: run_ver.to_string(),
        worktree_version: wt_raw.to_string(),
        running_sha: display_sha(run_sha),
        worktree_head_sha: display_sha(head_raw),
        ancestry: stored_ancestry,
        version_shape: shape,
    })
}

enum VersionCompare {
    SameRaw,
    Compared {
        shape: VersionShape,
        ordering: Ordering,
    },
}

fn version_shape(running: &str, worktree: &str) -> VersionCompare {
    match (parse_ver(running), parse_ver(worktree)) {
        (Some(run), Some(tree)) => {
            let ordering = run.cmp(&tree);
            let shape = if ordering == Ordering::Equal {
                VersionShape::Equal
            } else {
                VersionShape::TriplesDiffer
            };
            VersionCompare::Compared { shape, ordering }
        }
        _ if running == worktree => VersionCompare::SameRaw,
        _ => VersionCompare::Compared {
            shape: VersionShape::Unparsed,
            ordering: Ordering::Equal,
        },
    }
}

fn direction_for(
    shape: VersionShape,
    ordering: Ordering,
    usable: bool,
    prefix_equal: bool,
    ancestry: CommitAncestry,
) -> Option<BinaryCurrencyDirection> {
    if shape == VersionShape::TriplesDiffer {
        return Some(if ordering == Ordering::Less {
            BinaryCurrencyDirection::BehindTree
        } else {
            BinaryCurrencyDirection::AheadOfTree
        });
    }

    if shape == VersionShape::Equal {
        if prefix_equal || !usable {
            return None;
        }
        return match ancestry {
            CommitAncestry::SameCommit => None,
            CommitAncestry::RunningIsAncestor => Some(BinaryCurrencyDirection::BehindTree),
            CommitAncestry::HeadIsAncestor => Some(BinaryCurrencyDirection::AheadOfTree),
            CommitAncestry::Divergent | CommitAncestry::Unknown => {
                Some(BinaryCurrencyDirection::DivergesFromTree)
            }
            CommitAncestry::NotConsulted => None,
        };
    }

    if usable && prefix_equal {
        return None;
    }
    if usable {
        return match ancestry {
            CommitAncestry::SameCommit => None,
            CommitAncestry::RunningIsAncestor => Some(BinaryCurrencyDirection::BehindTree),
            CommitAncestry::HeadIsAncestor => Some(BinaryCurrencyDirection::AheadOfTree),
            CommitAncestry::Divergent | CommitAncestry::Unknown => {
                Some(BinaryCurrencyDirection::DivergesFromTree)
            }
            CommitAncestry::NotConsulted => None,
        };
    }
    Some(BinaryCurrencyDirection::DivergesFromTree)
}

fn sha_token_usable(raw: &str) -> bool {
    !raw.is_empty() && !raw.eq_ignore_ascii_case("unknown")
}

fn display_sha(raw: &str) -> String {
    if sha_token_usable(raw) {
        shorten_sha_for_display(raw)
    } else {
        String::new()
    }
}

fn needs_ancestry(
    running_version: &str,
    worktree_version: Option<&str>,
    running_sha: &str,
    worktree_head_sha: Option<&str>,
) -> bool {
    let run_ver = running_version.trim();
    let wt_raw = worktree_version.map(str::trim).unwrap_or("");
    let run_sha = running_sha.trim();
    let head_raw = worktree_head_sha.map(str::trim).unwrap_or("");
    let usable = sha_token_usable(run_sha) && sha_token_usable(head_raw);
    if !usable || sha_prefix_equal(run_sha, head_raw) {
        return false;
    }
    matches!(
        version_shape(run_ver, wt_raw),
        VersionCompare::Compared {
            shape: VersionShape::Equal | VersionShape::Unparsed,
            ..
        }
    )
}

/// Per-arm message. `Divergent` does not say ancestry is unknown.
pub fn compose_binary_currency_message(lag: &BinaryCurrencyLag) -> String {
    let run = &lag.running_version;
    let tree = &lag.worktree_version;
    let sha = &lag.running_sha;
    let head = &lag.worktree_head_sha;
    match (lag.direction, lag.version_shape, lag.ancestry, lag.shas_usable) {
        (BinaryCurrencyDirection::BehindTree, VersionShape::TriplesDiffer, _, true) => format!(
            "Installed ledgerful binary is behind this engine worktree: running {run} vs worktree {tree}. Running commit {sha} vs worktree HEAD {head}."
        ),
        (BinaryCurrencyDirection::BehindTree, VersionShape::TriplesDiffer, _, false) => format!(
            "Installed ledgerful binary is behind this engine worktree: running {run} vs worktree {tree}."
        ),
        (BinaryCurrencyDirection::BehindTree, VersionShape::Equal, _, _) => format!(
            "Installed ledgerful binary is behind this engine worktree: same version {run}; running commit {sha} is an ancestor of worktree HEAD {head}."
        ),
        (BinaryCurrencyDirection::BehindTree, VersionShape::Unparsed, _, _) => format!(
            "Installed ledgerful binary is behind this engine worktree: version was not comparable; running commit {sha} is an ancestor of worktree HEAD {head}."
        ),
        (BinaryCurrencyDirection::AheadOfTree, VersionShape::TriplesDiffer, _, true) => format!(
            "Installed ledgerful binary is newer than this engine worktree: running {run} vs worktree {tree}. Running commit {sha} vs worktree HEAD {head}. Do not install this worktree over it."
        ),
        (BinaryCurrencyDirection::AheadOfTree, VersionShape::TriplesDiffer, _, false) => format!(
            "Installed ledgerful binary is newer than this engine worktree: running {run} vs worktree {tree}. Do not install this worktree over it."
        ),
        (BinaryCurrencyDirection::AheadOfTree, VersionShape::Equal, _, _) => format!(
            "Installed ledgerful binary is newer than this engine worktree: same version {run}; worktree HEAD {head} is an ancestor of running commit {sha}. Do not install this worktree over it."
        ),
        (BinaryCurrencyDirection::AheadOfTree, VersionShape::Unparsed, _, _) => format!(
            "Installed ledgerful binary is newer than this engine worktree: version was not comparable; worktree HEAD {head} is an ancestor of running commit {sha}. Do not install this worktree over it."
        ),
        (
            BinaryCurrencyDirection::DivergesFromTree,
            VersionShape::Equal,
            CommitAncestry::Divergent,
            _,
        ) => format!(
            "Installed ledgerful binary diverges from this engine worktree: same version {run}; running commit {sha} and worktree HEAD {head} have diverged."
        ),
        (BinaryCurrencyDirection::DivergesFromTree, VersionShape::Equal, _, _) => format!(
            "Installed ledgerful binary diverges from this engine worktree: same version {run}; commit ancestry is unknown. Running commit {sha} vs worktree HEAD {head}."
        ),
        (
            BinaryCurrencyDirection::DivergesFromTree,
            VersionShape::Unparsed | VersionShape::TriplesDiffer,
            CommitAncestry::Divergent,
            _,
        ) => format!(
            "Installed ledgerful binary diverges from this engine worktree: version was not comparable; running commit {sha} and worktree HEAD {head} have diverged."
        ),
        (
            BinaryCurrencyDirection::DivergesFromTree,
            VersionShape::Unparsed | VersionShape::TriplesDiffer,
            _,
            _,
        ) => "Installed ledgerful binary diverges from this engine worktree: version was not comparable and commit ancestry is unknown.".to_string(),
    }
}

/// Build the single tree-currency finding.
pub fn build_binary_currency_finding(lag: &BinaryCurrencyLag) -> DoctorFinding {
    let message = compose_binary_currency_message(lag);
    match lag.direction {
        BinaryCurrencyDirection::BehindTree => {
            DoctorFinding::warn(BINARY_BEHIND_TREE_CODE, DoctorCategory::Tools, message)
                .with_remediation(BINARY_BEHIND_TREE_REMEDIATION)
        }
        BinaryCurrencyDirection::AheadOfTree => {
            DoctorFinding::info(BINARY_AHEAD_OF_TREE_CODE, DoctorCategory::Tools, message)
                .with_remediation(BINARY_TREE_VERSION_REMEDIATION)
        }
        BinaryCurrencyDirection::DivergesFromTree => DoctorFinding::info(
            BINARY_DIVERGES_FROM_TREE_CODE,
            DoctorCategory::Tools,
            message,
        )
        .with_remediation(BINARY_TREE_VERSION_REMEDIATION),
    }
}

/// Build the `binary-behind-tree` finding. Callers that already classified a
/// behind lag keep this name. The builder still follows `lag.direction`.
pub fn build_binary_behind_tree_finding(lag: &BinaryCurrencyLag) -> DoctorFinding {
    build_binary_currency_finding(lag)
}

/// Probe: resolve engine root, gather facts, return one finding when currency differs.
///
/// Prefer `layout_root` when it carries engine markers; else `current_dir`.
/// Runtime HEAD via gix only (no git subprocess). Ancestry is consulted only
/// for an equal or unparsed version whose SHAs are usable and not prefix-equal.
pub fn probe_binary_currency(
    layout_root: &Path,
    current_dir: &Path,
    running_version: &str,
    running_sha: &str,
) -> Option<DoctorFinding> {
    let engine_root = if is_ledgerful_engine_worktree(layout_root) {
        layout_root
    } else if is_ledgerful_engine_worktree(current_dir) {
        current_dir
    } else {
        return None;
    };

    let worktree_version = worktree_package_version(engine_root);
    let repo = crate::git::repo::open_repo(engine_root).ok();
    let worktree_head = repo
        .as_ref()
        .and_then(|opened| crate::git::repo::get_head_info(opened).ok())
        .and_then(|(hash, _)| hash);

    let ancestry = if needs_ancestry(
        running_version,
        worktree_version.as_deref(),
        running_sha,
        worktree_head.as_deref(),
    ) {
        match repo.as_ref() {
            Some(opened) => {
                commit_ancestry(opened, running_sha, worktree_head.as_deref().unwrap_or(""))
            }
            None => CommitAncestry::Unknown,
        }
    } else {
        CommitAncestry::NotConsulted
    };

    let lag = classify_binary_currency(
        running_version,
        worktree_version.as_deref(),
        running_sha,
        worktree_head.as_deref(),
        true,
        ancestry,
    )?;
    Some(build_binary_currency_finding(&lag))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::doctor::finding::{
        DoctorSeverity, SessionPriority, dashboard_failures, ready_for_publish,
    };
    use crate::platform::is_ledgerful_engine_worktree;
    use crate::platform::worktree_package_version;
    use std::fs;
    use tempfile::tempdir;

    fn classify(
        running: &str,
        worktree: Option<&str>,
        run_sha: &str,
        head: Option<&str>,
        engine: bool,
        ancestry: CommitAncestry,
    ) -> Option<BinaryCurrencyLag> {
        classify_binary_currency(running, worktree, run_sha, head, engine, ancestry)
    }

    /// Pure fail-soft mapping of `git rev-parse --short=12 HEAD` → embed token.
    /// Mirrors root `build.rs` (keep in sync): non-success / empty → `"unknown"`.
    fn embed_sha_from_rev_parse(success: bool, stdout: &str) -> String {
        if !success {
            return "unknown".to_string();
        }
        let s = stdout.trim();
        if s.is_empty() {
            "unknown".to_string()
        } else {
            s.to_string()
        }
    }

    /// Pure long version string for clap `long_version` (mirrors `build.rs`).
    fn compose_version_long(pkg_version: &str, sha: &str) -> String {
        if sha != "unknown" && !sha.is_empty() {
            format!("{pkg_version} ({sha})")
        } else {
            pkg_version.to_string()
        }
    }

    #[test]
    fn dogfood_same_version_different_sha_finds_commit_only() {
        let lag = classify(
            "0.2.5",
            Some("0.2.5"),
            "aaaaaaaaaaaa",
            Some("bbbbbbbbbbbb"),
            true,
            CommitAncestry::Unknown,
        )
        .expect("unrelated SHAs without ancestor proof diverge");
        assert_eq!(lag.direction, BinaryCurrencyDirection::DivergesFromTree);
        let f = build_binary_currency_finding(&lag);
        assert_eq!(f.code, BINARY_DIVERGES_FROM_TREE_CODE);
        assert_eq!(f.severity, DoctorSeverity::Info);
        assert_eq!(f.category, DoctorCategory::Tools);
        assert!(
            f.message.contains("same version 0.2.5"),
            "must name same version: {}",
            f.message
        );
        assert!(f.message.contains("ancestry is unknown"), "{}", f.message);
        assert!(!f.message.contains("behind"), "{}", f.message);
        assert!(
            !f.remediation
                .as_deref()
                .unwrap_or("")
                .contains("cargo install"),
            "{:?}",
            f.remediation
        );
        assert!(ready_for_publish(std::slice::from_ref(&f)));
        assert_eq!(dashboard_failures(std::slice::from_ref(&f)), 0);
    }

    #[test]
    fn version_only_message_has_no_fake_sha() {
        let lag = classify(
            "0.2.4",
            Some("0.2.5"),
            "unknown",
            Some("bbbbbbbbbbbb"),
            true,
            CommitAncestry::RunningIsAncestor,
        )
        .expect("version lag");
        assert_eq!(lag.direction, BinaryCurrencyDirection::BehindTree);
        assert!(!lag.shas_usable);
        let msg = compose_binary_currency_message(&lag);
        assert!(msg.contains("running 0.2.4 vs worktree 0.2.5"));
        assert!(!msg.contains("unknown"));
        assert!(!msg.contains("bbbbbbbbbbbb"));
        assert!(!msg.contains("commit"));
    }

    #[test]
    fn sha_only_and_both_are_single_finding() {
        let sha_only = classify(
            "0.2.5",
            Some("0.2.5"),
            "111111111111",
            Some("222222222222"),
            true,
            CommitAncestry::Unknown,
        )
        .expect("sha only");
        assert_eq!(
            sha_only.direction,
            BinaryCurrencyDirection::DivergesFromTree
        );
        let sha_finding = build_binary_currency_finding(&sha_only);
        assert_eq!(sha_finding.code, BINARY_DIVERGES_FROM_TREE_CODE);

        let both = classify(
            "0.2.4",
            Some("0.2.5"),
            "111111111111",
            Some("222222222222"),
            true,
            CommitAncestry::HeadIsAncestor,
        )
        .expect("both");
        assert_eq!(both.direction, BinaryCurrencyDirection::BehindTree);
        assert!(both.shas_usable);
        let msg = compose_binary_currency_message(&both);
        assert!(msg.contains("0.2.4") && msg.contains("0.2.5"));
        assert!(msg.contains("111111111111") && msg.contains("222222222222"));
        let f = build_binary_behind_tree_finding(&both);
        assert_eq!(f.code, BINARY_BEHIND_TREE_CODE);
    }

    #[test]
    fn non_engine_gate_returns_none() {
        assert!(
            classify(
                "0.2.4",
                Some("0.2.5"),
                "aaaaaaaaaaaa",
                Some("bbbbbbbbbbbb"),
                false,
                CommitAncestry::NotConsulted,
            )
            .is_none()
        );
    }

    #[test]
    fn non_engine_currency_stays_silent() {
        let cases = [
            ("0.2.16", Some("0.2.17"), CommitAncestry::NotConsulted),
            ("0.2.17", Some("0.2.16"), CommitAncestry::RunningIsAncestor),
            ("0.2.17", Some("0.2.17"), CommitAncestry::Divergent),
            ("not-a-version", Some("also-not"), CommitAncestry::Unknown),
        ];
        for (running, worktree, ancestry) in cases {
            let lag = classify(
                running,
                worktree,
                "aaaaaaaaaaaa",
                Some("bbbbbbbbbbbb"),
                false,
                ancestry,
            );
            assert!(
                lag.is_none(),
                "non-engine must not emit {BINARY_BEHIND_TREE_CODE}, {BINARY_AHEAD_OF_TREE_CODE}, or {BINARY_DIVERGES_FROM_TREE_CODE} for {running} / {ancestry:?}"
            );
        }
    }

    #[test]
    fn unknown_sha_equal_version_no_finding() {
        assert!(
            classify(
                "0.2.5",
                Some("0.2.5"),
                "unknown",
                Some("bbbbbbbbbbbb"),
                true,
                CommitAncestry::Unknown,
            )
            .is_none()
        );
        assert!(
            classify(
                "0.2.5",
                Some("0.2.5"),
                "",
                Some("bbbbbbbbbbbb"),
                true,
                CommitAncestry::Unknown,
            )
            .is_none()
        );
    }

    #[test]
    fn prefix_sha_equality_no_finding() {
        assert!(
            classify(
                "0.2.5",
                Some("0.2.5"),
                "b57f447",
                Some("b57f4472efb3"),
                true,
                CommitAncestry::Divergent,
            )
            .is_none()
        );
        assert!(
            classify(
                "0.2.5",
                Some("0.2.5"),
                "b57f4472efb3",
                Some("b57f447"),
                true,
                CommitAncestry::Divergent,
            )
            .is_none()
        );
        assert!(sha_prefix_equal("B57F4472EFB3", "b57f4472efb3"));
    }

    #[test]
    fn equal_version_and_sha_no_finding() {
        assert!(
            classify(
                "0.2.5",
                Some("0.2.5"),
                "aaaaaaaaaaaa",
                Some("aaaaaaaaaaaa"),
                true,
                CommitAncestry::NotConsulted,
            )
            .is_none()
        );
    }

    #[test]
    fn remediation_has_force_and_update_binary() {
        let lag = classify(
            "0.2.16",
            Some("0.2.17"),
            "aaaaaaaaaaaa",
            Some("bbbbbbbbbbbb"),
            true,
            CommitAncestry::HeadIsAncestor,
        )
        .expect("older semver is behind even if ancestry says otherwise");
        let f = build_binary_behind_tree_finding(&lag);
        let rem = f.remediation.as_deref().expect("remediation");
        assert_eq!(rem, BINARY_BEHIND_TREE_REMEDIATION);
        assert!(rem.contains("cargo install --path . --force"), "{rem}");
        assert!(!rem.contains("ledgerful update --binary"), "{rem}");
        assert!(rem.contains("ledgerful --version"));
        let first = rem.lines().next().unwrap_or("");
        assert_eq!(first, "cargo install --path . --force", "{rem}");
    }

    #[test]
    fn installed_newer_semver_does_not_recommend_downgrade() {
        let lag = classify(
            "0.2.17",
            Some("0.2.16"),
            "aaaaaaaaaaaa",
            Some("bbbbbbbbbbbb"),
            true,
            CommitAncestry::NotConsulted,
        )
        .expect("newer");
        let f = build_binary_currency_finding(&lag);
        assert_eq!(f.code, BINARY_AHEAD_OF_TREE_CODE);
        assert_eq!(
            f.message,
            "Installed ledgerful binary is newer than this engine worktree: running 0.2.17 vs worktree 0.2.16. Running commit aaaaaaaaaaaa vs worktree HEAD bbbbbbbbbbbb. Do not install this worktree over it."
        );
        let commit_at = f.message.find("Running commit").expect("sha sentence");
        let prohibit_at = f.message.find("Do not install").expect("prohibition");
        assert!(commit_at < prohibit_at);
        let rem = f.remediation.as_deref().unwrap_or("");
        assert_eq!(rem, "ledgerful --version");
        assert!(!f.message.contains("cargo install"));
        assert!(!rem.contains("cargo install"));
    }

    #[test]
    fn installed_older_semver_recommends_worktree_install() {
        let lag = classify(
            "0.2.16",
            Some("0.2.17"),
            "aaaaaaaaaaaa",
            Some("bbbbbbbbbbbb"),
            true,
            CommitAncestry::NotConsulted,
        )
        .expect("older");
        let f = build_binary_currency_finding(&lag);
        assert_eq!(f.code, BINARY_BEHIND_TREE_CODE);
        assert_eq!(f.severity, DoctorSeverity::Warn);
        let rem = f.remediation.as_deref().unwrap_or("");
        assert_eq!(rem.lines().next(), Some("cargo install --path . --force"));
        assert!(!rem.contains("update --binary"));
    }

    #[test]
    fn same_version_ancestor_recommends_worktree_install() {
        let lag = classify(
            "0.2.17",
            Some("0.2.17"),
            "aaaaaaaaaaaa",
            Some("bbbbbbbbbbbb"),
            true,
            CommitAncestry::RunningIsAncestor,
        )
        .expect("ancestor");
        let f = build_binary_currency_finding(&lag);
        assert_eq!(f.code, BINARY_BEHIND_TREE_CODE);
        assert!(f.message.contains("ancestor"), "{}", f.message);
        assert_eq!(
            f.remediation.as_deref(),
            Some(BINARY_BEHIND_TREE_REMEDIATION)
        );
    }

    #[test]
    fn needs_ancestry_returns_false_for_prefix_equal_usable_shas() {
        assert!(!needs_ancestry(
            "0.2.17",
            Some("0.2.17"),
            "abcdef012345",
            Some("abcdef0123456789abcdef0123456789abcdef01"),
        ));
    }

    #[test]
    fn same_version_same_commit_ancestry_yields_no_finding() {
        // Inject ancestry with deliberately non-prefix-equal SHAs to reach its match arm.
        assert_eq!(
            classify(
                "0.2.17",
                Some("0.2.17"),
                "aaaaaaaaaaaa",
                Some("bbbbbbbbbbbb"),
                true,
                CommitAncestry::SameCommit,
            ),
            None
        );
    }

    #[test]
    fn unparsed_version_same_commit_ancestry_yields_no_finding() {
        // Inject ancestry with deliberately non-prefix-equal SHAs to reach its match arm.
        assert_eq!(
            classify(
                "not-a-version",
                Some("0.2.17"),
                "aaaaaaaaaaaa",
                Some("bbbbbbbbbbbb"),
                true,
                CommitAncestry::SameCommit,
            ),
            None
        );
    }

    #[test]
    fn same_version_unknown_ancestry_does_not_install() {
        let lag = classify(
            "0.2.17",
            Some("0.2.17"),
            "aaaaaaaaaaaa",
            Some("bbbbbbbbbbbb"),
            true,
            CommitAncestry::Unknown,
        )
        .expect("unknown");
        let f = build_binary_currency_finding(&lag);
        assert_eq!(f.code, BINARY_DIVERGES_FROM_TREE_CODE);
        assert!(f.message.contains("ancestry is unknown"), "{}", f.message);
        assert!(!f.message.contains("behind"), "{}", f.message);
        assert!(!f.message.contains("diverged"), "{}", f.message);
        assert_eq!(f.remediation.as_deref(), Some("ledgerful --version"));
    }

    #[test]
    fn same_version_divergent_ancestry_does_not_install() {
        let lag = classify(
            "0.2.17",
            Some("0.2.17"),
            "aaaaaaaaaaaa",
            Some("bbbbbbbbbbbb"),
            true,
            CommitAncestry::Divergent,
        )
        .expect("divergent");
        let f = build_binary_currency_finding(&lag);
        assert_eq!(f.code, BINARY_DIVERGES_FROM_TREE_CODE);
        assert_eq!(f.severity, DoctorSeverity::Info);
        assert!(f.message.contains("diverged"), "{}", f.message);
        assert!(!f.message.contains("unknown"), "{}", f.message);
        assert!(!f.message.contains("behind"), "{}", f.message);
        assert_eq!(f.remediation.as_deref(), Some("ledgerful --version"));
    }

    #[test]
    fn same_version_head_ancestor_is_ahead() {
        let lag = classify(
            "0.2.17",
            Some("0.2.17"),
            "bbbbbbbbbbbb",
            Some("aaaaaaaaaaaa"),
            true,
            CommitAncestry::HeadIsAncestor,
        )
        .expect("ahead");
        let f = build_binary_currency_finding(&lag);
        assert_eq!(f.code, BINARY_AHEAD_OF_TREE_CODE);
        assert!(
            f.message.contains("Do not install this worktree over it."),
            "{}",
            f.message
        );
        assert!(!f.message.contains("cargo install"));
    }

    #[test]
    fn unparseable_version_does_not_say_behind() {
        let lag = classify(
            "not-a-version",
            Some("0.2.17"),
            "unknown",
            Some("bbbbbbbbbbbb"),
            true,
            CommitAncestry::NotConsulted,
        )
        .expect("unparsed");
        let f = build_binary_currency_finding(&lag);
        assert_eq!(f.code, BINARY_DIVERGES_FROM_TREE_CODE);
        assert!(
            f.message.contains("version was not comparable"),
            "{}",
            f.message
        );
        assert!(!f.message.contains("behind"), "{}", f.message);

        let ancestor = classify(
            "not-a-version",
            Some("0.2.17"),
            "aaaaaaaaaaaa",
            Some("bbbbbbbbbbbb"),
            true,
            CommitAncestry::RunningIsAncestor,
        )
        .expect("unparsed ancestor");
        let behind = build_binary_currency_finding(&ancestor);
        assert_eq!(behind.code, BINARY_BEHIND_TREE_CODE);
        assert!(behind.message.contains("version was not comparable"));
        assert!(!behind.message.contains("not-a-version vs"));
        assert!(!behind.message.contains("running not-a-version vs worktree"));
    }

    #[test]
    fn ahead_and_diverge_are_info_later() {
        let ahead = build_binary_currency_finding(
            &classify(
                "0.2.17",
                Some("0.2.16"),
                "unknown",
                None,
                true,
                CommitAncestry::NotConsulted,
            )
            .expect("ahead"),
        );
        let diverge = build_binary_currency_finding(
            &classify(
                "0.2.17",
                Some("0.2.17"),
                "aaaaaaaaaaaa",
                Some("bbbbbbbbbbbb"),
                true,
                CommitAncestry::Divergent,
            )
            .expect("diverge"),
        );
        assert_eq!(ahead.severity, DoctorSeverity::Info);
        assert_eq!(diverge.severity, DoctorSeverity::Info);
        assert!(crate::doctor::finding::is_hygiene(&ahead));
        assert!(crate::doctor::finding::is_hygiene(&diverge));
        assert_eq!(dashboard_failures(&[ahead.clone(), diverge.clone()]), 0);
        assert!(ready_for_publish(&[ahead.clone(), diverge.clone()]));
        let mut findings = vec![ahead, diverge];
        super::super::assign_session_priorities(
            &mut findings,
            &crate::config::model::Config::default(),
        );
        assert!(
            findings
                .iter()
                .all(|f| f.session_priority == SessionPriority::Later)
        );
    }

    #[test]
    fn behind_tree_stays_warn_now() {
        let behind = build_binary_currency_finding(
            &classify(
                "0.2.16",
                Some("0.2.17"),
                "unknown",
                None,
                true,
                CommitAncestry::NotConsulted,
            )
            .expect("behind"),
        );
        assert_eq!(behind.severity, DoctorSeverity::Warn);
        assert_eq!(behind.code, BINARY_BEHIND_TREE_CODE);
        assert!(!crate::doctor::finding::is_hygiene(&behind));
        assert_eq!(dashboard_failures(std::slice::from_ref(&behind)), 1);
        assert!(ready_for_publish(std::slice::from_ref(&behind)));
        let mut findings = vec![behind];
        super::super::assign_session_priorities(
            &mut findings,
            &crate::config::model::Config::default(),
        );
        assert_eq!(findings[0].session_priority, SessionPriority::Now);
    }

    #[test]
    fn is_engine_false_for_consumer_temp_layout() {
        let tmp = tempdir().expect("tempdir");
        let root = tmp.path();
        fs::write(
            root.join("Cargo.toml"),
            r#"[package]
name = "my-app"
version = "1.0.0"
"#,
        )
        .expect("write");
        assert!(!is_ledgerful_engine_worktree(root));

        fs::write(
            root.join("Cargo.toml"),
            r#"[package]
name = "ledgerful"
version = "0.2.5"
"#,
        )
        .expect("write");
        assert!(!is_ledgerful_engine_worktree(root));

        fs::create_dir_all(root.join("src").join("cli")).expect("dirs");
        fs::create_dir_all(root.join("src").join("cli").join("args")).expect("args dir");
        fs::write(
            root.join("src").join("cli").join("args").join("mod.rs"),
            "// stub",
        )
        .expect("args");
        assert!(is_ledgerful_engine_worktree(root));
        assert_eq!(worktree_package_version(root).as_deref(), Some("0.2.5"));
    }

    #[test]
    fn full_sha_shortened_for_compare() {
        let lag = classify(
            "0.2.5",
            Some("0.2.5"),
            "aaaaaaaaaaaa",
            Some("aaaaaaaaaaaabbbbbbbbbbbbbbbbbbbbbbbb"),
            true,
            CommitAncestry::Divergent,
        );
        assert!(lag.is_none(), "prefix of full HEAD must equal short embed");
    }

    #[test]
    fn embed_sha_fail_soft_unknown_on_git_failure_or_empty() {
        assert_eq!(embed_sha_from_rev_parse(false, "deadbeefcafe"), "unknown");
        assert_eq!(embed_sha_from_rev_parse(true, ""), "unknown");
        assert_eq!(embed_sha_from_rev_parse(true, "   \n"), "unknown");
        assert_eq!(
            embed_sha_from_rev_parse(true, "b57f4472efb3\n"),
            "b57f4472efb3"
        );
    }

    #[test]
    fn version_long_includes_sha_only_when_known() {
        assert_eq!(compose_version_long("0.2.5", "unknown"), "0.2.5");
        assert_eq!(compose_version_long("0.2.5", ""), "0.2.5");
        assert_eq!(
            compose_version_long("0.2.5", "b57f4472efb3"),
            "0.2.5 (b57f4472efb3)"
        );
    }

    #[test]
    fn compiled_embed_env_matches_version_long_contract() {
        let sha = env!("LEDGERFUL_GIT_SHA");
        let pkg = env!("CARGO_PKG_VERSION");
        let long = env!("LEDGERFUL_VERSION_LONG");
        assert!(!sha.is_empty(), "LEDGERFUL_GIT_SHA must be set by build.rs");
        assert_eq!(long, compose_version_long(pkg, sha));
    }
}
