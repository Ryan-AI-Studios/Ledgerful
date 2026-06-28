use crate::git::classify::classify_status;
use crate::git::{FileChange, GitError};
use anyhow::Result;
use gix::Repository;
use gix::bstr::BString;

pub fn get_repo_status(repo: &Repository) -> Result<Vec<FileChange>, GitError> {
    let mut file_changes = Vec::new();

    let status = repo
        .status(gix::progress::Discard)
        .map_err(|e| GitError::MetadataError { source: e.into() })?
        .untracked_files(gix::status::UntrackedFiles::Files)
        .index_worktree_rewrites(Some(gix::diff::Rewrites::default()));

    let items = status
        .into_iter(Vec::<BString>::new())
        .map_err(|e| GitError::MetadataError { source: e.into() })?;

    for item in items {
        let item = item.map_err(|e| GitError::MetadataError { source: e.into() })?;
        if let Some(changes) = classify_status(repo, &item) {
            file_changes.extend(changes);
        }
    }

    // Sort changes by path for determinism
    file_changes.sort_by(|a, b| a.path.cmp(&b.path));

    Ok(file_changes)
}
