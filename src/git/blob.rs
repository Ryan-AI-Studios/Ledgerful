//! Read file content at HEAD via `gix` (no subprocess).
//!
//! Used by impact enrichment that needs previous-revision content. All failure
//! modes degrade to `None` — callers must never treat absence as a false-positive
//! "something changed" signal.

use std::path::Path;

/// Read the UTF-8 content of a repo-relative path from HEAD using gix only.
///
/// Returns `None` (never panics, never Err) when:
/// - the path is not inside a git repository
/// - HEAD is unborn / missing (fresh repo, no commits)
/// - the file is absent from HEAD (newly added)
/// - the path exists but is not a blob (e.g. tree/submodule)
/// - the blob is not valid UTF-8
/// - any gix lookup fails
///
/// For renames, pass the **old** path (where HEAD still stores the blob).
pub fn read_head_blob(project_root: &Path, path: &Path) -> Option<String> {
    let repo = crate::git::repo::open_repo(project_root).ok()?;
    let tree = repo.head_tree().ok()?;

    // Normalize to forward slashes — gix tree paths are POSIX-style even on Windows.
    let path_str = path.to_string_lossy().replace('\\', "/");
    if path_str.is_empty() {
        return None;
    }

    let entry = tree.lookup_entry_by_path(path_str.as_str()).ok()??;
    let object = entry.object().ok()?;
    let blob = object.try_into_blob().ok()?;
    String::from_utf8(blob.data.clone()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command;
    use tempfile::tempdir;

    fn git(cwd: &Path, args: &[&str]) -> std::process::Output {
        Command::new("git")
            .args(args)
            .current_dir(cwd)
            .env("GIT_AUTHOR_NAME", "test")
            .env("GIT_AUTHOR_EMAIL", "test@example.com")
            .env("GIT_COMMITTER_NAME", "test")
            .env("GIT_COMMITTER_EMAIL", "test@example.com")
            .output()
            .expect("git command")
    }

    fn init_repo_with_commit(dir: &Path) {
        assert!(git(dir, &["init", "-b", "main"]).status.success());
        assert!(
            git(dir, &["config", "user.email", "test@example.com"])
                .status
                .success()
        );
        assert!(git(dir, &["config", "user.name", "test"]).status.success());
        fs::write(dir.join("tracked.txt"), "hello from head\n").unwrap();
        fs::create_dir_all(dir.join("sub")).unwrap();
        fs::write(dir.join("sub").join("nested.txt"), "nested\n").unwrap();
        assert!(git(dir, &["add", "."]).status.success());
        assert!(git(dir, &["commit", "-m", "init"]).status.success());
    }

    #[test]
    fn modified_file_reads_head_content() {
        let dir = tempdir().unwrap();
        init_repo_with_commit(dir.path());
        // Dirty working tree must not affect HEAD blob.
        fs::write(dir.path().join("tracked.txt"), "dirty working tree\n").unwrap();
        let content = read_head_blob(dir.path(), Path::new("tracked.txt"));
        assert_eq!(content.as_deref(), Some("hello from head\n"));
    }

    #[test]
    fn added_file_absent_from_head_returns_none() {
        let dir = tempdir().unwrap();
        init_repo_with_commit(dir.path());
        fs::write(dir.path().join("brand_new.txt"), "only in worktree\n").unwrap();
        assert!(read_head_blob(dir.path(), Path::new("brand_new.txt")).is_none());
    }

    #[test]
    fn deleted_file_still_readable_from_head() {
        let dir = tempdir().unwrap();
        init_repo_with_commit(dir.path());
        fs::remove_file(dir.path().join("tracked.txt")).unwrap();
        let content = read_head_blob(dir.path(), Path::new("tracked.txt"));
        assert_eq!(content.as_deref(), Some("hello from head\n"));
    }

    #[test]
    fn renamed_file_readable_via_old_path() {
        let dir = tempdir().unwrap();
        init_repo_with_commit(dir.path());
        // Simulate rename: old path still at HEAD, new path only in worktree.
        fs::write(dir.path().join("renamed.txt"), "hello from head\n").unwrap();
        fs::remove_file(dir.path().join("tracked.txt")).unwrap();
        let via_old = read_head_blob(dir.path(), Path::new("tracked.txt"));
        let via_new = read_head_blob(dir.path(), Path::new("renamed.txt"));
        assert_eq!(via_old.as_deref(), Some("hello from head\n"));
        assert!(via_new.is_none(), "new path is not in HEAD yet");
    }

    #[test]
    fn no_head_unborn_repo_returns_none() {
        let dir = tempdir().unwrap();
        // Fresh init, no commits → unborn HEAD.
        assert!(git(dir.path(), &["init", "-b", "main"]).status.success());
        fs::write(dir.path().join("x.txt"), "no commit yet\n").unwrap();
        assert!(read_head_blob(dir.path(), Path::new("x.txt")).is_none());
    }

    #[test]
    fn nested_path_reads_correctly() {
        let dir = tempdir().unwrap();
        init_repo_with_commit(dir.path());
        let content = read_head_blob(dir.path(), Path::new("sub/nested.txt"));
        assert_eq!(content.as_deref(), Some("nested\n"));
    }

    #[test]
    fn non_repo_returns_none() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("x.txt"), "no git\n").unwrap();
        assert!(read_head_blob(dir.path(), Path::new("x.txt")).is_none());
    }
}
