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

/// Returns the path to the git binary, respecting the GIT_BINARY env override.
fn git_binary() -> String {
    std::env::var("GIT_BINARY").unwrap_or_else(|_| "git".to_string())
}

/// Build and configure a `std::process::Command` for the git binary with
/// ProcessPolicy timeout enforcement.
#[allow(dead_code)]
fn git_command() -> Command {
    let binary = git_binary();
    let mut cmd = Command::new(binary);
    // Apply default process policy timeout (5 minutes)
    let policy = ProcessPolicy::default();
    // Check that "git" is allowed by policy
    if let Err(e) = check_policy("git", &policy) {
        // Policy check failure shouldn't happen with defaults, but log it
        tracing::warn!("Git command blocked by process policy: {}", e);
    }
    // Set a reasonable timeout
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
    let output = Command::new(git_binary())
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
    let output = Command::new(git_binary())
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
    let status = Command::new(git_binary())
        .args(["diff", "--cached", "--quiet"])
        .status()
        .map_err(|e| {
            GitStateError::CommandFailed(format!("Failed to run git diff --cached: {}", e))
        })?;

    // exit 0 = no differences (no staged changes) â†’ return false
    // exit 1 = differences (staged changes) â†’ return true
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
pub fn git_commit(message: &str, signoff: bool) -> Result<(), GitCommitError> {
    let binary = git_binary();
    let mut cmd = Command::new(&binary);
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
    fn test_git_binary_env_override() {
        let original = std::env::var("GIT_BINARY").ok();
        unsafe { std::env::set_var("GIT_BINARY", "my-mock-git") };
        assert_eq!(git_binary(), "my-mock-git");
        // Cleanup
        if let Some(orig) = original {
            unsafe { std::env::set_var("GIT_BINARY", orig) };
        } else {
            unsafe { std::env::remove_var("GIT_BINARY") };
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
