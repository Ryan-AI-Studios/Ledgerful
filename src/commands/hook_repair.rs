//! Repair already-installed `.git/hooks` scripts that invoke the retired
//! binary name instead of the canonical `ledgerful` binary.
//!
//! `ledgerful init` (see `src/commands/init.rs`) already writes hooks that
//! call `ledgerful`. This module fixes up hooks that were installed by an
//! older version of `init` (or hand-written).
//!
//! Only exact command invocations are rewritten via literal substring
//! replacement; idempotency marker comments (e.g. `# ledgerful-ledger-gate`)
//! and `.ledgerful/` paths are deliberately left untouched because the
//! replacement patterns never match them.

use camino::{Utf8Path, Utf8PathBuf};
use miette::{IntoDiagnostic, Result};
use owo_colors::OwoColorize;
use std::fs;

/// Exact substring replacements applied to hook file contents, in order.
///
/// Each pattern is a full command invocation fragment (binary name + a
/// trailing space/subcommand marker) so it can never match the
/// `# ledgerful-ledger-gate` / `# ledgerful-verify-gate` / `# ledgerful-intent-gate`
/// / `# ledgerful-post-commit-gate` marker comments (hyphen immediately
/// after `ledgerful`, no space) or `.ledgerful/` directory paths
/// (preceded by `.`, not a word boundary the patterns below match against).
const LEGACY_BINARY: &str = concat!("change", "guard");

/// Names of known third-party hook managers, checked in priority order.
/// Mirrors the canonical spelling used by `is_pre_commit_path` in
/// `src/index/ci_gates.rs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThirdPartyHookManager {
    Husky,
    Lefthook,
    PreCommit,
}

impl ThirdPartyHookManager {
    pub fn name(&self) -> &'static str {
        match self {
            ThirdPartyHookManager::Husky => "husky",
            ThirdPartyHookManager::Lefthook => "lefthook",
            ThirdPartyHookManager::PreCommit => "pre-commit",
        }
    }

    /// Relative path (from repo root) to the manager's config file/dir, used
    /// in the hint printed alongside the warning.
    pub fn config_hint(&self) -> &'static str {
        match self {
            ThirdPartyHookManager::Husky => ".husky/",
            ThirdPartyHookManager::Lefthook => "lefthook.yml",
            ThirdPartyHookManager::PreCommit => ".pre-commit-config.yaml",
        }
    }
}

/// Detect a third-party hook manager at `root`, checking in fixed priority
/// order: husky, lefthook, pre-commit. Returns only the first match.
pub fn detect_third_party_hook_manager(root: &Utf8Path) -> Option<ThirdPartyHookManager> {
    if root.join(".husky").is_dir() {
        return Some(ThirdPartyHookManager::Husky);
    }
    if root.join("lefthook.yml").is_file() {
        return Some(ThirdPartyHookManager::Lefthook);
    }
    if root.join(".pre-commit-config.yaml").is_file() {
        return Some(ThirdPartyHookManager::PreCommit);
    }
    None
}

/// Outcome of a single hook file's repair attempt.
#[derive(Debug, Clone)]
pub struct HookRepairReport {
    /// Hooks that contained at least one stale invocation and were rewritten
    /// (or, in dry-run mode, would be rewritten). Sorted by filename.
    pub repaired: Vec<String>,
    /// Hooks that already used `ledgerful` invocations exclusively (no
    /// `ledgerful` invocations found to replace). Sorted by filename.
    pub already_correct: Vec<String>,
    /// Hooks with no ledger-related invocations at all; left untouched.
    /// Sorted by filename.
    pub skipped: Vec<String>,
    /// Third-party hook manager detected, if any. When set, no hooks were
    /// rewritten regardless of their contents.
    pub third_party_manager: Option<ThirdPartyHookManager>,
    /// True if this was a dry run (nothing was actually written to disk).
    pub dry_run: bool,
}

impl HookRepairReport {
    fn empty(dry_run: bool) -> Self {
        Self {
            repaired: Vec::new(),
            already_correct: Vec::new(),
            skipped: Vec::new(),
            third_party_manager: None,
            dry_run,
        }
    }
}

