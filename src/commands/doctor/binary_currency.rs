//! Engine binary currency vs worktree (0137).
//!
//! When doctor runs inside the Ledgerful engine checkout and the executing
//! binary lags Cargo.toml version and/or embedded build SHA vs HEAD, emit one
//! warn finding with install-only remediation (never auto-install).
//!
//! Consumer repos (non-engine) stay silent. Runtime HEAD is via gix only —
//! no `Command::new("git")` at doctor runtime.

use super::finding::{DoctorCategory, DoctorFinding, DoctorSeverity};
use std::path::Path;

/// Stable finding code (greppable; distinct from `tool-git`).
pub const BINARY_BEHIND_TREE_CODE: &str = "binary-behind-tree";

/// Remediation lines always include `--force` for same-version reinstall (B3).
pub const BINARY_BEHIND_TREE_REMEDIATION: &str = "\
cargo install --path . --force
ledgerful update --binary
ledgerful --version";

/// Arms that fired for a single `binary-behind-tree` finding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BinaryCurrencyLag {
    pub version_lag: bool,
    pub commit_lag: bool,
    pub running_version: String,
    pub worktree_version: String,
    pub running_sha: String,
    pub worktree_head_sha: String,
}

/// Engine layout fingerprint: package name is exactly `ledgerful` **and**
/// `src/cli/args/mod.rs` exists under that root.
///
/// Layout fingerprint only — if CLI layout moves, update this marker.
/// Do **not** path-string match `"ledgerful"` in the directory name.
pub fn is_ledgerful_engine_worktree(root: &Path) -> bool {
    // Layout fingerprint: if CLI layout moves, update this marker.
    if !root
        .join("src")
        .join("cli")
        .join("args")
        .join("mod.rs")
        .is_file()
    {
        return false;
    }
    let Ok(content) = std::fs::read_to_string(root.join("Cargo.toml")) else {
        return false;
    };
    let Ok(value) = toml::from_str::<toml::Value>(&content) else {
        return false;
    };
    value
        .get("package")
        .and_then(|p| p.get("name"))
        .and_then(|n| n.as_str())
        == Some("ledgerful")
}

