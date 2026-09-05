use crate::config::load::load_config;
use crate::git::classify::classify_status;
use crate::git::repo::open_repo;
use crate::git::{FileChange, GitError};
use crate::state::layout::Layout;
use gix::Repository;
use gix::bstr::BString;
use std::path::Path;

pub fn get_repo_status(repo: &Repository) -> Result<Vec<FileChange>, GitError> {
    let mut file_changes = Vec::new();

    let status = repo
        .status(gix::progress::Discard)
        .map_err(|e| GitError::StatusPlatform {
            source: Box::new(e),
        })?
        .untracked_files(gix::status::UntrackedFiles::Files)
        .index_worktree_rewrites(Some(gix::diff::Rewrites::default()));

    let items = status
        .into_iter(Vec::<BString>::new())
        .map_err(|e| GitError::StatusIter {
            source: Box::new(e),
        })?;

    for item in items {
        let item = item.map_err(|e| GitError::StatusItem {
            source: Box::new(e),
        })?;
        if let Some(changes) = classify_status(repo, &item) {
            file_changes.extend(changes);
        }
    }

    // Sort changes by path for determinism
    file_changes.sort_by(|a, b| a.path.cmp(&b.path));

    Ok(file_changes)
}

/// Normalize a path for filter-CLI membership (`\` → `/`, strip leading `./` and `/`).
///
/// Git `FileChange` paths are repo-relative; this is only for set membership
/// against indexed `file_path` / `source_file` strings.
pub fn normalize_filter_path(path: &Path) -> String {
    let mut s = path.to_string_lossy().replace('\\', "/");
    if let Some(stripped) = s.strip_prefix("./") {
        s = stripped.to_string();
    }
    if let Some(stripped) = s.strip_prefix('/') {
        s = stripped.to_string();
    }
    s
}

/// Working-tree changes for filter CLIs (ignore-filtered). Git-only; no impact.
///
/// Mirrors the front half of `execute_impact_silent` (open + status + ignore
/// filter) without StorageManager, orchestrator, federation, `save_packet`, or
/// `write_impact_report`. Not a merge with verify's material-path checks.
pub fn collect_changed_files_for_filter(layout: &Layout) -> miette::Result<Vec<FileChange>> {
    let repo = open_repo(layout.root.as_std_path())?;
    let all_changes = get_repo_status(&repo)?;
    let config = load_config(layout).unwrap_or_else(|_| crate::config::model::Config::default());
    let changes = crate::git::ignore::filter_ignored_changes(
        all_changes,
        &config.watch.ignore_patterns,
        true,
    )?;
    Ok(changes)
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
        fs::write(dir.join("tracked.txt"), "hello\n").expect("write tracked");
        assert!(git(dir, &["add", "."]).status.success());
        assert!(git(dir, &["commit", "-m", "init"]).status.success());
    }

    #[test]
    fn normalize_filter_path_backslash_and_dot_slash() {
        assert_eq!(
            normalize_filter_path(Path::new("src\\foo\\bar.rs")),
            "src/foo/bar.rs"
        );
        assert_eq!(
            normalize_filter_path(Path::new("./src/foo.rs")),
            "src/foo.rs"
        );
        assert_eq!(
            normalize_filter_path(Path::new("/src/foo.rs")),
            "src/foo.rs"
        );
        assert_eq!(normalize_filter_path(Path::new("src/foo.rs")), "src/foo.rs");
    }

    #[test]
    fn collect_changed_files_for_filter_clean_is_empty() {
        let dir = tempdir().expect("tempdir");
        init_repo_with_commit(dir.path());
        let root = camino::Utf8Path::from_path(dir.path()).expect("utf8 path");
        let layout = Layout::new(root);
        let changes = collect_changed_files_for_filter(&layout).expect("collect");
        assert!(
            changes.is_empty(),
            "clean tree should yield empty change set, got {changes:?}"
        );
    }

    #[test]
    fn collect_changed_files_for_filter_dirty_contains_path() {
        let dir = tempdir().expect("tempdir");
        init_repo_with_commit(dir.path());
        fs::write(dir.path().join("tracked.txt"), "dirty\n").expect("dirty write");
        let root = camino::Utf8Path::from_path(dir.path()).expect("utf8 path");
        let layout = Layout::new(root);
        let changes = collect_changed_files_for_filter(&layout).expect("collect");
        let paths: Vec<String> = changes
            .iter()
            .map(|c| normalize_filter_path(&c.path))
            .collect();
        assert!(
            paths
                .iter()
                .any(|p| p == "tracked.txt" || p.ends_with("tracked.txt")),
            "dirty file should appear in change set, got {paths:?}"
        );
    }
}
