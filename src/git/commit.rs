use std::path::Path;
use std::process::Command;

use miette::Diagnostic;
use thiserror::Error;

use crate::platform::process_policy::{ProcessPolicy, check_policy};

/// Error variants for git commit failures, mapped from git stderr output.
#[derive(Debug, Error, Diagnostic)]
pub enum GitCommitError {
    #[error("Nothing to commit. Stage files with `git add` first.")]
    #[diagnostic(help("Use `git add <files>` to stage changes before committing."))]
    NothingToCommit,

    #[error("Pre-commit hook failed with exit code {exit_code}")]
    #[diagnostic(help("Fix the issues reported by the hook and try again."))]
    PreCommitHookFailed { exit_code: i32, stderr: String },

    #[error("A merge is in progress. Complete or abort the merge before committing.")]
    #[diagnostic(help("Run `git merge --continue` or `git merge --abort`."))]
    MergeInProgress,

    #[error("Unresolved conflicts remain. Resolve them before committing.")]
    #[diagnostic(help("Use `git status` to see conflicted files."))]
    ConflictsRemaining,

    #[error("GPG signing failed")]
    #[diagnostic(help("Check your GPG configuration with `gpg --list-secret-keys`."))]
    GpgSigningFailed,

    #[error("Git commit failed: {stderr}")]
    Other { stderr: String },
}

/// Error variants for git state checks (non-fatal, advisory).
#[derive(Debug, Error, Diagnostic)]
pub enum GitStateError {
    #[error("Merge in progress")]
    MergeInProgress,

    #[error("Unresolved conflicts")]
    ConflictsRemaining,

    #[error("Failed to run git command: {0}")]
    CommandFailed(String),
}

/// Validate a `GIT_BINARY` override: **absolute** path whose basename is
/// `git` / `git.exe` (case-insensitive). Bare names and relative paths are
/// rejected (they enable PATH redirection) with warn + fallback to plain
/// `"git"`. Unset env continues to use PATH `"git"` intentionally.
pub fn validate_git_binary(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return "git".to_string();
    }

    let path = Path::new(trimmed);
    if !path.is_absolute() {
        tracing::warn!(
            "GIT_BINARY override {trimmed:?} rejected (must be an absolute path to git/git.exe; bare names enable PATH redirection); falling back to \"git\""
        );
        return "git".to_string();
    }

    let base = path.file_name().and_then(|s| s.to_str()).unwrap_or(trimmed);

    let base_ok = base.eq_ignore_ascii_case("git") || base.eq_ignore_ascii_case("git.exe");
    if !base_ok {
        tracing::warn!(
            "GIT_BINARY override {trimmed:?} rejected (basename must be git/git.exe); falling back to \"git\""
        );
        return "git".to_string();
    }

    trimmed.to_string()
}

/// Returns the path to the git binary, respecting a validated `GIT_BINARY`
/// env override. Invalid overrides warn and fall back to `"git"`.
pub fn git_binary() -> String {
    match std::env::var("GIT_BINARY") {
        Ok(value) => validate_git_binary(&value),
        Err(_) => "git".to_string(),
    }
}

/// Env vars that redirect git's own execution surface (admin-trusted-env RCE
/// class). Stripped from this binary's internal git subprocesses.
const GIT_ENV_STRIP: &[&str] = &[
    "GIT_EXEC_PATH",
    "GIT_CONFIG_COUNT",
    "GIT_CONFIG_PARAMETERS",
    "GIT_SSH_COMMAND",
    "GIT_SSH",
];

/// Apply env hardening to an internal git `Command`: strip execution-altering
/// git env vars and any dynamic `GIT_CONFIG_KEY_*` / `GIT_CONFIG_VALUE_*` pairs.
pub fn harden_git_env(cmd: &mut Command) {
    for key in GIT_ENV_STRIP {
        cmd.env_remove(key);
    }
    for (key, _) in std::env::vars() {
        if key.starts_with("GIT_CONFIG_KEY_") || key.starts_with("GIT_CONFIG_VALUE_") {
            cmd.env_remove(&key);
        }
    }
}