/// Worktree `package.version` from root `Cargo.toml`, when parseable and non-empty.
pub fn worktree_package_version(root: &Path) -> Option<String> {
    let content = std::fs::read_to_string(root.join("Cargo.toml")).ok()?;
    let value: toml::Value = toml::from_str(&content).ok()?;
    value
        .get("package")
        .and_then(|p| p.get("version"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Lowercase hex normalize + prefix equality (7 vs 12 → equal).
///
/// Empty inputs are never equal.
pub fn sha_prefix_equal(a: &str, b: &str) -> bool {
    let a = normalize_sha_hex(a);
    let b = normalize_sha_hex(b);
    if a.is_empty() || b.is_empty() {
        return false;
    }
    a.starts_with(&b) || b.starts_with(&a)
}

fn normalize_sha_hex(s: &str) -> String {
    s.trim().to_ascii_lowercase()
}

/// First 12 lowercase hex characters of a full (or short) SHA for display/compare.
pub fn shorten_sha_for_display(full_or_short: &str) -> String {
    normalize_sha_hex(full_or_short).chars().take(12).collect()
}

/// Pure classifier with injected facts (unit-testable).
///
/// Returns [`Some`] lag when B1 (version) and/or B2 (commit) fire under the
/// engine gate; [`None`] when not engine, equal, or uncomparable (unknown SHA
/// skips B2 only — version arm still applies when both versions present).
pub fn classify_binary_currency(
    running_version: &str,
    worktree_version: Option<&str>,
    running_sha: &str,
    worktree_head_sha: Option<&str>,
    is_engine: bool,
) -> Option<BinaryCurrencyLag> {
    if !is_engine {
        return None;
    }

    let run_ver = running_version.trim();
    let wt_ver = worktree_version.map(str::trim).filter(|s| !s.is_empty());

    let version_lag = match (run_ver.is_empty(), wt_ver) {
        (false, Some(w)) => run_ver != w,
        _ => false,
    };

    let run_sha = running_sha.trim();
    let run_sha_usable = !run_sha.is_empty() && !run_sha.eq_ignore_ascii_case("unknown");
    let wt_head = worktree_head_sha
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(shorten_sha_for_display);

    let commit_lag = match (run_sha_usable, wt_head.as_deref()) {
        (true, Some(head)) => !sha_prefix_equal(run_sha, head),
        _ => false,
    };

    if !version_lag && !commit_lag {
        return None;
    }

    Some(BinaryCurrencyLag {
        version_lag,
        commit_lag,
        running_version: run_ver.to_string(),
        worktree_version: wt_ver.unwrap_or("").to_string(),
        running_sha: if run_sha_usable {
            shorten_sha_for_display(run_sha)
        } else {
            run_sha.to_string()
        },
        worktree_head_sha: wt_head.unwrap_or_default(),
    })
}

/// Per-arm message (B3): only name fields for arms that actually differ.
pub fn compose_binary_currency_message(lag: &BinaryCurrencyLag) -> String {
    match (lag.version_lag, lag.commit_lag) {
        (true, true) => format!(
            "Installed ledgerful binary is behind the engine worktree: running {} ({}) vs worktree {} ({})",
            lag.running_version, lag.running_sha, lag.worktree_version, lag.worktree_head_sha
        ),
        (true, false) => format!(
            "Installed ledgerful binary is behind the engine worktree: running {} vs worktree {}",
            lag.running_version, lag.worktree_version
        ),
        (false, true) => format!(
            "Installed ledgerful binary is behind the engine worktree: same version {}; running commit {} vs worktree HEAD {}",
            lag.running_version, lag.running_sha, lag.worktree_head_sha
        ),
        (false, false) => {
            // Caller should only build a finding when an arm fired.
            "Installed ledgerful binary is behind the engine worktree".to_string()
        }
    }
}

/// Build the single `binary-behind-tree` finding (warn / tools; install remediation).
pub fn build_binary_behind_tree_finding(lag: &BinaryCurrencyLag) -> DoctorFinding {
    DoctorFinding {
        code: BINARY_BEHIND_TREE_CODE.to_string(),
        severity: DoctorSeverity::Warn,
        category: DoctorCategory::Tools,
        message: compose_binary_currency_message(lag),
        remediation: Some(BINARY_BEHIND_TREE_REMEDIATION.to_string()),
    }
}

/// Probe: resolve engine root, gather facts, return finding if lagging.
///
/// Prefer `layout_root` when it carries engine markers; else `current_dir`.
/// Runtime HEAD via gix only (no git subprocess).
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
    let worktree_head = crate::git::repo::open_repo(engine_root)
        .ok()
        .and_then(|repo| crate::git::repo::get_head_info(&repo).ok())
        .and_then(|(hash, _)| hash);

    let lag = classify_binary_currency(
        running_version,
        worktree_version.as_deref(),
        running_sha,
        worktree_head.as_deref(),
        true,
    )?;
    Some(build_binary_behind_tree_finding(&lag))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::doctor::finding::{dashboard_failures, ready_for_publish};
    use std::fs;
    use tempfile::tempdir;

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
        let lag = classify_binary_currency(
            "0.2.5",
            Some("0.2.5"),
            "aaaaaaaaaaaa",
            Some("bbbbbbbbbbbb"),
            true,
        )
        .expect("dogfood SHA lag must find");
        assert!(!lag.version_lag);
        assert!(lag.commit_lag);
        let f = build_binary_behind_tree_finding(&lag);
        assert_eq!(f.code, BINARY_BEHIND_TREE_CODE);
        assert_eq!(f.severity, DoctorSeverity::Warn);
        assert_eq!(f.category, DoctorCategory::Tools);
        assert!(
            f.message.contains("same version 0.2.5"),
            "must name same version, not version mismatch: {}",
            f.message
        );
        assert!(
            f.message.contains("aaaaaaaaaaaa") && f.message.contains("bbbbbbbbbbbb"),
            "must name SHAs: {}",
            f.message
        );
        assert!(
            !f.message.contains("running 0.2.5 vs worktree"),
            "must not claim version mismatch: {}",
            f.message
        );
        assert!(ready_for_publish(std::slice::from_ref(&f)));
        assert_eq!(dashboard_failures(std::slice::from_ref(&f)), 1);
    }

    #[test]
    fn version_only_message_has_no_fake_sha() {
        let lag = classify_binary_currency(
            "0.2.4",
            Some("0.2.5"),
            "unknown",
            Some("bbbbbbbbbbbb"),
            true,
        )
        .expect("version lag");
        assert!(lag.version_lag);
        assert!(!lag.commit_lag);
        let msg = compose_binary_currency_message(&lag);
        assert!(msg.contains("running 0.2.4 vs worktree 0.2.5"));
        assert!(!msg.contains("unknown"));
        assert!(!msg.contains("bbbbbbbbbbbb"));
        assert!(!msg.contains("commit"));
    }

    #[test]
    fn sha_only_and_both_are_single_finding() {
        let sha_only = classify_binary_currency(
            "0.2.5",
            Some("0.2.5"),
            "111111111111",
            Some("222222222222"),
            true,
        )
        .expect("sha only");
        assert!(!sha_only.version_lag && sha_only.commit_lag);

        let both = classify_binary_currency(
            "0.2.4",
            Some("0.2.5"),
            "111111111111",
            Some("222222222222"),
            true,
        )
        .expect("both");
        assert!(both.version_lag && both.commit_lag);
        let msg = compose_binary_currency_message(&both);
        assert!(msg.contains("0.2.4") && msg.contains("0.2.5"));
        assert!(msg.contains("111111111111") && msg.contains("222222222222"));
        // One finding builder, not two codes.
        let f = build_binary_behind_tree_finding(&both);
        assert_eq!(f.code, BINARY_BEHIND_TREE_CODE);
    }

    #[test]
    fn non_engine_gate_returns_none() {
        assert!(
            classify_binary_currency(
                "0.2.4",
                Some("0.2.5"),
                "aaaaaaaaaaaa",
                Some("bbbbbbbbbbbb"),
                false,
            )
            .is_none()
        );
    }

    #[test]
    fn unknown_sha_equal_version_no_finding() {
        assert!(
            classify_binary_currency(
                "0.2.5",
                Some("0.2.5"),
                "unknown",
                Some("bbbbbbbbbbbb"),
                true,
            )
            .is_none()
        );
        assert!(
            classify_binary_currency("0.2.5", Some("0.2.5"), "", Some("bbbbbbbbbbbb"), true,)
                .is_none()
        );
    }

    #[test]
    fn prefix_sha_equality_no_finding() {
        // 7 vs 12 where short is prefix of long
        assert!(
            classify_binary_currency(
                "0.2.5",
                Some("0.2.5"),
                "b57f447",
                Some("b57f4472efb3"),
                true,
            )
            .is_none()
        );
        // reverse direction
        assert!(
            classify_binary_currency(
                "0.2.5",
                Some("0.2.5"),
                "b57f4472efb3",
                Some("b57f447"),
                true,
            )
            .is_none()
        );
        // case-insensitive
        assert!(sha_prefix_equal("B57F4472EFB3", "b57f4472efb3"));
    }

    #[test]
    fn equal_version_and_sha_no_finding() {
        assert!(
            classify_binary_currency(
                "0.2.5",
                Some("0.2.5"),
                "aaaaaaaaaaaa",
                Some("aaaaaaaaaaaa"),
                true,
            )
            .is_none()
        );
    }

    #[test]
    fn remediation_has_force_and_update_binary() {
        let lag = classify_binary_currency(
            "0.2.5",
            Some("0.2.5"),
            "aaaaaaaaaaaa",
            Some("bbbbbbbbbbbb"),
            true,
        )
        .expect("lag");
        let f = build_binary_behind_tree_finding(&lag);
        let rem = f.remediation.as_deref().expect("remediation");
        assert!(
            rem.contains("cargo install --path . --force"),
            "must include --force: {rem}"
        );
        assert!(
            rem.contains("ledgerful update --binary"),
            "must include update --binary: {rem}"
        );
        assert!(rem.contains("ledgerful --version"));
    }

    #[test]
    fn is_engine_false_for_consumer_temp_layout() {
        let tmp = tempdir().expect("tempdir");
        let root = tmp.path();
        // Consumer-like: Cargo.toml named something else, no CLI layout.
        fs::write(
            root.join("Cargo.toml"),
            r#"[package]
name = "my-app"
version = "1.0.0"
"#,
        )
        .expect("write");
        assert!(!is_ledgerful_engine_worktree(root));

        // Even with ledgerful-ish dir name marker alone must not suffice —
        // missing args/mod.rs.
        fs::write(
            root.join("Cargo.toml"),
            r#"[package]
name = "ledgerful"
version = "0.2.5"
"#,
        )
        .expect("write");
        assert!(!is_ledgerful_engine_worktree(root));

        // Name + fingerprint → engine.
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
        let lag = classify_binary_currency(
            "0.2.5",
            Some("0.2.5"),
            "aaaaaaaaaaaa",
            // full 40-char that starts with aaaaaaaaaaaa → equal
            Some("aaaaaaaaaaaabbbbbbbbbbbbbbbbbbbbbbbb"),
            true,
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
        // Live binary under test: env! values must obey the same pure helper.
        let sha = env!("LEDGERFUL_GIT_SHA");
        let pkg = env!("CARGO_PKG_VERSION");
        let long = env!("LEDGERFUL_VERSION_LONG");
        assert!(!sha.is_empty(), "LEDGERFUL_GIT_SHA must be set by build.rs");
        assert_eq!(long, compose_version_long(pkg, sha));
    }
}
