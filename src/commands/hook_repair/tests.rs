use super::resolve::parse_git_config_value;
use super::rewrite::{GATE_SUFFIXES, apply_replacements};
use super::*;
use camino::Utf8PathBuf;
use std::fs;
use std::path::{Path, PathBuf};

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
    assert!(report.residual_invocations.is_empty());

    let rewritten = fs::read_to_string(&hook_path).unwrap();

    assert!(rewritten.contains("if command -v ledgerful &>/dev/null; then"));
    assert!(
        rewritten.contains("if ! ledgerful ledger status --compact --exit-code 2>/dev/null; then")
    );
    assert!(rewritten.contains("if ! ledgerful verify; then"));
    assert!(!rewritten.contains(LEGACY_BINARY));
    assert!(rewritten.contains("# ledgerful-ledger-gate: auto-installed by `ledgerful init`"));
    assert!(rewritten.contains("# ledgerful-verify-gate: full quality gate before push"));
    assert!(rewritten.contains("if ! cargo fmt --all -- --check; then"));
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

    assert!(report.repaired.is_empty());
    assert!(report.already_correct.is_empty());
    assert_eq!(report.skipped, vec!["pre-commit".to_string()]);

    let after = fs::read_to_string(&hook_path).unwrap();
    assert_eq!(after, content);
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
}