/// Build a hardened `Command` for this binary's own internal git invocations.
///
/// - Resolves `GIT_BINARY` via [`git_binary`]
/// - Strips execution-altering git env vars
/// - Passes `-c core.hooksPath=` so local hooks do not run for internal calls
/// - Applies a process-policy timeout env hint
pub fn git_command() -> Command {
    let binary = git_binary();
    let mut cmd = Command::new(binary);
    harden_git_env(&mut cmd);
    cmd.arg("-c").arg("core.hooksPath=");
    let policy = ProcessPolicy::default();
    if let Err(e) = check_policy("git", &policy) {
        tracing::warn!("Git command blocked by process policy: {}", e);
    }
    cmd.env(
        "CG_PROCESS_TIMEOUT",
        policy.default_timeout_secs.to_string(),
    );
    cmd
}

/// Check whether a git commit can proceed by inspecting repository state.
///
/// Returns `Ok(true)` if a commit can proceed, `Ok(false)` if there is nothing
/// staged, or an `Err(GitStateError)` if the repository is in a blocked state
/// (merge in progress, conflicts remaining).
pub fn can_commit() -> Result<bool, GitStateError> {
    // Check for merge in progress
    if git_rev_parse_merge_head_exists()? {
        return Err(GitStateError::MergeInProgress);
    }

    // Check for unresolved conflicts
    if has_unresolved_conflicts()? {
        return Err(GitStateError::ConflictsRemaining);
    }

    // Check if there are staged changes
    if !has_staged_changes()? {
        return Ok(false);
    }

    Ok(true)
}

fn git_rev_parse_merge_head_exists() -> Result<bool, GitStateError> {
    let output = git_command()
        .args(["rev-parse", "--git-path", "MERGE_HEAD"])
        .output()
        .map_err(|e| GitStateError::CommandFailed(format!("Failed to run git rev-parse: {}", e)))?;

    // If the command succeeds and produces output, parse the path
    if output.status.success() {
        let path_str = String::from_utf8_lossy(&output.stdout).trim().to_string();
        // Check if the file actually exists
        Ok(std::path::Path::new(&path_str).exists())
    } else {
        Ok(false)
    }
}

fn has_unresolved_conflicts() -> Result<bool, GitStateError> {
    let output = git_command()
        .args(["diff", "--name-only", "--diff-filter=U"])
        .output()
        .map_err(|e| GitStateError::CommandFailed(format!("Failed to run git diff: {}", e)))?;

    if output.status.success() {
        let files = String::from_utf8_lossy(&output.stdout).trim().to_string();
        Ok(!files.is_empty())
    } else {
        Ok(false)
    }
}

fn has_staged_changes() -> Result<bool, GitStateError> {
    let status = git_command()
        .args(["diff", "--cached", "--quiet"])
        .status()
        .map_err(|e| {
            GitStateError::CommandFailed(format!("Failed to run git diff --cached: {}", e))
        })?;

    // exit 0 = no differences (no staged changes) → return false
    // exit 1 = differences (staged changes) → return true
    match status.code() {
        Some(0) => Ok(false),
        Some(1) => Ok(true),
        Some(code) => Err(GitStateError::CommandFailed(format!(
            "git diff --cached --quiet exited with status {code}"
        ))),
        None => Err(GitStateError::CommandFailed(
            "git diff --cached --quiet terminated by signal".to_string(),
        )),
    }
}

/// Invoke `git commit` with the given message and optional signoff.
///
/// Shells out to the `git` binary (not libgit2) to preserve user hooks,
/// GPG signing, and `.gitconfig`. The message is passed via `-m` using
/// argv-based invocation (no shell string injection).
///
/// Note: for interactive user commits we still hardens env, but do **not**
/// clear hooksPath so the user's pre-commit hooks run as expected.
pub fn git_commit(message: &str, signoff: bool) -> Result<(), GitCommitError> {
    let binary = git_binary();
    let mut cmd = Command::new(&binary);
    harden_git_env(&mut cmd);
    // User-facing commit: preserve hooks (do not set core.hooksPath=).
    cmd.args(["commit", "-m", message]);

    if signoff {
        cmd.arg("--signoff");
    }

    let policy = ProcessPolicy::default();
    cmd.env(
        "CG_PROCESS_TIMEOUT",
        policy.default_timeout_secs.to_string(),
    );

    let output = cmd.output().map_err(|e| GitCommitError::Other {
        stderr: format!("Failed to execute git: {}", e),
    })?;

    if output.status.success() {
        return Ok(());
    }

    let exit_code = output.status.code();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    classify_git_error(&stderr, exit_code)
}

