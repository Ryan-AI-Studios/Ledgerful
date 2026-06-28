use crate::common::{DirGuard, non_interactive, setup_git_repo};
use camino::Utf8Path;
use ledgerful::commands::hook_repair::{ThirdPartyHookManager, repair_hooks_at};
use ledgerful::commands::init::execute_init;
use ledgerful::commands::update::execute_update;
use serial_test::serial;
use std::fs;
use tempfile::tempdir;

/// The exact real stale `pre-push` hook content this repo's `.git/hooks/pre-push`
/// contained before TA23 (auto-installed by an older `ledgerful init`).
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
    let retired = concat!("change", "guard");
    CURRENT_PRE_PUSH
        .replace("command -v ledgerful", &format!("command -v {retired}"))
        .replace("ledgerful ledger", &format!("{retired} ledger"))
        .replace("ledgerful verify", &format!("{retired} verify"))
}

#[test]
#[serial(env, cwd)]
fn repair_hooks_via_update_command_rewrites_stale_hook() {
    let _env_non_interactive = non_interactive();
    let tmp = tempdir().unwrap();
    let root = Utf8Path::from_path(tmp.path()).unwrap();

    setup_git_repo(tmp.path());
    let hooks_dir = root.join(".git").join("hooks");
    fs::create_dir_all(&hooks_dir).unwrap();
    fs::write(hooks_dir.join("pre-push"), stale_pre_push()).unwrap();

    let _guard = DirGuard::from_utf8(root);

    // migrate=false, binary=false, repair_hooks=true
    let result = execute_update(false, false, false, false, false, false, true);
    assert!(result.is_ok());

    let rewritten = fs::read_to_string(hooks_dir.join("pre-push")).unwrap();
    assert!(rewritten.contains("command -v ledgerful"));
    assert!(rewritten.contains("ledgerful ledger status"));
    assert!(rewritten.contains("ledgerful verify"));
    assert!(!rewritten.contains(concat!("change", "guard")));
    // Marker comments preserved.
    assert!(rewritten.contains("# ledgerful-ledger-gate"));
    assert!(rewritten.contains("# ledgerful-verify-gate"));
}

#[test]
#[serial(env, cwd)]
fn repair_hooks_dry_run_does_not_write() {
    let _env_non_interactive = non_interactive();
    let tmp = tempdir().unwrap();
    let root = Utf8Path::from_path(tmp.path()).unwrap();

    setup_git_repo(tmp.path());
    let hooks_dir = root.join(".git").join("hooks");
    fs::create_dir_all(&hooks_dir).unwrap();
    let stale = stale_pre_push();
    fs::write(hooks_dir.join("pre-push"), &stale).unwrap();

    let _guard = DirGuard::from_utf8(root);

    let result = execute_update(false, false, false, false, false, true, true);
    assert!(result.is_ok());

    let after = fs::read_to_string(hooks_dir.join("pre-push")).unwrap();
    assert_eq!(after, stale, "dry-run must not write changes");
}

#[test]
#[serial(env, cwd)]
fn repair_hooks_no_action_flags_prints_hint_not_error() {
    let _env_non_interactive = non_interactive();
    let tmp = tempdir().unwrap();
    let root = Utf8Path::from_path(tmp.path()).unwrap();
    setup_git_repo(tmp.path());
    let _guard = DirGuard::from_utf8(root);

    let result = execute_update(false, false, false, false, false, false, false);
    assert!(result.is_ok());
}

#[test]
#[serial(env, cwd)]
fn detect_husky_via_repair_hooks_at_skips_rewrite_and_names_manager() {
    let tmp = tempdir().unwrap();
    let root = Utf8Path::from_path(tmp.path()).unwrap();

    setup_git_repo(tmp.path());
    let hooks_dir = root.join(".git").join("hooks");
    fs::create_dir_all(&hooks_dir).unwrap();
    let stale = stale_pre_push();
    fs::write(hooks_dir.join("pre-push"), &stale).unwrap();
    fs::create_dir_all(root.join(".husky")).unwrap();

    let report = repair_hooks_at(root, false).unwrap();

    assert_eq!(
        report.third_party_manager,
        Some(ThirdPartyHookManager::Husky)
    );
    assert!(report.repaired.is_empty());

    let untouched = fs::read_to_string(hooks_dir.join("pre-push")).unwrap();
    assert_eq!(untouched, stale);
}

#[test]
#[serial(env, cwd)]
fn detect_lefthook_via_repair_hooks_at_skips_rewrite() {
    let tmp = tempdir().unwrap();
    let root = Utf8Path::from_path(tmp.path()).unwrap();

    setup_git_repo(tmp.path());
    let hooks_dir = root.join(".git").join("hooks");
    fs::create_dir_all(&hooks_dir).unwrap();
    let stale = stale_pre_push();
    fs::write(hooks_dir.join("pre-push"), &stale).unwrap();
    fs::write(root.join("lefthook.yml"), "pre-push:\n  commands:\n").unwrap();

    let report = repair_hooks_at(root, false).unwrap();

    assert_eq!(
        report.third_party_manager,
        Some(ThirdPartyHookManager::Lefthook)
    );
    assert!(report.repaired.is_empty());

    let untouched = fs::read_to_string(hooks_dir.join("pre-push")).unwrap();
    assert_eq!(untouched, stale);
}

#[test]
#[serial(env, cwd)]
fn init_generates_only_ledgerful_hook_commands() {
    let _env_non_interactive = non_interactive();
    let tmp = tempdir().unwrap();
    let root = Utf8Path::from_path(tmp.path()).unwrap();

    setup_git_repo(tmp.path());
    let _guard = DirGuard::from_utf8(root);

    execute_init(false).unwrap();

    for hook_name in ["pre-commit", "pre-push", "commit-msg", "post-commit"] {
        let content = fs::read_to_string(root.join(".git").join("hooks").join(hook_name)).unwrap();
        assert!(content.contains("ledgerful"));
        assert!(!content.contains(concat!("change", "guard")));
    }
}
