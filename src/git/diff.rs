use gix::Repository;
use std::path::Path;

/// Returns the first 50 lines of diff for a file, or None if not available.
///
/// Uses [`crate::git::git_command`] with Default process policy (no `Config`
/// in this helper). Policy deny degrades to `None` without spawning.
pub fn get_diff_summary(repo: &Repository, path: &Path) -> Option<String> {
    let repo_root = repo.workdir().unwrap_or(repo.path());
    let path_str = path.to_string_lossy();
    let output = crate::git::git_command()
        .ok()?
        .args(["diff", "HEAD", "--", path_str.as_ref()])
        .current_dir(repo_root)
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let diff = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = diff.lines().take(50).collect();
    if lines.is_empty() {
        return None;
    }

    Some(lines.join("\n"))
}
