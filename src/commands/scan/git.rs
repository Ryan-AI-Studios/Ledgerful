use crate::git::{ChangeType, FileChange};
use miette::Result;
use std::path::PathBuf;

/// Parse `git diff --name-status` output into `FileChange` values.
fn parse_name_status_output(stdout: &str) -> Vec<FileChange> {
    let mut changes = Vec::new();
    for line in stdout.lines().filter(|l| !l.is_empty()) {
        let mut parts = line.splitn(3, '\t');
        let status = parts.next().unwrap_or("").trim();
        let path_a = parts.next().unwrap_or("").trim();
        let path_b = parts.next().map(str::trim);

        let (change_type, path) = if status.starts_with('R') {
            // Renamed: status is R<score>, path_a=old, path_b=new
            let new_path = path_b.unwrap_or(path_a);
            (
                ChangeType::Renamed {
                    old_path: PathBuf::from(path_a),
                },
                PathBuf::from(new_path),
            )
        } else {
            let ct = match status {
                "A" => ChangeType::Added,
                "D" => ChangeType::Deleted,
                _ => ChangeType::Modified,
            };
            (ct, PathBuf::from(path_a))
        };

        changes.push(FileChange {
            path,
            change_type,
            is_staged: true,
        });
    }
    changes
}

/// Detect whether a git-diff failure is because the base commit is missing from
/// the local clone (typical shallow checkout with `fetch-depth: 1`).
pub(super) fn is_missing_base_commit_error(stderr: &str) -> bool {
    let lowered = stderr.to_lowercase();
    lowered.contains("not a valid object name")
        || lowered.contains("unknown revision")
        || lowered.contains("bad revision")
        || lowered.contains("does not exist")
        || lowered.contains("invalid symmetric difference expression")
}

/// Format the actionable fetch-depth error.
fn missing_base_commit_error(base_ref: &str) -> miette::Error {
    miette::miette!(
        "base commit '{}' is not present in the local clone.\n       This usually means the checkout was shallow (fetch-depth: 1).\n       Fix: set `fetch-depth: 0` in your actions/checkout step, or fetch the base ref explicitly.",
        base_ref
    )
}

/// Collect changed files by running `git diff --name-status <base_ref>...HEAD`.
/// Returns a `Vec<FileChange>` with accurate `ChangeType` values per entry.
///
/// `pub(crate)` so `change-context` can build a base-ref `RepoSnapshot` and run
/// the in-memory impact path without calling silent-persist helpers.
///
/// Resolves `base_ref` to a commit OID first so option-like values
/// (e.g. `--output=…`) cannot be interpreted as `git diff` options.
pub(crate) fn files_changed_since(
    repo_root: &std::path::Path,
    base_ref: &str,
) -> Result<Vec<FileChange>> {
    let resolved = resolve_commit_oid(repo_root, base_ref)?;
    files_changed_between(repo_root, &format!("{resolved}...HEAD"), base_ref)
}

/// Resolve a user-supplied revision to a full commit OID.
///
/// Rejects empty / option-like values, then uses
/// `git rev-parse --verify --end-of-options <rev>^{commit}`.
pub(crate) fn resolve_commit_oid(repo_root: &std::path::Path, rev: &str) -> Result<String> {
    let rev = rev.trim();
    if rev.is_empty() {
        return Err(miette::miette!("git revision must not be empty"));
    }
    if rev.starts_with('-') {
        return Err(miette::miette!(
            "git revision must not start with '-': refused option-like ref '{rev}'"
        ));
    }
    let peel = format!("{rev}^{{commit}}");
    let output = crate::git::git_command()?
        .args(["rev-parse", "--verify", "--end-of-options", &peel])
        .current_dir(repo_root)
        .output()
        .map_err(|e| miette::miette!("Failed to run git rev-parse: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if is_missing_base_commit_error(&stderr) {
            return Err(missing_base_commit_error(rev));
        }
        return Err(miette::miette!(
            "git rev-parse --verify failed for '{rev}': {}",
            stderr.trim()
        ));
    }
    let oid = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if oid.is_empty() || oid.starts_with('-') {
        return Err(miette::miette!(
            "git rev-parse returned an unusable OID for '{rev}'"
        ));
    }
    Ok(oid)
}

/// Collect changed files by running `git diff --name-status <range>`.
/// `base_ref_for_errors` is used when formatting the missing-base-commit hint.
///
/// Prefer resolved commit OIDs in `range` (see [`resolve_commit_oid`]) so
/// untrusted ref strings cannot inject git options.
pub(crate) fn files_changed_between(
    repo_root: &std::path::Path,
    range: &str,
    base_ref_for_errors: &str,
) -> Result<Vec<FileChange>> {
    // Guard residual option injection if a caller passes a raw range.
    if range.trim_start().starts_with('-') {
        return Err(miette::miette!(
            "git diff range must not start with '-': refused '{range}'"
        ));
    }
    let output = crate::git::git_command()?
        .args(["diff", "--name-status", "--end-of-options", range])
        .current_dir(repo_root)
        .output()
        .map_err(|e| miette::miette!("Failed to run git diff: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if is_missing_base_commit_error(&stderr) {
            return Err(missing_base_commit_error(base_ref_for_errors));
        }
        return Err(miette::miette!(
            "git diff --name-status {} failed: {}",
            range,
            stderr.trim()
        ));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(parse_name_status_output(&stdout))
}

/// Parse a `--pr <RANGE>` value into `(base_ref, head_ref, git_range)`.
///
/// Supports `base...head`, `base..head`, or a bare `base` (default head to
/// `HEAD`). Validates that base is non-empty. `git_range` is the normalized
/// three-dot range to pass to `git diff --name-status`.
///
/// Two-dot (`A..B`) is normalized to three-dot (`A...B`) because, in git,
/// `A..B` diffs A against B directly while `A...B` diffs merge-base(A,B)
/// against B. For PR risk assessment three-dot is always correct: two-dot
/// can include base-branch changes that are not part of the PR.
pub(crate) fn parse_pr_range(range: &str) -> Result<(String, String, String)> {
    let trimmed = range.trim();
    if trimmed.is_empty() {
        return Err(miette::miette!("--pr range must not be empty"));
    }

    let (base, head, normalized_git_range) = if let Some(pos) = trimmed.find("...") {
        let (base, head) = trimmed.split_at(pos);
        (base, &head[3..], trimmed.to_string())
    } else if let Some(pos) = trimmed.find("..") {
        let (base, head) = trimmed.split_at(pos);
        let head = &head[2..];
        let normalized = format!("{}...{}", base, head);
        (base, head, normalized)
    } else {
        (trimmed, "HEAD", format!("{}...HEAD", trimmed))
    };

    let base = base.trim();
    let head = head.trim();

    if base.is_empty() {
        return Err(miette::miette!(
            "--pr range '{}' has an empty base ref",
            range
        ));
    }
    if head.is_empty() {
        return Err(miette::miette!(
            "--pr range '{}' has an empty head ref",
            range
        ));
    }

    Ok((base.to_string(), head.to_string(), normalized_git_range))
}
