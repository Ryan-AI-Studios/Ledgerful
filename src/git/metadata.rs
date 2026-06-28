//! Shared git metadata walk logic (Tracks TA29 + TA30).
//!
//! Walks git history (first-parent, up to 1000 commits) to collect the most
//! recent commit timestamp and author name for each file. The result is a
//! `HashMap<String, (String, String)>` mapping `file_path → (iso8601_committer_time,
//! author_name)`.
//!
//! Uses the **author** signature for the contributor field (the person who
//! wrote the code) and the **committer time** for the timestamp (when the
//! commit landed in the repo). This distinction matters in GitHub squash-merge
//! flow where the committer is often "GitHub" but the author is the developer.
//!
//! Both TA29 (web API TTL cache) and TA30 (indexer backfill) call this function
//! to avoid code duplication.

use crate::git::repo::open_repo;
use camino::Utf8Path;
use gix::object::tree::diff::ChangeDetached;
use miette::Result;
use std::collections::HashMap;
use std::time::Instant;

/// Default maximum commits to walk.
pub const DEFAULT_MAX_COMMITS: usize = 1000;

/// Cache entry for the web API TTL cache: `(fetched_at, map)`.
pub type GitMetaCacheEntry = Option<(Instant, HashMap<String, (String, String)>)>;

/// Build a `file_path → (iso8601_committer_time, author_name)` map by walking
/// git history newest-first. First occurrence of each file wins (most recent
/// commit). Files with no git history are absent from the map.
///
/// - `repo_root`: the repository root path.
/// - `max_commits`: maximum number of commits to walk.
pub fn collect_git_metadata(
    repo_root: &Utf8Path,
    max_commits: usize,
) -> Result<HashMap<String, (String, String)>> {
    let repo = match open_repo(repo_root.as_std_path()) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("No git repository available for git metadata: {}", e);
            return Ok(HashMap::new());
        }
    };

    let head = match repo.head_commit() {
        Ok(h) => h,
        Err(e) => {
            tracing::warn!("No HEAD commit for git metadata: {}", e);
            return Ok(HashMap::new());
        }
    };

    let walk = head
        .id()
        .ancestors()
        .first_parent_only()
        .all()
        .map_err(|e| miette::miette!("Failed to start commit walk for git metadata: {}", e))?;

    let mut map: HashMap<String, (String, String)> = HashMap::new();
    let mut commit_count: usize = 0;

    for res in walk {
        if commit_count >= max_commits {
            break;
        }
        let info = match res {
            Ok(info) => info,
            Err(e) => {
                tracing::warn!("Failed to retrieve commit info during git metadata walk: {e}");
                continue;
            }
        };

        let commit = match info.id().object().map(|obj| obj.into_commit()) {
            Ok(commit) => commit,
            Err(e) => {
                tracing::warn!("Failed to retrieve commit object for {}: {e}", info.id());
                continue;
            }
        };

        // Committer time → ISO-8601.
        let committer_time = match commit.time() {
            Ok(t) => t.seconds,
            Err(e) => {
                tracing::warn!("Failed to read commit time for {}: {e}", info.id());
                continue;
            }
        };
        let iso_ts = chrono::DateTime::from_timestamp(committer_time, 0)
            .map(|dt| dt.to_rfc3339())
            .unwrap_or_else(|| committer_time.to_string());

        // Author name (NOT committer — see module docs).
        let author_name = commit
            .author()
            .map(|a| a.name.to_string())
            .unwrap_or_else(|_| "unknown".to_string());

        // Diff tree against parent to collect touched files.
        let current_tree = match commit.tree() {
            Ok(tree) => tree,
            Err(e) => {
                tracing::warn!("Failed to retrieve tree for {}: {e}", info.id());
                continue;
            }
        };

        let parent_tree = match commit.parent_ids().next() {
            Some(p_id) => match p_id.object().map(|obj| obj.into_commit().tree()) {
                Ok(Ok(tree)) => tree,
                _ => repo.empty_tree(),
            },
            None => repo.empty_tree(),
        };

        let changes = match repo.diff_tree_to_tree(Some(&parent_tree), Some(&current_tree), None) {
            Ok(changes) => changes,
            Err(e) => {
                tracing::warn!("Failed to diff tree for {}: {e}", info.id());
                continue;
            }
        };

        for change in changes {
            let locations: Vec<Vec<u8>> = match change {
                ChangeDetached::Addition { location, .. }
                | ChangeDetached::Deletion { location, .. }
                | ChangeDetached::Modification { location, .. } => {
                    vec![location.to_vec()]
                }
                ChangeDetached::Rewrite {
                    location,
                    source_location,
                    ..
                } => vec![location.to_vec(), source_location.to_vec()],
            };
            for loc in locations {
                let path_str = String::from_utf8_lossy(&loc).replace('\\', "/");
                // First occurrence wins (newest commit, since we walk newest-first).
                map.entry(path_str)
                    .or_insert((iso_ts.clone(), author_name.clone()));
            }
        }

        commit_count += 1;
    }

    Ok(map)
}

