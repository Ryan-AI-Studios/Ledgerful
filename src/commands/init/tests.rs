use super::hooks::{install_git_hook, install_pre_push_verify_block};
use crate::commands::hook_repair::LEGACY_BINARY;
use crate::commands::hook_template::{LEDGER_GATE_MARKER, VERIFY_GATE_MARKER};
use camino::Utf8PathBuf;
use std::fs;

fn make_repo(tmp: &std::path::Path) -> Utf8PathBuf {
    let root = Utf8PathBuf::from_path_buf(tmp.to_path_buf()).unwrap();
    fs::create_dir_all(root.join(".git").join("hooks")).unwrap();
    root
}

fn fully_legacy_pre_push() -> String {
    format!(
        r#"#!/usr/bin/env bash

# {brand}-ledger-gate: auto-installed by `{brand} init`
if command -v {brand} &>/dev/null; then
    if ! {brand} ledger status --compact --exit-code 2>/dev/null; then
        echo ""
        echo "  Resolve with:"
        echo "    Pending tx:  {brand} ledger commit <tx-id> --summary '...' --reason '...'"
        echo "    Drift:       {brand} ledger reconcile --all --reason '...'"
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
"#,
        brand = LEGACY_BINARY
    )
}

fn mixed_current_ledger_legacy_verify() -> String {
    format!(
        r#"#!/usr/bin/env bash

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
"#,
        brand = LEGACY_BINARY
    )
}

/// DoD-2a: fully-legacy pre-push (ledger+verify) after install must
/// yield one current ledger + one verify, not two brands.
#[test]
fn install_does_not_append_beside_fully_legacy_pre_push() {
    let tmp = tempfile::tempdir().unwrap();
    let root = make_repo(tmp.path());
    let hook_path = root.join(".git").join("hooks").join("pre-push");
    fs::write(&hook_path, fully_legacy_pre_push()).unwrap();

    install_git_hook(&root, "pre-push", "git push --no-verify").unwrap();
    install_pre_push_verify_block(&root).unwrap();

    let after = fs::read_to_string(hook_path.as_std_path()).unwrap();
    assert_eq!(
        after.matches(LEDGER_GATE_MARKER).count(),
        1,
        "expected one ledger-gate; got:\n{after}"
    );
    assert_eq!(
        after.matches(VERIFY_GATE_MARKER).count(),
        1,
        "expected one verify-gate; got:\n{after}"
    );
    assert!(
        !after.contains(&format!("# {LEGACY_BINARY}-")),
        "legacy markers must be rewritten, not left beside a new pair:\n{after}"
    );
}

/// DoD-2b / 0206-A1: current ledger + leftover legacy verify + no current
/// verify must not append a second verify.
#[test]
fn install_does_not_append_verify_beside_legacy_verify() {
    let tmp = tempfile::tempdir().unwrap();
    let root = make_repo(tmp.path());
    let hook_path = root.join(".git").join("hooks").join("pre-push");
    fs::write(&hook_path, mixed_current_ledger_legacy_verify()).unwrap();

    install_git_hook(&root, "pre-push", "git push --no-verify").unwrap();
    install_pre_push_verify_block(&root).unwrap();

    let after = fs::read_to_string(hook_path.as_std_path()).unwrap();
    assert_eq!(
        after.matches(LEDGER_GATE_MARKER).count(),
        1,
        "expected one ledger-gate; got:\n{after}"
    );
    assert_eq!(
        after.matches(VERIFY_GATE_MARKER).count(),
        1,
        "expected one verify-gate; got:\n{after}"
    );
    assert!(
        !after.contains(&format!("# {LEGACY_BINARY}-")),
        "legacy verify must be aliased, not left beside a new verify:\n{after}"
    );
}

/// 0206-A2: fully-legacy pre-commit must go through ensure, never raw-append.
#[test]
fn install_does_not_raw_append_beside_legacy_pre_commit() {
    let tmp = tempfile::tempdir().unwrap();
    let root = make_repo(tmp.path());
    let hook_path = root.join(".git").join("hooks").join("pre-commit");
    let legacy = format!(
        r#"#!/usr/bin/env bash

# {brand}-ledger-gate: auto-installed by `{brand} init`
if command -v {brand} &>/dev/null; then
    if ! {brand} ledger status --compact --exit-code 2>/dev/null; then
        echo ""
        echo "  Bypass (not recommended): git commit --no-verify"
        exit 1
    fi
fi
"#,
        brand = LEGACY_BINARY
    );
    fs::write(&hook_path, legacy).unwrap();

    install_git_hook(&root, "pre-commit", "git commit --no-verify").unwrap();

    let after = fs::read_to_string(hook_path.as_std_path()).unwrap();
    assert_eq!(
        after.matches(LEDGER_GATE_MARKER).count(),
        1,
        "expected one ledger-gate; got:\n{after}"
    );
    assert!(
        !after.contains(&format!("# {LEGACY_BINARY}-")),
        "legacy marker must be rewritten:\n{after}"
    );
}