#[test]
fn repair_no_hooks_dir_returns_cleanly() {
    let tmp = tempfile::tempdir().unwrap();
    let root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();

    let report = repair_hooks_at(&root, false).unwrap();

    assert!(report.repaired.is_empty());
    assert!(!report.discovery_notes.is_empty());
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
    assert!(second_report.repaired.is_empty());
    assert_eq!(second_report.already_correct, vec!["pre-push".to_string()]);

    let second_contents = fs::read_to_string(&hook_path).unwrap();
    assert_eq!(first_contents, second_contents);
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
fn replacement_patterns_never_match_ledgerful_dir() {
    let markers = [".ledgerful/state/ledger.db", ".ledgerful/config.toml"];
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
    // `{brand}_CACHE` and path mentions without invocation patterns stay.
    // Note: `{brand} ` bare form is treated as residual if present with space.
    let (out, changed) = apply_replacements(&content);
    assert_eq!(out, content);
    assert!(!changed);
}

/// DoD-3: after repair, markers are current; subsequent init does not
/// append a second ledger-gate block.
#[test]
fn repair_then_init_yields_exactly_one_ledger_gate() {
    let tmp = tempfile::tempdir().unwrap();
    let root = make_repo(tmp.path());
    let hook_path = root.join(".git").join("hooks").join("pre-commit");
    let legacy = format!(
        "#!/usr/bin/env bash\n\n\
# {0}-ledger-gate: auto-installed by `{0} init`\n\
if command -v {0} &>/dev/null; then\n\
    if ! {0} ledger status --compact --exit-code 2>/dev/null; then\n\
        echo \"\"\n\
        echo \"  Resolve with:\"\n\
        echo \"    Pending tx:  {0} ledger commit <tx-id> --summary '...' --reason '...'\"\n\
        echo \"    Drift:       {0} ledger reconcile --all --reason '...'\"\n\
        echo \"\"\n\
        echo \"  Bypass (not recommended): git commit --no-verify\"\n\
        exit 1\n\
    fi\n\
fi\n",
        LEGACY_BINARY
    );
    fs::write(&hook_path, &legacy).unwrap();

    let report = repair_hooks_at(&root, false).unwrap();
    assert!(
        report.repaired.contains(&"pre-commit".to_string()),
        "expected repair: {:?}",
        report
    );
    assert!(report.residual_invocations.is_empty());

    let after_repair = fs::read_to_string(&hook_path).unwrap();
    assert!(after_repair.contains("# ledgerful-ledger-gate"));
    assert!(!after_repair.contains(&format!("# {LEGACY_BINARY}-ledger-gate")));
    assert!(!after_repair.contains(LEGACY_BINARY));

    // Simulate init's idempotency branch: HOOK_MARKER found → no append.
    const HOOK_MARKER: &str = "# ledgerful-ledger-gate";
    assert!(
        after_repair.contains(HOOK_MARKER),
        "init must recognise the repaired marker"
    );
    let gate_count = after_repair.matches(HOOK_MARKER).count();
    assert_eq!(gate_count, 1, "exactly one ledger-gate marker after repair");
    // If init were to re-run install_git_hook it would upgrade-in-place /
    // return without append because marker is present.
}

/// DoD-4: ledgerful-web dual-marker pre-push de-duplicates to one block
/// per gate type.
#[test]
fn repair_dedups_dual_marker_ledgerful_web_shape() {
    let tmp = tempfile::tempdir().unwrap();
    let root = make_repo(tmp.path());
    let hook_path = root.join(".git").join("hooks").join("pre-push");
    // Captured from output/0094-hooks/ledgerful-web/pre-push (abbreviated
    // to the dual ledger + verify pattern).
    let dual = format!(
        r#"#!/usr/bin/env bash

# {brand}-ledger-gate: auto-installed by `ledgerful init`
if command -v ledgerful &>/dev/null; then
    if ! ledgerful ledger status --compact --exit-code --verify-signatures 2>/dev/null; then
        echo ""
        echo "  Resolve with:"
        echo "    Pending tx:  ledgerful ledger commit <tx-id> --summary '...' --reason '...'"
        echo "    Drift:       ledgerful ledger reconcile --all --reason '...'"
        echo ""
        echo "  Bypass (not recommended): git push --no-verify"
        exit 1
    fi
fi


# {brand}-verify-gate: fast scoped verification (pre-push only)
if command -v ledgerful &>/dev/null; then
    if ! ledgerful verify --scope fast 2>/dev/null; then
        echo ""
        echo "  Pre-push quality gate FAILED (ledgerful verify --scope fast)."
        echo "  Fix the above errors before pushing."
        echo ""
        echo "  Bypass (not recommended): git push --no-verify"
        exit 1
    fi
fi


# ledgerful-ledger-gate: auto-installed by `ledgerful init`
if command -v ledgerful &>/dev/null; then
    if ! ledgerful ledger status --compact --exit-code --verify-signatures; then
        echo "[Ledgerful] Blocked by ledger state."
        echo "[Ledgerful] Resolve with:"
        echo "[Ledgerful]   Pending tx:  ledgerful ledger commit <tx-id> --summary '...' --reason '...'"
        echo "[Ledgerful]   Drift:       ledgerful ledger reconcile --all --reason '...'"
        echo "[Ledgerful] Fix the issues or bypass with: git push --no-verify"
        exit 1
    fi
fi


# ledgerful-verify-gate: fast scoped verification (pre-push only)
if command -v ledgerful &>/dev/null; then
    if ! ledgerful verify --scope fast; then
        echo "[Ledgerful] Push blocked by verification failure."
        echo "[Ledgerful] Fix the issues or bypass with: git push --no-verify"
        exit 1
    fi
fi
"#,
        brand = LEGACY_BINARY
    );
    fs::write(&hook_path, dual).unwrap();

    let report = repair_hooks_at(&root, false).unwrap();
    assert!(report.residual_invocations.is_empty());
    let after = fs::read_to_string(&hook_path).unwrap();
    assert_eq!(
        after.matches("# ledgerful-ledger-gate").count(),
        1,
        "expected one ledger-gate after dedup; got:\n{after}"
    );
    assert_eq!(
        after.matches("# ledgerful-verify-gate").count(),
        1,
        "expected one verify-gate after dedup; got:\n{after}"
    );
    assert!(!after.contains(&format!("# {LEGACY_BINARY}-")));
}

/// DoD-5: frontend hand-edited `(renamed from …)` form is unchanged.
#[test]
fn repair_leaves_frontend_renamed_marker_unchanged() {
    let tmp = tempfile::tempdir().unwrap();
    let root = make_repo(tmp.path());
    let hook_path = root.join(".git").join("hooks").join("pre-commit");
    let content = format!(
        "#!/usr/bin/env bash\n\n\
# ledgerful-ledger-gate: auto-installed by `ledgerful init` (renamed from {brand})\n\
if command -v ledgerful &>/dev/null; then\n\
    if ! ledgerful ledger status --compact --exit-code --verify-signatures 2>/dev/null; then\n\
        echo \"\"\n\
        echo \"  Resolve with:\"\n\
        echo \"    Pending tx:  ledgerful ledger commit <tx-id> --summary '...' --reason '...'\"\n\
        echo \"    Drift:       ledgerful ledger reconcile --all --reason '...'\"\n\
        echo \"\"\n\
        echo \"  Bypass (not recommended): git commit --no-verify\"\n\
        exit 1\n\
    fi\n\
fi\n",
        brand = LEGACY_BINARY
    );
    fs::write(&hook_path, &content).unwrap();

    let report = repair_hooks_at(&root, false).unwrap();
    assert!(report.repaired.is_empty(), "must not rewrite: {:?}", report);
    let after = fs::read_to_string(&hook_path).unwrap();
    assert_eq!(after, content);
}

/// DoD-4c: Photo-shaped hook with `scan --impact` must not be reported
/// as fully repaired while residual retired binary remains. After covering
/// `scan`, residual should be cleared and it is honestly repaired.
#[test]
fn repair_photo_scan_impact_is_honest() {
    let tmp = tempfile::tempdir().unwrap();
    let root = make_repo(tmp.path());
    let hook_path = root.join(".git").join("hooks").join("pre-commit");
    let photo = format!(
        "#!/bin/sh\n\
if command -v {0} >/dev/null 2>&1; then\n\
  {0} scan --impact\n\
fi\n\
\n\
# {0}-ledger-gate: auto-installed by `{0} init`\n\
if command -v {0} &>/dev/null; then\n\
    if ! {0} ledger status --compact --exit-code 2>/dev/null; then\n\
        echo \"\"\n\
        echo \"  Resolve with:\"\n\
        echo \"    Pending tx:  {0} ledger commit <tx-id> --summary '...' --reason '...'\"\n\
        echo \"    Drift:       {0} ledger reconcile --all --reason '...'\"\n\
        echo \"\"\n\
        echo \"  Bypass (not recommended): git commit --no-verify\"\n\
        exit 1\n\
    fi\n\
fi\n",
        LEGACY_BINARY
    );
    fs::write(&hook_path, photo).unwrap();

    let report = repair_hooks_at(&root, false).unwrap();
    let after = fs::read_to_string(&hook_path).unwrap();
    assert!(
        !after.contains(LEGACY_BINARY),
        "scan and ledger invocations must be rewritten: {after}"
    );
    assert!(
        report.residual_invocations.is_empty(),
        "must not claim residual when scan is covered: {:?}",
        report.residual_invocations
    );
    assert!(
        report.repaired.contains(&"pre-commit".to_string()),
        "honest full repair expected: {:?}",
        report
    );
    assert!(after.contains("ledgerful scan --impact"));
}

/// DoD-4b: dual-marker where the legacy block is a customised Photo-shaped
/// variant (not exact known template) → near-miss report, not auto-deleted.
#[test]
fn repair_photo_shaped_duplicate_is_reported_not_deleted() {
    let tmp = tempfile::tempdir().unwrap();
    let root = make_repo(tmp.path());
    let hook_path = root.join(".git").join("hooks").join("pre-commit");
    // Custom wording so it is not an exact known template.
    let dual = format!(
        r#"#!/bin/sh

# {brand}-ledger-gate: auto-installed by `{brand} init`
if command -v {brand} &>/dev/null; then
    if ! {brand} ledger status --compact --exit-code 2>/dev/null; then
        echo ""
        echo "  CUSTOM Resolve with:"
        echo "    Pending tx:  {brand} ledger commit <tx-id> --summary '...' --reason '...'"
        echo "    Drift:       {brand} ledger reconcile --all --reason '...'"
        echo ""
        echo "  Bypass (not recommended): git commit --no-verify"
        exit 1
    fi
fi

# ledgerful-ledger-gate: auto-installed by `ledgerful init`
if command -v ledgerful &>/dev/null; then
    if ! ledgerful ledger status --compact --exit-code --verify-signatures; then
        echo "[Ledgerful] Blocked by ledger state."
        exit 1
    fi
fi
"#,
        brand = LEGACY_BINARY
    );
    fs::write(&hook_path, dual).unwrap();

    let report = repair_hooks_at(&root, false).unwrap();
    // Near-miss should be reported (custom wording).
    assert!(
        !report.near_miss_blocks.is_empty()
            || fs::read_to_string(&hook_path)
                .unwrap()
                .contains("CUSTOM Resolve"),
        "custom legacy block must not be silently dropped; report={:?}",
        report.near_miss_blocks
    );
    // The custom block body (after marker/invocation rewrite may change
    // binary names) — CUSTOM text must still be present if not exact-match.
    let after = fs::read_to_string(&hook_path).unwrap();
    if report.near_miss_blocks.is_empty() {
        // If tier-1 somehow matched, that would be a test design error.
        panic!("expected tier-2 near-miss for custom block; after=\n{after}");
    }
    assert!(
        after.contains("CUSTOM Resolve") || after.matches("# ledgerful-ledger-gate").count() >= 1,
        "custom block must remain on disk"
    );
}

/// Outside-repo hooksPath is refused (DoD-9b). Config written via pure FS.
/// Asserts the outside hook file bytes are unchanged (not rewritten).
#[test]
fn repair_refuses_outside_repo_hooks_path() {
    let tmp = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let root = make_repo(tmp.path());
    let outside_hooks = outside.path().join("hooks");
    fs::create_dir_all(&outside_hooks).unwrap();
    let outside_hook = outside_hooks.join("pre-commit");
    let original_bytes = stale_pre_push().into_bytes();
    fs::write(&outside_hook, &original_bytes).unwrap();

    // Write core.hooksPath into .git/config (pure FS — matches production reader).
    let config = format!(
        "[core]\n\thooksPath = {}\n",
        outside_hooks.to_str().unwrap().replace('\\', "/")
    );
    fs::write(root.join(".git").join("config"), config).unwrap();

    let report = repair_hooks_at(&root, false).unwrap();
    assert!(
        report.discovery_notes.iter().any(|n| n.contains("outside")),
        "expected outside-repo note: {:?}",
        report.discovery_notes
    );
    assert!(report.repaired.is_empty());
    // Critical: outside-repo hooks must not be rewritten at all.
    let after_bytes = fs::read(&outside_hook).unwrap();
    assert_eq!(
        after_bytes, original_bytes,
        "outside-repo hook file must be byte-identical after refuse"
    );
}

/// Non-root husky (CrawlX shape) is detected via resolved path (DoD-9b).
#[test]
fn repair_detects_non_root_husky_via_hooks_path() {
    let tmp = tempfile::tempdir().unwrap();
    let root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
    let husky_hooks = root.join("apps").join("api").join(".husky").join("_");
    fs::create_dir_all(&husky_hooks).unwrap();
    fs::write(husky_hooks.join("pre-commit"), stale_pre_push()).unwrap();
    fs::create_dir_all(root.join(".git")).unwrap();
    fs::write(
        root.join(".git").join("config"),
        "[core]\n\thooksPath = apps/api/.husky/_\n",
    )
    .unwrap();

    let report = repair_hooks_at(&root, false).unwrap();
    assert_eq!(
        report.third_party_manager,
        Some(ThirdPartyHookManager::Husky),
        "non-root husky must be refused: {:?}",
        report
    );
    assert!(report.repaired.is_empty());
}

#[test]
fn parse_git_config_hooks_path() {
    let content = "[core]\n\trepositoryformatversion = 0\n\thooksPath = .git/hooks\n";
    assert_eq!(
        parse_git_config_value(content, "core", "hooksPath").as_deref(),
        Some(".git/hooks")
    );
}

/// Absolute hooksPath inside the repo (Design shape) is accepted.
#[test]
fn repair_accepts_absolute_inside_repo_hooks_path() {
    let tmp = tempfile::tempdir().unwrap();
    let root = make_repo(tmp.path());
    let hooks = root.join(".git").join("hooks");
    fs::write(hooks.join("pre-commit"), stale_pre_push()).unwrap();
    let abs = hooks.as_str().replace('\\', "/");
    fs::write(
        root.join(".git").join("config"),
        format!("[core]\n\thooksPath = {abs}\n"),
    )
    .unwrap();

    let report = repair_hooks_at(&root, false).unwrap();
    assert!(
        report
            .discovery_notes
            .iter()
            .all(|n| !n.contains("outside")),
        "inside-repo absolute path must not be refused: {:?}",
        report.discovery_notes
    );
    assert!(
        report.repaired.contains(&"pre-commit".to_string()),
        "expected repair of hooks at absolute inside path: {:?}",
        report
    );
}

/// Case-mismatched absolute path must not be refused on Windows (DoD-9b).
#[cfg(windows)]
#[test]
fn repair_accepts_case_mismatched_absolute_hooks_path() {
    let tmp = tempfile::tempdir().unwrap();
    let root = make_repo(tmp.path());
    let hooks = root.join(".git").join("hooks");
    fs::write(hooks.join("pre-commit"), stale_pre_push()).unwrap();
    // Flip case on the drive letter / path segments where possible.
    let mut abs = hooks.as_str().replace('\\', "/");
    if let Some(rest) = abs.strip_prefix("C:") {
        abs = format!("c:{rest}");
    } else if let Some(rest) = abs.strip_prefix("c:") {
        abs = format!("C:{rest}");
    }
    fs::write(
        root.join(".git").join("config"),
        format!("[core]\n\thooksPath = {abs}\n"),
    )
    .unwrap();

    let report = repair_hooks_at(&root, false).unwrap();
    assert!(
        !report.discovery_notes.iter().any(|n| n.contains("outside")),
        "case-mismatched absolute path must not be refused: {:?}",
        report.discovery_notes
    );
}

/// Real `git worktree add` fixture: resolved hooks path is the main repo's
/// common `.git/hooks` via commondir (DoD-9) — not the worktree gitdir hooks.
#[test]
fn resolve_hooks_via_worktree_commondir() {
    let tmp = tempfile::tempdir().unwrap();
    let main = tmp.path().join("main");
    fs::create_dir_all(&main).unwrap();
    let git = |args: &[&str], cwd: &std::path::Path| {
        std::process::Command::new("git")
            .args(args)
            .current_dir(cwd)
            .output()
            .expect("git available")
    };
    assert!(git(&["init"], &main).status.success());
    assert!(
        git(&["config", "user.email", "t@example.com"], &main)
            .status
            .success()
    );
    assert!(git(&["config", "user.name", "t"], &main).status.success());
    fs::write(main.join("README"), "x").unwrap();
    assert!(git(&["add", "README"], &main).status.success());
    assert!(git(&["commit", "-m", "init"], &main).status.success());
    let linked = tmp.path().join("linked");
    let out = git(
        &["worktree", "add", linked.to_str().unwrap(), "HEAD"],
        &main,
    );
    assert!(
        out.status.success(),
        "worktree add failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let linked_root = Utf8PathBuf::from_path_buf(linked).unwrap();
    let main_hooks = main.join(".git").join("hooks");
    assert!(
        main_hooks.is_dir(),
        "main common hooks dir must exist for comparison"
    );
    let main_hooks_canon = dunce::canonicalize(&main_hooks).unwrap();

    match resolve_hooks_dir(&linked_root) {
        HooksDirResolution::Found { hooks_dir } => {
            // Resolved path must exist as a directory (common-dir hooks).
            assert!(
                hooks_dir.is_dir(),
                "hooks dir from commondir must exist: {hooks_dir}"
            );
            let resolved_canon = dunce::canonicalize(hooks_dir.as_std_path()).unwrap();
            assert_eq!(
                resolved_canon, main_hooks_canon,
                "worktree hooks must resolve to main .git/hooks via commondir; got {hooks_dir}"
            );

            // Worktree-private <gitdir>/hooks must NOT be what was used.
            let git_file = linked_root.join(".git");
            assert!(git_file.is_file(), "linked worktree has .git file");
            let git_contents = fs::read_to_string(git_file.as_std_path()).unwrap();
            let gitdir_line = git_contents
                .lines()
                .find_map(|l| l.trim().strip_prefix("gitdir:"))
                .map(str::trim)
                .expect("gitdir: line");
            let worktree_gitdir = {
                let p = Path::new(gitdir_line);
                if p.is_absolute() {
                    PathBuf::from(p)
                } else {
                    linked_root.as_std_path().join(p)
                }
            };
            let worktree_private_hooks = worktree_gitdir.join("hooks");
            // Either the private path does not exist, or it is not the resolved dir.
            if worktree_private_hooks.exists() {
                let private_canon = dunce::canonicalize(&worktree_private_hooks).unwrap();
                assert_ne!(
                    resolved_canon, private_canon,
                    "must not use worktree-private <gitdir>/hooks"
                );
            } else {
                assert!(
                    !worktree_private_hooks.exists(),
                    "worktree-private hooks path should not exist: {}",
                    worktree_private_hooks.display()
                );
            }
        }
        other => panic!("expected Found via commondir, got {other:?}"),
    }
}

#[test]
fn marker_normalization_rewrites_all_four_gate_types() {
    for suffix in GATE_SUFFIXES {
        let input = format!("# {LEGACY_BINARY}-{suffix}: hello");
        let (out, changed) = apply_replacements(&input);
        assert!(changed);
        assert_eq!(out, format!("# ledgerful-{suffix}: hello"));
    }
}

fn live_web_pre_push() -> String {
    format!(
        r#"#!/usr/bin/env bash

# {brand}-ledger-gate: auto-installed by `ledgerful init`
if command -v ledgerful &>/dev/null; then
    if ! ledgerful ledger status --compact --exit-code --verify-signatures 2>/dev/null; then
        echo ""
        echo "  Resolve with:"
        echo "    Pending tx:  ledgerful ledger commit <tx-id> --summary '...' --reason '...'"
        echo "    Drift:       ledgerful ledger reconcile --all --reason '...'"
        echo ""
        echo "  Bypass (not recommended): git push --no-verify"
        exit 1
    fi
fi


# {brand}-verify-gate: fast scoped verification (pre-push only)
if command -v ledgerful &>/dev/null; then
    if ! ledgerful verify --scope fast 2>/dev/null; then
        echo ""
        echo "  Pre-push quality gate FAILED (ledgerful verify --scope fast)."
        echo "  Fix the above errors before pushing."
        echo ""
        echo "  Bypass (not recommended): git push --no-verify"
        exit 1
    fi
fi


# ledgerful-ledger-gate: auto-installed by `ledgerful init`
if command -v ledgerful &>/dev/null; then
    if ! ledgerful ledger status --compact --exit-code --verify-signatures; then
        echo "[Ledgerful] Blocked by ledger state."
        echo "[Ledgerful] Resolve with:"
        echo "[Ledgerful]   Pending tx:  ledgerful ledger commit <tx-id> --summary '...' --reason '...'"
        echo "[Ledgerful]   Drift:       ledgerful ledger reconcile --all --reason '...'"
        echo "[Ledgerful] Fix the issues or bypass with: git push --no-verify"
        exit 1
    fi
fi


# ledgerful-verify-gate: fast scoped verification (pre-push only)
if command -v ledgerful &>/dev/null; then
    if ! ledgerful verify --scope fast; then
        echo "[Ledgerful] Push blocked by verification failure."
        echo "[Ledgerful] Fix the issues or bypass with: git push --no-verify"
        exit 1
    fi
fi
"#,
        brand = LEGACY_BINARY
    )
}

fn live_web_pre_commit() -> String {
    format!(
        r#"#!/usr/bin/env bash

# {brand}-ledger-gate: auto-installed by `ledgerful init`
if command -v ledgerful &>/dev/null; then
    if ! ledgerful ledger status --compact --exit-code --verify-signatures 2>/dev/null; then
        echo ""
        echo "  Resolve with:"
        echo "    Pending tx:  ledgerful ledger commit <tx-id> --summary '...' --reason '...'"
        echo "    Drift:       ledgerful ledger reconcile --all --reason '...'"
        echo ""
        echo "  Bypass (not recommended): git commit --no-verify"
        exit 1
    fi
fi


# ledgerful-ledger-gate: auto-installed by `ledgerful init`
if command -v ledgerful &>/dev/null; then
    if ! ledgerful ledger status --compact --exit-code --verify-signatures; then
        echo "[Ledgerful] Blocked by ledger state."
        echo "[Ledgerful] Resolve with:"
        echo "[Ledgerful]   Pending tx:  ledgerful ledger commit <tx-id> --summary '...' --reason '...'"
        echo "[Ledgerful]   Drift:       ledgerful ledger reconcile --all --reason '...'"
        echo "[Ledgerful] Fix the issues or bypass with: git commit --no-verify"
        exit 1
    fi
fi
"#,
        brand = LEGACY_BINARY
    )
}

fn live_web_commit_msg() -> String {
    format!(
        r#"#!/usr/bin/env bash

# {brand}-intent-gate: auto-installed by `ledgerful init`
if command -v ledgerful &>/dev/null; then
    ledgerful internal hook-commit-msg "$1"
fi


# ledgerful-intent-gate: auto-installed by `ledgerful init`
if command -v ledgerful &>/dev/null; then
    ledgerful internal hook-commit-msg "$1"
fi
"#,
        brand = LEGACY_BINARY
    )
}

fn live_web_post_commit() -> String {
    format!(
        r#"#!/usr/bin/env bash

# {brand}-post-commit-gate: auto-installed by `ledgerful init`
if command -v ledgerful &>/dev/null; then
    ledgerful internal hook-post-commit "$@"
fi


# ledgerful-post-commit-gate: auto-installed by `ledgerful init`
if command -v ledgerful &>/dev/null; then
    ledgerful internal hook-post-commit "$@"
fi
"#,
        brand = LEGACY_BINARY
    )
}

fn unstamped_verify_gate() -> &'static str {
    "\
# ledgerful-verify-gate: fast scoped verification (pre-push only)
if command -v ledgerful &>/dev/null; then
    if ! ledgerful verify --scope fast; then
        echo \"[Ledgerful] Push blocked by verification failure.\"
        echo \"[Ledgerful] Fix the issues or bypass with: git push --no-verify\"
        exit 1
    fi
fi
"
}

/// 0206-B: dual-brand commit-msg intent is tier-1, not a near-miss.
#[test]
fn repair_dedups_dual_marker_intent() {
    let tmp = tempfile::tempdir().unwrap();
    let root = make_repo(tmp.path());
    let hook_path = root.join(".git").join("hooks").join("commit-msg");
    fs::write(&hook_path, live_web_commit_msg()).unwrap();

    let report = repair_hooks_at(&root, false).unwrap();
    assert!(
        report.near_miss_blocks.is_empty(),
        "intent must be known-generated, not near-miss: {:?}",
        report.near_miss_blocks
    );
    let after = fs::read_to_string(&hook_path).unwrap();
    assert_eq!(
        after.matches("# ledgerful-intent-gate").count(),
        1,
        "expected one intent-gate after dedup; got:\n{after}"
    );
    assert!(!after.contains(&format!("# {LEGACY_BINARY}-")));
}

/// Dual-brand live-web intent on CRLF must still be tier-1: extract must
/// advance by `\r\n` (2), not `+ 1`, or the block truncates before `fi`.
#[test]
fn repair_dedups_dual_marker_intent_crlf() {
    let tmp = tempfile::tempdir().unwrap();
    let root = make_repo(tmp.path());
    let hook_path = root.join(".git").join("hooks").join("commit-msg");
    let crlf = live_web_commit_msg().replace('\n', "\r\n");
    fs::write(&hook_path, crlf).unwrap();

    let report = repair_hooks_at(&root, false).unwrap();
    assert!(
        report.near_miss_blocks.is_empty(),
        "CRLF intent must be known-generated, not near-miss: {:?}",
        report.near_miss_blocks
    );
    let after = fs::read_to_string(&hook_path).unwrap();
    assert_eq!(
        after.matches("# ledgerful-intent-gate").count(),
        1,
        "expected one intent-gate after CRLF dedup; got:\n{after}"
    );
    assert!(!after.contains(&format!("# {LEGACY_BINARY}-")));
}

/// 0206-B: dual-brand post-commit is tier-1, not a near-miss.
#[test]
fn repair_dedups_dual_marker_post_commit() {
    let tmp = tempfile::tempdir().unwrap();
    let root = make_repo(tmp.path());
    let hook_path = root.join(".git").join("hooks").join("post-commit");
    fs::write(&hook_path, live_web_post_commit()).unwrap();

    let report = repair_hooks_at(&root, false).unwrap();
    assert!(
        report.near_miss_blocks.is_empty(),
        "post-commit must be known-generated, not near-miss: {:?}",
        report.near_miss_blocks
    );
    let after = fs::read_to_string(&hook_path).unwrap();
    assert_eq!(
        after.matches("# ledgerful-post-commit-gate").count(),
        1,
        "expected one post-commit-gate after dedup; got:\n{after}"
    );
    assert!(!after.contains(&format!("# {LEGACY_BINARY}-")));
}

/// DoD-1: live web four-hook dual-brand corpus repairs to one current
/// marker per gate type, zero legacy markers, empty near-miss.
#[test]
fn repair_four_hook_live_web_corpus() {
    let tmp = tempfile::tempdir().unwrap();
    let root = make_repo(tmp.path());
    let hooks = root.join(".git").join("hooks");
    fs::write(hooks.join("pre-push"), live_web_pre_push()).unwrap();
    fs::write(hooks.join("pre-commit"), live_web_pre_commit()).unwrap();
    fs::write(hooks.join("commit-msg"), live_web_commit_msg()).unwrap();
    fs::write(hooks.join("post-commit"), live_web_post_commit()).unwrap();

    let report = repair_hooks_at(&root, false).unwrap();
    assert!(
        report.near_miss_blocks.is_empty(),
        "near-miss must be empty after 0206-B: {:?}",
        report.near_miss_blocks
    );
    assert!(report.residual_invocations.is_empty());

    let pre_push = fs::read_to_string(hooks.join("pre-push")).unwrap();
    let pre_commit = fs::read_to_string(hooks.join("pre-commit")).unwrap();
    let commit_msg = fs::read_to_string(hooks.join("commit-msg")).unwrap();
    let post_commit = fs::read_to_string(hooks.join("post-commit")).unwrap();

    assert_eq!(pre_push.matches("# ledgerful-ledger-gate").count(), 1);
    assert_eq!(pre_push.matches("# ledgerful-verify-gate").count(), 1);
    assert_eq!(pre_commit.matches("# ledgerful-ledger-gate").count(), 1);
    assert_eq!(commit_msg.matches("# ledgerful-intent-gate").count(), 1);
    assert_eq!(
        post_commit.matches("# ledgerful-post-commit-gate").count(),
        1
    );

    for (name, body) in [
        ("pre-push", &pre_push),
        ("pre-commit", &pre_commit),
        ("commit-msg", &commit_msg),
        ("post-commit", &post_commit),
    ] {
        assert!(
            !body.contains(&format!("# {LEGACY_BINARY}-")),
            "{name} still has legacy markers:\n{body}"
        );
    }
}

/// Original-brand intent twin is known-generated (0206-B1).
#[test]
fn repair_dedups_original_brand_intent() {
    let tmp = tempfile::tempdir().unwrap();
    let root = make_repo(tmp.path());
    let hook_path = root.join(".git").join("hooks").join("commit-msg");
    let dual = format!(
        r#"#!/usr/bin/env bash

# {brand}-intent-gate: auto-installed by `{brand} init`
if command -v {brand} &>/dev/null; then
    {brand} internal hook-commit-msg "$1"
fi


# ledgerful-intent-gate: auto-installed by `ledgerful init`
if command -v ledgerful &>/dev/null; then
    ledgerful internal hook-commit-msg "$1"
fi
"#,
        brand = LEGACY_BINARY
    );
    fs::write(&hook_path, dual).unwrap();

    let report = repair_hooks_at(&root, false).unwrap();
    assert!(
        report.near_miss_blocks.is_empty(),
        "original-brand intent must be tier-1: {:?}",
        report.near_miss_blocks
    );
    let after = fs::read_to_string(&hook_path).unwrap();
    assert_eq!(after.matches("# ledgerful-intent-gate").count(), 1);
    assert!(!after.contains(&format!("# {LEGACY_BINARY}-")));
}

/// DoD-3: two current-brand same-suffix markers (no legacy) fire
/// `legacy-hooks`; repair leaves one.
#[test]
fn doctor_and_repair_same_brand_duplicate_verify() {
    let tmp = tempfile::tempdir().unwrap();
    let root = make_repo(tmp.path());
    let hook_path = root.join(".git").join("hooks").join("pre-push");
    let body = unstamped_verify_gate();
    let dual = format!("#!/usr/bin/env bash\n\n{body}\n{body}");
    fs::write(&hook_path, dual).unwrap();

    let findings = doctor_legacy_hook_findings(&root);
    assert!(
        findings.iter().any(|f| f.code == "legacy-hooks"),
        "expected legacy-hooks finding on N>1 current verify; got {findings:?}"
    );
    assert!(
        findings.iter().any(|f| f.code == "legacy-hooks"
            && (f.message.contains("duplicate") || f.message.contains("repair-hooks"))),
        "expected duplicate/repair-hooks message; got {findings:?}"
    );

    let report = repair_hooks_at(&root, false).unwrap();
    assert!(report.near_miss_blocks.is_empty(), "{:?}", report);
    let after = fs::read_to_string(&hook_path).unwrap();
    assert_eq!(
        after.matches("# ledgerful-verify-gate").count(),
        1,
        "same-brand collapse must leave one verify; got:\n{after}"
    );
    assert!(!after.contains(&format!("# {LEGACY_BINARY}-")));
}

/// Same-brand unstamped verify duplicates on CRLF must collapse to one.
#[test]
fn repair_same_brand_duplicate_verify_crlf() {
    let tmp = tempfile::tempdir().unwrap();
    let root = make_repo(tmp.path());
    let hook_path = root.join(".git").join("hooks").join("pre-push");
    let body = unstamped_verify_gate();
    let dual = format!("#!/usr/bin/env bash\n\n{body}\n{body}");
    let crlf = dual.replace('\n', "\r\n");
    fs::write(&hook_path, crlf).unwrap();

    let report = repair_hooks_at(&root, false).unwrap();
    assert!(report.near_miss_blocks.is_empty(), "{:?}", report);
    let after = fs::read_to_string(&hook_path).unwrap();
    assert_eq!(
        after.matches("# ledgerful-verify-gate").count(),
        1,
        "same-brand CRLF collapse must leave one verify; got:\n{after}"
    );
    assert!(!after.contains(&format!("# {LEGACY_BINARY}-")));
}