/// Normalized lookup: tries the exact path, then a backslash-normalized
/// variant. Returns `None` if the file has no git history in the walk window.
pub fn lookup_git_meta<'a>(
    map: &'a HashMap<String, (String, String)>,
    file_path: &str,
) -> Option<&'a (String, String)> {
    if let Some(v) = map.get(file_path) {
        return Some(v);
    }
    let normalized = file_path.replace('\\', "/");
    map.get(&normalized)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lookup_exact_path() {
        let mut map = HashMap::new();
        map.insert(
            "src/main.rs".to_string(),
            ("2024-01-01T00:00:00+00:00".to_string(), "Alice".to_string()),
        );
        let result = lookup_git_meta(&map, "src/main.rs");
        assert!(result.is_some());
        assert_eq!(result.unwrap().1, "Alice");
    }

    #[test]
    fn test_lookup_backslash_path() {
        let mut map = HashMap::new();
        map.insert(
            "src/main.rs".to_string(),
            ("2024-01-01T00:00:00+00:00".to_string(), "Bob".to_string()),
        );
        let result = lookup_git_meta(&map, "src\\main.rs");
        assert!(result.is_some());
        assert_eq!(result.unwrap().1, "Bob");
    }

    #[test]
    fn test_lookup_missing_returns_none() {
        let map: HashMap<String, (String, String)> = HashMap::new();
        let result = lookup_git_meta(&map, "nonexistent.rs");
        assert!(result.is_none());
    }

    #[test]
    fn test_collect_no_git_repo_returns_empty_map() {
        let dir = tempfile::tempdir().unwrap();
        let result =
            collect_git_metadata(camino::Utf8Path::from_path(dir.path()).unwrap(), 1000).unwrap();
        assert!(result.is_empty());
    }

    /// Verify that `collect_git_metadata` uses the **author** signature for
    /// `last_contributor`, not the committer. This is a critical correctness
    /// requirement (Track TA30 testing strategy): in GitHub squash-merge flow,
    /// the committer is often "GitHub" but the author is the developer.
    #[test]
    fn test_collect_uses_author_not_committer() {
        use std::process::Command;

        let dir = tempfile::tempdir().unwrap();
        let dir_path = dir.path();

        // Init git repo.
        Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(dir_path)
            .output()
            .unwrap();

        // Set author and committer to DIFFERENT names.
        // `git commit` uses GIT_AUTHOR_* and GIT_COMMITTER_* env vars.
        let author_name = "Alice Developer";
        let committer_name = "CI Bot";

        // Create a file and commit it.
        std::fs::create_dir_all(dir_path.join("src")).unwrap();
        std::fs::write(dir_path.join("src/main.rs"), "fn main() {}\n").unwrap();

        let output = Command::new("git")
            .args(["add", "src/main.rs"])
            .current_dir(dir_path)
            .output()
            .unwrap();
        assert!(output.status.success(), "git add failed");

        let output = Command::new("git")
            .args([
                "-c",
                "user.name=Alice Developer",
                "-c",
                "user.email=alice@test.local",
                "commit",
                "-m",
                "initial",
            ])
            .env("GIT_AUTHOR_NAME", author_name)
            .env("GIT_AUTHOR_EMAIL", "alice@test.local")
            .env("GIT_COMMITTER_NAME", committer_name)
            .env("GIT_COMMITTER_EMAIL", "ci-bot@test.local")
            .current_dir(dir_path)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git commit failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );

        // Collect git metadata.
        let map =
            collect_git_metadata(camino::Utf8Path::from_path(dir_path).unwrap(), 1000).unwrap();

        // Verify the contributor is the AUTHOR, not the committer.
        let entry = map.get("src/main.rs");
        assert!(entry.is_some(), "src/main.rs should be in the map");
        let (_ts, contributor) = entry.unwrap();
        assert_eq!(
            contributor, "Alice Developer",
            "last_contributor should be the author name, not the committer name"
        );
        assert_ne!(
            contributor, "CI Bot",
            "last_contributor should NOT be the committer name"
        );
    }
}