/// Apply the fixed set of literal replacements to `content`. Returns the
/// rewritten content and whether any replacement actually fired.
fn apply_replacements(content: &str) -> (String, bool) {
    let mut result = content.to_string();
    let mut changed = false;
    for (current, suffix) in [
        ("command -v ledgerful", "command -v "),
        ("ledgerful ledger", ""),
        ("ledgerful internal hook-", ""),
        ("ledgerful verify", ""),
    ] {
        let retired = if suffix.is_empty() {
            current.replacen("ledgerful", LEGACY_BINARY, 1)
        } else {
            format!("{suffix}{LEGACY_BINARY}")
        };
        if result.contains(&retired) {
            result = result.replace(&retired, current);
            changed = true;
        }
    }
    (result, changed)
}

/// Whether `content` contains any retired or current Ledgerful invocations we
/// know how to repair. Used to distinguish "already correct" (ledger hook
/// with no stale invocations left) from "skipped" (not a ledger hook at
/// all) when no replacement fires.
fn contains_ledger_invocation(content: &str) -> bool {
    let legacy_markers = [
        format!("command -v {LEGACY_BINARY}"),
        format!("{LEGACY_BINARY} ledger"),
        format!("{LEGACY_BINARY} internal hook-"),
        format!("{LEGACY_BINARY} verify"),
    ];
    const CURRENT_MARKERS: &[&str] = &[
        "command -v ledgerful",
        "ledgerful ledger",
        "ledgerful internal hook-",
        "ledgerful verify",
    ];
    legacy_markers.iter().any(|marker| content.contains(marker))
        || CURRENT_MARKERS
            .iter()
            .any(|marker| content.contains(marker))
}

/// Core repair logic operating on an explicit `.git` directory root. Pure
/// with respect to its inputs other than the filesystem at `git_dir`'s
/// parent (the repo root, used for third-party manager detection) and the
/// hooks directory itself.
///
/// `repo_root` is the directory containing `.git` (used only to look for
/// `.husky/`, `lefthook.yml`, `.pre-commit-config.yaml`).
pub fn repair_hooks_at(repo_root: &Utf8Path, dry_run: bool) -> Result<HookRepairReport> {
    let mut report = HookRepairReport::empty(dry_run);

    if let Some(manager) = detect_third_party_hook_manager(repo_root) {
        report.third_party_manager = Some(manager);
        return Ok(report);
    }

    let hooks_dir = repo_root.join(".git").join("hooks");
    if !hooks_dir.is_dir() {
        return Ok(report);
    }

    let entries = fs::read_dir(hooks_dir.as_std_path()).into_diagnostic()?;
    let mut filenames: Vec<Utf8PathBuf> = Vec::new();
    for entry in entries {
        let entry = entry.into_diagnostic()?;
        let path = entry.path();
        if path.is_dir() {
            continue;
        }
        let Some(utf8_path) = Utf8PathBuf::from_path_buf(path).ok() else {
            continue;
        };
        let Some(name) = utf8_path.file_name() else {
            continue;
        };
        if name.ends_with(".sample") {
            continue;
        }
        filenames.push(utf8_path);
    }

    for hook_path in filenames {
        // Safe: filtered to files with a valid file_name above.
        let Some(name) = hook_path.file_name() else {
            continue;
        };
        let name = name.to_string();
        let content = fs::read_to_string(hook_path.as_std_path()).into_diagnostic()?;
        let (rewritten, changed) = apply_replacements(&content);

        if changed {
            if !dry_run {
                fs::write(hook_path.as_std_path(), &rewritten).into_diagnostic()?;
            }
            report.repaired.push(name);
        } else if contains_ledger_invocation(&content) {
            report.already_correct.push(name);
        } else {
            report.skipped.push(name);
        }
    }

    report.repaired.sort();
    report.already_correct.sort();
    report.skipped.sort();

    Ok(report)
}