/// Classify git commit failure stderr into a typed GitCommitError.
fn classify_git_error(stderr: &str, exit_code: Option<i32>) -> Result<(), GitCommitError> {
    let stderr_lower = stderr.to_lowercase();

    if stderr_lower.contains("nothing to commit")
        || stderr_lower.contains("nothing added to commit")
    {
        return Err(GitCommitError::NothingToCommit);
    }

    if stderr_lower.contains("merge") && stderr_lower.contains("in progress") {
        return Err(GitCommitError::MergeInProgress);
    }

    if stderr_lower.contains("conflict") || stderr_lower.contains("unmerged") {
        return Err(GitCommitError::ConflictsRemaining);
    }

    if stderr_lower.contains("gpg") || stderr_lower.contains("signing failed") {
        return Err(GitCommitError::GpgSigningFailed);
    }

    if stderr_lower.contains("pre-commit") || stderr_lower.contains("hook") {
        return Err(GitCommitError::PreCommitHookFailed {
            exit_code: exit_code.unwrap_or(1),
            stderr: stderr.to_string(),
        });
    }

    Err(GitCommitError::Other {
        stderr: stderr.to_string(),
    })
}

/// Format a git commit message from a template.
///
/// Supported placeholders: `{category}`, `{summary}`, `{tx_id}`.
pub fn format_commit_message(template: &str, category: &str, summary: &str, tx_id: &str) -> String {
    template
        .replace("{category}", category)
        .replace("{summary}", summary)
        .replace("{tx_id}", tx_id)
}

