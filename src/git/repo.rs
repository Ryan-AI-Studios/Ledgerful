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

/// Where the running commit sits relative to worktree HEAD.
///
/// `NotConsulted` means the caller did not ask. After both ids resolve,
/// `SameCommit` is decided before `merge_base`; otherwise the merge base
/// determines their relationship. Resolution or merge-base failure is `Unknown`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommitAncestry {
    NotConsulted,
    /// Both ids resolved to the same OID.
    SameCommit,
    RunningIsAncestor,
    HeadIsAncestor,
    Divergent,
    Unknown,
}

/// Resolve both ids, check for the same commit, then compare their merge base.
///
/// Equal resolved OIDs return [`CommitAncestry::SameCommit`] before `merge_base`.
/// Inputs are expected to resolve to commits or branch refs; annotated tags are
/// not peeled.
/// A missing, ambiguous, or unresolvable id is [`CommitAncestry::Unknown`].
/// This function does not create commits and does not spawn `git`.
pub fn commit_ancestry(repo: &Repository, running_sha: &str, head_sha: &str) -> CommitAncestry {
    let running = match repo.rev_parse_single(running_sha.trim()) {
        Ok(id) => id.detach(),
        Err(_) => return CommitAncestry::Unknown,
    };
    let head = match repo.rev_parse_single(head_sha.trim()) {
        Ok(id) => id.detach(),
        Err(_) => return CommitAncestry::Unknown,
    };
    if running == head {
        return CommitAncestry::SameCommit;
    }
    let base = match repo.merge_base(running, head) {
        Ok(id) => id.detach(),
        Err(_) => return CommitAncestry::Unknown,
    };
    if base == running {
        CommitAncestry::RunningIsAncestor
    } else if base == head {
        CommitAncestry::HeadIsAncestor
    } else {
        CommitAncestry::Divergent
    }
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

    #[test]
    fn commit_ancestry_parent_child_and_divergent() {
        let dir = tempdir().unwrap();
        let repo = gix::init(dir.path()).unwrap();
        let tree = repo.empty_tree().id;
        let time = "0 +0000";
        let author = gix::actor::SignatureRef {
            name: "Ledgerful Test".into(),
            email: "test@example.com".into(),
            time,
        };
        let parent = repo
            .commit_as(
                author,
                author,
                "refs/heads/main",
                "parent",
                tree,
                std::iter::empty::<gix::ObjectId>(),
            )
            .unwrap();
        let parent_id = parent.detach();
        let parent_sha = parent_id.to_string();
        let child = repo
            .commit_as(
                author,
                author,
                "refs/heads/child",
                "child",
                tree,
                [parent_id],
            )
            .unwrap();
        let child_sha = child.to_string();
        let other = repo
            .commit_as(
                author,
                author,
                "refs/heads/other",
                "other",
                tree,
                [parent_id],
            )
            .unwrap();
        let other_sha = other.to_string();

        assert_eq!(
            commit_ancestry(&repo, &parent_sha, &child_sha),
            CommitAncestry::RunningIsAncestor
        );
        assert_eq!(
            commit_ancestry(&repo, &child_sha, &parent_sha),
            CommitAncestry::HeadIsAncestor
        );
        assert_eq!(
            commit_ancestry(&repo, &child_sha, &other_sha),
            CommitAncestry::Divergent
        );
        assert_eq!(
            commit_ancestry(
                &repo,
                "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
                &child_sha
            ),
            CommitAncestry::Unknown
        );
    }

    #[rstest::rstest]
    #[case::full_sha("full")]
    #[case::short_sha("short")]
    #[case::full_branch_ref("refs/heads/main")]
    #[case::short_branch_ref("main")]
    fn commit_ancestry_same_commit(#[case] revision: &str, #[values(false, true)] reverse: bool) {
        let dir = tempdir().unwrap();
        let repo = gix::init(dir.path()).unwrap();
        let author = gix::actor::SignatureRef {
            name: "Ledgerful Test".into(),
            email: "test@example.com".into(),
            time: "0 +0000",
        };
        let commit = repo
            .commit_as(
                author,
                author,
                "refs/heads/main",
                "same commit",
                repo.empty_tree().id,
                std::iter::empty::<gix::ObjectId>(),
            )
            .unwrap();
        let full_sha = commit.to_string();
        let revision = match revision {
            "full" => full_sha.as_str(),
            "short" => &full_sha[..7],
            reference => reference,
        };
        let (running, head) = if reverse {
            (full_sha.as_str(), revision)
        } else {
            (revision, full_sha.as_str())
        };
        assert_eq!(
            commit_ancestry(&repo, running, head),
            CommitAncestry::SameCommit
        );
    }

    #[test]
    fn commit_ancestry_identical_unresolvable_ids_are_unknown() {
        let dir = tempdir().unwrap();
        let repo = gix::init(dir.path()).unwrap();
        let missing = "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef";
        assert_eq!(
            commit_ancestry(&repo, missing, missing),
            CommitAncestry::Unknown
        );
    }
}
