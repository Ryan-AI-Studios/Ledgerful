use crate::git::GitError;
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
    let head = repo.head().map_err(|e| GitError::HeadFailed {
        source: Box::new(e),
    })?;

    let hash = head.clone().id().map(|id| id.to_string());

    let branch = head.referent_name().map(|name| name.shorten().to_string());

    Ok((hash, branch))
}

/// First 12 lowercase characters of a full (or short) SHA for display/compare.
pub fn shorten_sha_for_display(full_or_short: &str) -> String {
    full_or_short
        .trim()
        .to_ascii_lowercase()
        .chars()
        .take(12)
        .collect()
}

/// Lowercase hex normalize + prefix equality (7 vs 12 → equal).
///
/// Empty inputs are never equal.
pub fn sha_prefix_equal(a: &str, b: &str) -> bool {
    let a = normalize_sha_hex(a);
    let b = normalize_sha_hex(b);
    if a.is_empty() || b.is_empty() {
        return false;
    }
    a.starts_with(&b) || b.starts_with(&a)
}

fn normalize_sha_hex(s: &str) -> String {
    s.trim().to_ascii_lowercase()
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
    fn test_head_info_unborn() -> Result<(), GitError> {
        let dir = tempdir().unwrap();
        let repo = gix::init(dir.path()).unwrap();
        let (hash, branch) = get_head_info(&repo)?;

        assert_eq!(hash, None);
        // On new repo, it's usually 'main' or 'master' even if unborn
        assert!(branch.is_some());
        Ok(())
    }

    #[test]
    fn shorten_sha_for_display_trims_lowercases_and_takes_twelve() {
        assert_eq!(
            shorten_sha_for_display("  AAAAAAAAAAAABBBBBBBBBBBBBBBBBBBBBBBB  "),
            "aaaaaaaaaaaa"
        );
        assert_eq!(shorten_sha_for_display("b57f447"), "b57f447");
        assert_eq!(shorten_sha_for_display("   b57f447   "), "b57f447");
        assert_eq!(shorten_sha_for_display("            b57f447"), "b57f447");
        assert_eq!(shorten_sha_for_display(""), "");
        assert_eq!(shorten_sha_for_display("   "), "");
        assert_eq!(shorten_sha_for_display("unknown"), "unknown");
        assert_eq!(shorten_sha_for_display("zzzzzzzzzzzzzzzz"), "zzzzzzzzzzzz");
    }

    #[test]
    fn sha_prefix_equal_accepts_either_prefix_after_trim_and_lowercase() {
        assert!(sha_prefix_equal("B57F4472EFB3", "b57f4472efb3"));
        assert!(sha_prefix_equal("b57f447", "b57f4472efb3"));
        assert!(sha_prefix_equal("b57f4472efb3", "b57f447"));
        assert!(sha_prefix_equal("  b57f447  ", "B57F4472EFB3"));
        assert!(sha_prefix_equal("unknown", "Unknown"));
        assert!(!sha_prefix_equal("", "b57f447"));
        assert!(!sha_prefix_equal("b57f447", ""));
        assert!(!sha_prefix_equal("", ""));
        assert!(!sha_prefix_equal("   ", "b57f447"));
        assert!(!sha_prefix_equal("abcd", "abce"));
    }
}