/// Default commit message template used when no custom template is configured.
pub const DEFAULT_COMMIT_MESSAGE_TEMPLATE: &str = "[{category}] {summary}\n\nLedger: {tx_id}";

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    #[test]
    fn test_format_commit_message_default() {
        let msg = format_commit_message(
            DEFAULT_COMMIT_MESSAGE_TEMPLATE,
            "Feature",
            "Add interactive fix suggestions",
            "550e8400-e29b-41d4-a716-446655440000",
        );
        assert!(msg.contains("[Feature] Add interactive fix suggestions"));
        assert!(msg.contains("550e8400-e29b-41d4-a716-446655440000"));
        assert!(msg.contains("Ledger:"));
    }

    #[test]
    fn test_format_commit_message_custom() {
        let template = "{category}: {summary} (Ref: {tx_id})";
        let msg = format_commit_message(template, "Bugfix", "Fix null deref", "abc123");
        assert_eq!(msg, "Bugfix: Fix null deref (Ref: abc123)");
    }

    #[test]
    fn validate_git_binary_accepts_absolute_git_only() {
        #[cfg(windows)]
        {
            assert_eq!(
                validate_git_binary(r"C:\Program Files\Git\cmd\git.exe"),
                r"C:\Program Files\Git\cmd\git.exe"
            );
            assert_eq!(
                validate_git_binary(r"C:\Program Files\Git\cmd\git"),
                r"C:\Program Files\Git\cmd\git"
            );
        }
        #[cfg(unix)]
        {
            assert_eq!(validate_git_binary("/usr/bin/git"), "/usr/bin/git");
        }
    }

    #[test]
    fn validate_git_binary_rejects_bare_and_relative_path_redirection() {
        // Bare names enable PATH redirection — DoD-6 requires absolute only.
        // Fallback is always the plain PATH name "git", never the bare override.
        assert_eq!(validate_git_binary("git"), "git");
        assert_eq!(validate_git_binary("git.exe"), "git");
        assert_eq!(validate_git_binary("my-mock-git"), "git");
        assert_eq!(validate_git_binary("relative/path/git"), "git");
        assert_eq!(validate_git_binary(r"C:\evil\notgit.exe"), "git");
        assert_eq!(validate_git_binary("./git"), "git");
        assert_eq!(validate_git_binary(""), "git");
    }

    #[test]
    #[serial]
    fn test_git_binary_env_override_validated() {
        let original = std::env::var("GIT_BINARY").ok();
        // Legitimate: test-only env mutation (edition-2024 set_var is unsafe).
        // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
        unsafe { std::env::set_var("GIT_BINARY", "my-mock-git") };
        assert_eq!(git_binary(), "git"); // rejected → fallback
        // Bare PATH override must also fall back (no PATH redirection).
        // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
        unsafe { std::env::set_var("GIT_BINARY", "git") };
        assert_eq!(git_binary(), "git");
        // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
        unsafe { std::env::set_var("GIT_BINARY", "git.exe") };
        assert_eq!(git_binary(), "git");
        // Cleanup
        if let Some(orig) = original {
            // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
            unsafe { std::env::set_var("GIT_BINARY", orig) };
        } else {
            // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
            unsafe { std::env::remove_var("GIT_BINARY") };
        }
    }

    #[test]
    #[serial]
    fn git_env_hardening_strips_exec_path_poison() {
        // `git --exec-path` prints GIT_EXEC_PATH when set. Unhardened spawns
        // would echo the poison path; hardened git_command() must strip it
        // and report the real install path instead.
        let poison = if cfg!(windows) {
            r"C:\nonexistent\ledgerful-git-exec-poison-0079"
        } else {
            "/nonexistent/ledgerful-git-exec-poison-0079"
        };
        let original = std::env::var("GIT_EXEC_PATH").ok();
        // Legitimate: test-only env mutation.
        // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
        unsafe { std::env::set_var("GIT_EXEC_PATH", poison) };

        // Control: unhardened spawn inherits the poison.
        let unhardened = Command::new("git")
            .args(["--exec-path"])
            .output()
            .expect("unhardened git --exec-path");
        let unhardened_out = String::from_utf8_lossy(&unhardened.stdout);
        assert!(
            unhardened_out.contains("ledgerful-git-exec-poison-0079"),
            "control: unhardened git should see poison GIT_EXEC_PATH, got: {unhardened_out}"
        );

        // Hardened: poison must not appear.
        let hardened = git_command()
            .args(["--exec-path"])
            .output()
            .expect("hardened git --exec-path");
        assert!(
            hardened.status.success(),
            "hardened git --exec-path failed: {}",
            String::from_utf8_lossy(&hardened.stderr)
        );
        let hardened_out = String::from_utf8_lossy(&hardened.stdout);
        assert!(
            !hardened_out.contains("ledgerful-git-exec-poison-0079"),
            "harden_git_env must strip GIT_EXEC_PATH; got: {hardened_out}"
        );
        assert!(
            !hardened_out.trim().is_empty(),
            "expected real exec-path output"
        );

        if let Some(orig) = original {
            // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
            unsafe { std::env::set_var("GIT_EXEC_PATH", orig) };
        } else {
            // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
            unsafe { std::env::remove_var("GIT_EXEC_PATH") };
        }
    }

    #[test]
    #[serial]
    fn git_env_hardening_strips_ssh_command() {
        let original = std::env::var("GIT_SSH_COMMAND").ok();
        // Legitimate: test-only env mutation.
        // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
        unsafe { std::env::set_var("GIT_SSH_COMMAND", r"C:\evil\notssh.exe") };

        // Local, non-network git operation that must not invoke ssh.
        let output = git_command()
            .args(["--version"])
            .output()
            .expect("git --version should spawn");

        assert!(
            output.status.success(),
            "git --version failed under GIT_SSH_COMMAND poison: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.to_ascii_lowercase().contains("git version"),
            "unexpected version output: {stdout}"
        );

        if let Some(orig) = original {
            // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
            unsafe { std::env::set_var("GIT_SSH_COMMAND", orig) };
        } else {
            // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
            unsafe { std::env::remove_var("GIT_SSH_COMMAND") };
        }
    }

    #[test]
    fn test_classify_nothing_to_commit() {
        let result = classify_git_error("nothing to commit, working tree clean", Some(1));
        match result {
            Err(GitCommitError::NothingToCommit) => {}
            other => panic!("Expected NothingToCommit, got {:?}", other),
        }
    }

    #[test]
    fn test_classify_pre_commit_hook() {
        let result = classify_git_error("error: pre-commit hook failed", Some(1));
        match result {
            Err(GitCommitError::PreCommitHookFailed { .. }) => {}
            other => panic!("Expected PreCommitHookFailed, got {:?}", other),
        }
    }

    #[test]
    fn test_classify_gpg_signing() {
        let result = classify_git_error(
            "error: gpg failed to sign the data\nfatal: failed to write commit object",
            Some(128),
        );
        match result {
            Err(GitCommitError::GpgSigningFailed) | Err(GitCommitError::Other { .. }) => {}
            other => panic!("Expected GpgSigningFailed or Other, got {:?}", other),
        }
    }

    #[test]
    fn test_classify_other() {
        let result = classify_git_error(
            "fatal: unable to create '.git/index.lock': File exists",
            Some(128),
        );
        match result {
            Err(GitCommitError::Other { .. }) => {}
            other => panic!("Expected Other, got {:?}", other),
        }
    }
}
