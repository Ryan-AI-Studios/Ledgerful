use crate::git::GitError;
use anyhow::Result;
use gix::{Repository, discover};
use std::path::Path;

pub fn open_repo(path: &Path) -> Result<Repository, GitError> {
    let discovered = discover(path).map_err(|e| GitError::RepoDiscoveryFailed {
        path: path.to_string_lossy().into_owned(),
        source: Box::new(e),
    })?;

    let repo = gix::open(discovered.path()).map_err(|e| GitError::RepoOpenFailed {
        path: path.to_string_lossy().into_owned(),
        source: Box::new(e),
    })?;

    Ok(repo)
}

pub fn get_head_info(repo: &Repository) -> Result<(Option<String>, Option<String>), GitError> {
    let head = repo
        .head()
        .map_err(|e| GitError::MetadataError { source: e.into() })?;

    let hash = head.clone().id().map(|id| id.to_string());

    let branch = head.referent_name().map(|name| name.shorten().to_string());

    Ok((hash, branch))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_discover_fail() {
        let dir = tempdir().unwrap();
        let result = open_repo(dir.path());
        assert!(matches!(result, Err(GitError::RepoDiscoveryFailed { .. })));
    }

    #[test]
    fn test_head_info_unborn() -> Result<()> {
        let dir = tempdir().unwrap();
        let repo = gix::init(dir.path()).unwrap();
        let (hash, branch) = get_head_info(&repo)?;

        assert_eq!(hash, None);
        // On new repo, it's usually 'main' or 'master' even if unborn
        assert!(branch.is_some());
        Ok(())
    }
}