/// Public entry point: discover the repo root the same way `execute_init`
/// does, then repair `.git/hooks` in place. Prints a human-readable summary.
pub fn execute_hook_repair(dry_run: bool) -> Result<()> {
    let root = match gix::discover(".") {
        Ok(repo) => {
            let path = repo
                .workdir()
                .ok_or(crate::commands::CommandError::RepoDiscoveryFailed)?
                .to_path_buf();
            Utf8PathBuf::from_path_buf(path)
                .map_err(|_| crate::commands::CommandError::RepoDiscoveryFailed)?
        }
        Err(_) => Utf8PathBuf::from_path_buf(std::env::current_dir().into_diagnostic()?)
            .map_err(|_| crate::commands::CommandError::RepoDiscoveryFailed)?,
    };

    let report = repair_hooks_at(&root, dry_run)?;
    print_report(&report);
    Ok(())
}

fn print_report(report: &HookRepairReport) {
    if let Some(manager) = report.third_party_manager {
        println!(
            "{} Third-party hook manager '{}' detected. Hooks are managed by '{}', not .git/hooks. Please update your {} config to call ledgerful.",
            "WARN:".yellow().bold(),
            manager.name(),
            manager.name(),
            manager.name(),
        );
        println!("  Config location: {}", manager.config_hint());
        return;
    }

    let prefix = if report.dry_run {
        "DRY-RUN".yellow().bold().to_string()
    } else {
        "DONE".green().bold().to_string()
    };

    if report.repaired.is_empty() && report.already_correct.is_empty() && report.skipped.is_empty()
    {
        println!("{prefix} No .git/hooks directory found; nothing to repair.");
    } else {
        let verb = if report.dry_run {
            "Would repair"
        } else {
            "Repaired"
        };
        println!(
            "{prefix} {verb} {} hook(s): {}",
            report.repaired.len(),
            if report.repaired.is_empty() {
                "(none)".to_string()
            } else {
                report.repaired.join(", ")
            }
        );
        if !report.already_correct.is_empty() {
            println!("  Already correct: {}", report.already_correct.join(", "));
        }
        if !report.skipped.is_empty() {
            println!(
                "  Skipped (no ledger invocations found): {}",
                report.skipped.join(", ")
            );
        }
    }

    println!(
        "{} If `ledgerful` still resolves to a stale binary on your PATH outside this install location, run `ledgerful update --binary` to refresh it, or remove the stale copy manually.",
        "HINT:".cyan().bold()
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// The exact real stale `pre-push` hook content from this repo's
    /// `.git/hooks/pre-push`, captured verbatim (see trackTA23 brief).
    const CURRENT_PRE_PUSH: &str = r#"#!/usr/bin/env bash

# ledgerful-ledger-gate: auto-installed by `ledgerful init`
if command -v ledgerful &>/dev/null; then
    if ! ledgerful ledger status --compact --exit-code 2>/dev/null; then
        echo ""
        echo "  Resolve with:"
        echo "    Pending tx:  ledgerful ledger commit <tx-id> --summary '...' --reason '...'"
        echo "    Drift:       ledgerful ledger reconcile --all --reason '...'"
        echo ""
        echo "  Bypass (not recommended): git push --no-verify"
        exit 1
    fi
fi

# ledgerful-verify-gate: full quality gate before push
echo "==> Running pre-push quality gate..."

if command -v ledgerful &>/dev/null; then
    if ! ledgerful verify; then
        echo ""
        echo "  Pre-push quality gate FAILED (ledgerful verify)."
        echo "  Fix the above errors before pushing."
        echo ""
        echo "  Bypass (not recommended): git push --no-verify"
        exit 1
    fi
else
    echo "  [warn] ledgerful not found, falling back to direct cargo checks."

    if ! cargo fmt --all -- --check; then
        echo ""
        echo "  Pre-push FAILED: formatting errors detected."
        echo "  Run: cargo fmt --all"
        echo ""
        exit 1
    fi

    if ! cargo clippy --all-targets --all-features -- -D warnings; then
        echo ""
        echo "  Pre-push FAILED: clippy warnings/errors detected."
        echo ""
        exit 1
    fi

    if ! cargo test; then
        echo ""
        echo "  Pre-push FAILED: test suite did not pass."
        echo ""
        exit 1
    fi
fi

echo "==> Quality gate passed. Pushing..."
"#;

    fn stale_pre_push() -> String {
        CURRENT_PRE_PUSH
            .replace(
                "command -v ledgerful",
                &format!("command -v {LEGACY_BINARY}"),
            )
            .replace("ledgerful ledger", &format!("{LEGACY_BINARY} ledger"))
            .replace("ledgerful verify", &format!("{LEGACY_BINARY} verify"))
    }

    fn make_repo(tmp: &std::path::Path) -> Utf8PathBuf {
        let root = Utf8PathBuf::from_path_buf(tmp.to_path_buf()).unwrap();
        fs::create_dir_all(root.join(".git").join("hooks")).unwrap();
        root
    }

    #[test]
    fn repair_rewrites_real_stale_pre_push_fixture() {
        let tmp = tempfile::tempdir().unwrap();
        let root = make_repo(tmp.path());
        let hook_path = root.join(".git").join("hooks").join("pre-push");
        fs::write(&hook_path, stale_pre_push()).unwrap();

        let report = repair_hooks_at(&root, false).unwrap();

        assert_eq!(report.repaired, vec!["pre-push".to_string()]);
        assert!(report.already_correct.is_empty());
        assert!(report.skipped.is_empty());
        assert!(report.third_party_manager.is_none());

        let rewritten = fs::read_to_string(&hook_path).unwrap();

        // Exact-invocation patterns must be converted.
        assert!(rewritten.contains("if command -v ledgerful &>/dev/null; then"));
        assert!(
            rewritten
                .contains("if ! ledgerful ledger status --compact --exit-code 2>/dev/null; then")
        );
        assert!(rewritten.contains(
            "Pending tx:  ledgerful ledger commit <tx-id> --summary '...' --reason '...'"
        ));
        assert!(rewritten.contains("Drift:       ledgerful ledger reconcile --all --reason '...'"));
        assert!(rewritten.contains("if ! ledgerful verify; then"));
        assert!(rewritten.contains("Pre-push quality gate FAILED (ledgerful verify)."));

        // No remaining bare `ledgerful` command invocations.
        assert!(!rewritten.contains(LEGACY_BINARY));

        // Marker comments must be preserved verbatim (untouched).
        assert!(rewritten.contains("# ledgerful-ledger-gate: auto-installed by `ledgerful init`"));
        assert!(rewritten.contains("# ledgerful-verify-gate: full quality gate before push"));

        // The fallback warning prose (doesn't match any of the 4 patterns)
        // must be untouched.
        assert!(rewritten.contains(
            "echo \"  [warn] ledgerful not found, falling back to direct cargo checks.\""
        ));

        // Unrelated cargo fallback commands untouched.
        assert!(rewritten.contains("if ! cargo fmt --all -- --check; then"));
        assert!(rewritten.contains("if ! cargo test; then"));
    }

    #[test]
    fn repair_leaves_comment_only_mentions_untouched() {
        let tmp = tempfile::tempdir().unwrap();
        let root = make_repo(tmp.path());
        let hook_path = root.join(".git").join("hooks").join("pre-commit");
        let content = "#!/usr/bin/env bash\n\
# This hook used to call ledgerful but now calls something else.\n\
MY_LEDGERFUL_VAR=\"not a command\"\n\
echo \"ledgerful was here\"\n";
        fs::write(&hook_path, content).unwrap();

        let report = repair_hooks_at(&root, false).unwrap();

        // None of the 4 exact-invocation patterns appear, so this is
        // classified as skipped (no ledger invocations found), not
        // repaired and not already_correct.
        assert!(report.repaired.is_empty());
        assert!(report.already_correct.is_empty());
        assert_eq!(report.skipped, vec!["pre-commit".to_string()]);

        let after = fs::read_to_string(&hook_path).unwrap();
        assert_eq!(
            after, content,
            "unrelated ledgerful mentions must be byte-identical after repair"
        );
    }

    #[test]
    fn repair_skips_hook_with_no_ledger_content() {
        let tmp = tempfile::tempdir().unwrap();
        let root = make_repo(tmp.path());
        let hook_path = root.join(".git").join("hooks").join("post-checkout");
        let content = "#!/usr/bin/env bash\necho \"unrelated user hook\"\n";
        fs::write(&hook_path, content).unwrap();

        let report = repair_hooks_at(&root, false).unwrap();

        assert!(report.repaired.is_empty());
        assert!(report.already_correct.is_empty());
        assert_eq!(report.skipped, vec!["post-checkout".to_string()]);

        let after = fs::read_to_string(&hook_path).unwrap();
        assert_eq!(
            after, content,
            "content must be byte-identical after repair"
        );
    }

    #[test]
    fn repair_classifies_already_ledgerful_hook_as_already_correct() {
        let tmp = tempfile::tempdir().unwrap();
        let root = make_repo(tmp.path());
        let hook_path = root.join(".git").join("hooks").join("commit-msg");
        let content = "#!/usr/bin/env bash\n\
# ledgerful-intent-gate: auto-installed by `ledgerful init`\n\
if command -v ledgerful &>/dev/null; then\n\
    ledgerful internal hook-commit-msg \"$1\"\n\
fi\n";
        fs::write(&hook_path, content).unwrap();

        let report = repair_hooks_at(&root, false).unwrap();

        assert!(report.repaired.is_empty());
        assert_eq!(report.already_correct, vec!["commit-msg".to_string()]);
        assert!(report.skipped.is_empty());

        let after = fs::read_to_string(&hook_path).unwrap();
        assert_eq!(after, content);
    }

    #[test]
    fn repair_skips_sample_files_and_subdirectories() {
        let tmp = tempfile::tempdir().unwrap();
        let root = make_repo(tmp.path());
        let hooks_dir = root.join(".git").join("hooks");
        fs::write(
            hooks_dir.join("pre-push.sample"),
            "#!/bin/sh\nledgerful ledger status\n",
        )
        .unwrap();
        fs::create_dir_all(hooks_dir.join("subdir")).unwrap();
        fs::write(
            hooks_dir.join("subdir").join("nested"),
            "ledgerful verify\n",
        )
        .unwrap();

        let report = repair_hooks_at(&root, false).unwrap();

        assert!(report.repaired.is_empty());
        assert!(report.already_correct.is_empty());
        assert!(report.skipped.is_empty());

        // Sample file must be untouched.
        let sample = fs::read_to_string(hooks_dir.join("pre-push.sample")).unwrap();
        assert!(sample.contains("ledgerful ledger status"));
    }

    #[test]
    fn repair_no_hooks_dir_returns_cleanly() {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        // No .git directory at all.

        let report = repair_hooks_at(&root, false).unwrap();

        assert!(report.repaired.is_empty());
        assert!(report.already_correct.is_empty());
        assert!(report.skipped.is_empty());
        assert!(report.third_party_manager.is_none());
    }

    #[test]
    fn repair_is_idempotent_second_call_reports_already_correct_with_identical_content() {
        let tmp = tempfile::tempdir().unwrap();
        let root = make_repo(tmp.path());
        let hook_path = root.join(".git").join("hooks").join("pre-push");
        fs::write(&hook_path, stale_pre_push()).unwrap();

        let first_report = repair_hooks_at(&root, false).unwrap();
        assert_eq!(first_report.repaired, vec!["pre-push".to_string()]);
        let first_contents = fs::read_to_string(&hook_path).unwrap();

        let second_report = repair_hooks_at(&root, false).unwrap();
        assert!(
            second_report.repaired.is_empty(),
            "second call must not find anything left to repair"
        );
        assert_eq!(
            second_report.already_correct,
            vec!["pre-push".to_string()],
            "second call must classify the already-rewritten hook as already correct"
        );
        assert!(second_report.skipped.is_empty());

        let second_contents = fs::read_to_string(&hook_path).unwrap();
        assert_eq!(
            first_contents, second_contents,
            "re-running repair on an already-repaired hook must be byte-identical"
        );
    }

    #[test]
    fn repair_dry_run_reports_without_writing() {
        let tmp = tempfile::tempdir().unwrap();
        let root = make_repo(tmp.path());
        let hook_path = root.join(".git").join("hooks").join("pre-push");
        let stale = stale_pre_push();
        fs::write(&hook_path, &stale).unwrap();

        let report = repair_hooks_at(&root, true).unwrap();

        assert_eq!(report.repaired, vec!["pre-push".to_string()]);
        assert!(report.dry_run);

        // File on disk must be untouched in dry-run mode.
        let after = fs::read_to_string(&hook_path).unwrap();
        assert_eq!(after, stale);
    }

    #[test]
    fn detect_husky_skips_rewriting() {
        let tmp = tempfile::tempdir().unwrap();
        let root = make_repo(tmp.path());
        fs::create_dir_all(root.join(".husky")).unwrap();
        let hook_path = root.join(".git").join("hooks").join("pre-push");
        let stale = stale_pre_push();
        fs::write(&hook_path, &stale).unwrap();

        let report = repair_hooks_at(&root, false).unwrap();

        assert_eq!(
            report.third_party_manager,
            Some(ThirdPartyHookManager::Husky)
        );
        assert!(report.repaired.is_empty());
        assert!(report.already_correct.is_empty());
        assert!(report.skipped.is_empty());

        // .git/hooks/pre-push must be completely untouched.
        let after = fs::read_to_string(&hook_path).unwrap();
        assert_eq!(after, stale);
    }

    #[test]
    fn detect_lefthook_skips_rewriting() {
        let tmp = tempfile::tempdir().unwrap();
        let root = make_repo(tmp.path());
        fs::write(root.join("lefthook.yml"), "pre-push:\n  commands:\n").unwrap();
        let hook_path = root.join(".git").join("hooks").join("pre-push");
        fs::write(&hook_path, stale_pre_push()).unwrap();

        let report = repair_hooks_at(&root, false).unwrap();

        assert_eq!(
            report.third_party_manager,
            Some(ThirdPartyHookManager::Lefthook)
        );
        assert!(report.repaired.is_empty());
    }

    #[test]
    fn detect_pre_commit_skips_rewriting() {
        let tmp = tempfile::tempdir().unwrap();
        let root = make_repo(tmp.path());
        fs::write(root.join(".pre-commit-config.yaml"), "repos: []\n").unwrap();
        let hook_path = root.join(".git").join("hooks").join("pre-push");
        fs::write(&hook_path, stale_pre_push()).unwrap();

        let report = repair_hooks_at(&root, false).unwrap();

        assert_eq!(
            report.third_party_manager,
            Some(ThirdPartyHookManager::PreCommit)
        );
        assert!(report.repaired.is_empty());
    }

    #[test]
    fn detect_priority_order_husky_wins_over_others() {
        let tmp = tempfile::tempdir().unwrap();
        let root = make_repo(tmp.path());
        fs::create_dir_all(root.join(".husky")).unwrap();
        fs::write(root.join("lefthook.yml"), "pre-push:\n").unwrap();
        fs::write(root.join(".pre-commit-config.yaml"), "repos: []\n").unwrap();

        let detected = detect_third_party_hook_manager(&root);
        assert_eq!(detected, Some(ThirdPartyHookManager::Husky));
    }

    #[test]
    fn replacement_patterns_never_match_marker_comments_or_ledgerful_dir() {
        // Current marker comments and state paths must remain byte-identical.
        let markers = [
            "# ledgerful-ledger-gate",
            "# ledgerful-verify-gate",
            "# ledgerful-intent-gate",
            "# ledgerful-post-commit-gate",
            ".ledgerful/state/ledger.db",
            ".ledgerful/config.toml",
        ];
        for marker in markers {
            assert_eq!(apply_replacements(marker), (marker.to_string(), false));
        }
    }

    #[test]
    fn repair_leaves_unrelated_retired_name_occurrences_untouched() {
        let content = format!(
            "# retired name in a comment: {0}\nPATH_HINT=/opt/{0}/bin\n{0}_CACHE=local\n",
            LEGACY_BINARY
        );

        assert_eq!(apply_replacements(&content), (content, false));
    }
}
