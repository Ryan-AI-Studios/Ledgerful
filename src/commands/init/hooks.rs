use crate::commands::hook_repair::{
    HooksDirResolution, contains_legacy_gate_suffix, resolve_hooks_dir,
};
use crate::commands::hook_template::{
    GateKind, INTENT_GATE_MARKER, LEDGER_GATE_MARKER, POST_COMMIT_GATE_MARKER,
    ensure_gate_on_hook_file, intent_gate_block, ledger_gate_block, post_commit_gate_block,
};
use camino::{Utf8Path, Utf8PathBuf};
use miette::{IntoDiagnostic, Result};
use std::fs;
use std::io::Write as IoWrite;

/// Shared hooks directory for install (linked worktrees via commondir — not `root/.git/hooks`).
fn hooks_dir_for_install(root: &Utf8Path) -> Option<Utf8PathBuf> {
    match resolve_hooks_dir(root) {
        HooksDirResolution::Found { hooks_dir } => Some(hooks_dir),
        HooksDirResolution::OutsideRepo { .. } | HooksDirResolution::CannotLook { .. } => None,
    }
}

fn set_executable_unix(hook_path: &Utf8Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(hook_path.as_std_path())
            .into_diagnostic()?
            .permissions();
        perms.set_mode(0o755);
        fs::set_permissions(hook_path.as_std_path(), perms).into_diagnostic()?;
    }
    let _ = hook_path;
    Ok(())
}

/// Install or stamp-aware-upgrade the ledger-gate block on pre-commit / pre-push.
/// Uses shared ensure (0121) — no independent body-diff silent rewrite.
pub(super) fn install_git_hook(
    root: &Utf8PathBuf,
    hook_name: &str,
    bypass_command: &str,
) -> Result<bool> {
    let Some(hooks_dir) = hooks_dir_for_install(root) else {
        return Ok(false);
    };
    fs::create_dir_all(&hooks_dir).into_diagnostic()?;

    let hook_path = hooks_dir.join(hook_name);
    let hook_block = ledger_gate_block(bypass_command);

    if hook_path.exists() {
        let existing = fs::read_to_string(hook_path.as_std_path()).into_diagnostic()?;
        if existing.contains(LEDGER_GATE_MARKER)
            || contains_legacy_gate_suffix(&existing, GateKind::Ledger.as_str())
        {
            // Stamp-aware ensure: refresh stale product bodies only.
            // Same-suffix legacy marker counts as present (0206-A2) — never raw-append.
            let _ = ensure_gate_on_hook_file(&hook_path, GateKind::Ledger, bypass_command, false)?;
            return Ok(false);
        }
        // Append to existing hook
        let mut file = fs::OpenOptions::new()
            .append(true)
            .open(hook_path.as_std_path())
            .into_diagnostic()?;
        let block = format!("\n{hook_block}");
        file.write_all(block.as_bytes()).into_diagnostic()?;
    } else {
        let content = format!("#!/usr/bin/env bash\n\n{hook_block}");
        fs::write(hook_path.as_std_path(), content).into_diagnostic()?;
        set_executable_unix(&hook_path)?;
    }

    Ok(true)
}

fn install_commit_msg_hook(root: &Utf8PathBuf) -> Result<bool> {
    let Some(hooks_dir) = hooks_dir_for_install(root) else {
        return Ok(false);
    };
    fs::create_dir_all(&hooks_dir).into_diagnostic()?;

    let hook_path = hooks_dir.join("commit-msg");
    let template = intent_gate_block();

    if hook_path.exists() {
        let existing = fs::read_to_string(hook_path.as_std_path()).into_diagnostic()?;
        if existing.contains(INTENT_GATE_MARKER)
            || contains_legacy_gate_suffix(&existing, GateKind::Intent.as_str())
        {
            let _ = ensure_gate_on_hook_file(
                &hook_path,
                GateKind::Intent,
                "git commit --no-verify",
                false,
            )?;
            return Ok(false);
        }
        let mut file = fs::OpenOptions::new()
            .append(true)
            .open(hook_path.as_std_path())
            .into_diagnostic()?;
        let block = format!("\n{template}");
        file.write_all(block.as_bytes()).into_diagnostic()?;
    } else {
        let content = format!("#!/usr/bin/env bash\n\n{template}");
        fs::write(hook_path.as_std_path(), content).into_diagnostic()?;
        set_executable_unix(&hook_path)?;
    }

    Ok(true)
}

fn install_post_commit_hook(root: &Utf8PathBuf) -> Result<bool> {
    let Some(hooks_dir) = hooks_dir_for_install(root) else {
        return Ok(false);
    };
    fs::create_dir_all(&hooks_dir).into_diagnostic()?;

    let hook_path = hooks_dir.join("post-commit");
    let template = post_commit_gate_block();

    if hook_path.exists() {
        let existing = fs::read_to_string(hook_path.as_std_path()).into_diagnostic()?;
        if existing.contains(POST_COMMIT_GATE_MARKER)
            || contains_legacy_gate_suffix(&existing, GateKind::PostCommit.as_str())
        {
            let _ = ensure_gate_on_hook_file(
                &hook_path,
                GateKind::PostCommit,
                "git commit --no-verify",
                false,
            )?;
            return Ok(false);
        }
        let mut file = fs::OpenOptions::new()
            .append(true)
            .open(hook_path.as_std_path())
            .into_diagnostic()?;
        let block = format!("\n{template}");
        file.write_all(block.as_bytes()).into_diagnostic()?;
    } else {
        let content = format!("#!/usr/bin/env bash\n\n{template}");
        fs::write(hook_path.as_std_path(), content).into_diagnostic()?;
        set_executable_unix(&hook_path)?;
    }

    Ok(true)
}

pub(super) fn install_ledger_gate_hooks(root: &Utf8PathBuf) -> Result<Vec<&'static str>> {
    // Skip if a third-party hook manager owns this repo's hooks
    if let Some(manager) =
        crate::commands::hook_repair::detect_third_party_hook_manager(root.as_path())
    {
        eprintln!(
            "INFO: Third-party hook manager '{}' detected. Skipping Ledgerful hook installation. Configure {} to call `ledgerful`.",
            manager.name(),
            manager.name()
        );
        return Ok(vec![]);
    }

    let mut installed = Vec::new();

    if install_git_hook(root, "pre-commit", "git commit --no-verify")? {
        installed.push("pre-commit");
    }

    if install_git_hook(root, "pre-push", "git push --no-verify")? {
        installed.push("pre-push");
    }
    // Append the fast scoped verify gate to the pre-push hook. Idempotent:
    // skips if the verify marker is already present.
    install_pre_push_verify_block(root)?;

    if install_commit_msg_hook(root)? {
        installed.push("commit-msg");
    }

    if install_post_commit_hook(root)? {
        installed.push("post-commit");
    }

    Ok(installed)
}

/// Append or stamp-aware-refresh the fast scoped verify block on pre-push.
/// Shared ensure (0121) supersedes the prior silent body-diff rewrite.
pub(super) fn install_pre_push_verify_block(root: &Utf8PathBuf) -> Result<()> {
    let Some(hooks_dir) = hooks_dir_for_install(root) else {
        return Ok(());
    };
    let hook_path = hooks_dir.join("pre-push");
    if !hook_path.exists() {
        return Ok(());
    }
    let _ = ensure_gate_on_hook_file(
        &hook_path,
        GateKind::Verify,
        "git push --no-verify",
        true, // append if marker absent
    )?;
    Ok(())
}
