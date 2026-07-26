//! Shared git metadata walk logic (Tracks TA29 + TA30 + 0086).
//!
//! Walks git history (first-parent, up to [`DEFAULT_MAX_COMMITS`] commits) to
//! collect per-path recency and (for PR reports) churn. The walk is **index-free**
//! and has no storage dependency — safe for CI checkouts with full history.
//!
//! ## Surfaces
//!
//! - [`collect_git_metadata`] — web API TTL cache + indexer backfill: returns
//!   `path → (iso8601_committer_time, author_name)`. First occurrence wins
//!   (most recent commit).
//! - [`collect_path_history`] — PR scan schema v2 enrichment: returns per-path
//!   `churn` + `last_commit_at` plus window honesty fields
//!   (`history_window_commits`, `history_truncated`). **Author names are
//!   deliberately omitted** — recency and churn are risk signals; naming a
//!   person in an automated public PR comment is a social cost with no analytic
//!   gain. Do not "restore" authors on this path.
//!
//! Both share a single bounded first-parent walk implementation.
//!
//! Uses the **author** signature for the contributor field (the person who
//! wrote the code) and the **committer time** for the timestamp (when the
//! commit landed in the repo). This distinction matters in GitHub squash-merge
//! flow where the committer is often "GitHub" but the author is the developer.

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

/// Per-path history signals for PR scan enrichment (schema v2).
///
/// **No author names** — see module docs. Callers must not re-add contributor
/// identity to the PR report path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathHistoryEntry {
    /// Number of commits in the walk window that touched this path.
    pub churn: u32,
    /// Committer time (ISO-8601) of the most recent touch in the window.
    pub last_commit_at: String,
}

/// Result of a bounded first-parent history walk for PR-report enrichment.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PathHistoryResult {
    /// Forward-slash-normalized path → history signals.
    pub by_path: HashMap<String, PathHistoryEntry>,
    /// How many commits were actually walked (≤ max_commits).
    pub history_window_commits: u32,
    /// `true` when the walk stopped because it hit `max_commits`.
    pub history_truncated: bool,
}

/// Internal per-path accumulator shared by both public walk APIs.
struct PathWalkStats {
    last_commit_at: String,
    author_name: String,
    churn: u32,
}

struct WalkOutcome {
    paths: HashMap<String, PathWalkStats>,
    commits_walked: u32,
    truncated: bool,
}

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
    let outcome = walk_history(repo_root, max_commits)?;
    let mut map = HashMap::with_capacity(outcome.paths.len());
    for (path, stats) in outcome.paths {
        map.insert(path, (stats.last_commit_at, stats.author_name));
    }
    Ok(map)
}

/// Collect per-path churn and recency for PR scan schema v2.
///
/// Index-free, bounded first-parent walk (same bound as [`collect_git_metadata`]).
/// **Does not return author names** — see module docs and [`PathHistoryEntry`].
///
/// Returns an empty result (not an error) when no git repository / HEAD is
/// available, matching [`collect_git_metadata`]'s degrade-gracefully contract.
pub fn collect_path_history(repo_root: &Utf8Path, max_commits: usize) -> Result<PathHistoryResult> {
    let outcome = walk_history(repo_root, max_commits)?;
    let mut by_path = HashMap::with_capacity(outcome.paths.len());
    for (path, stats) in outcome.paths {
        by_path.insert(
            path,
            PathHistoryEntry {
                churn: stats.churn,
                last_commit_at: stats.last_commit_at,
            },
        );
    }
    Ok(PathHistoryResult {
        by_path,
        history_window_commits: outcome.commits_walked,
        history_truncated: outcome.truncated,
    })
}

/// Shared first-parent walk: newest-first, bounded at `max_commits`.
///
/// For each path: first sighting records last_commit_at + author; every
/// subsequent sighting increments churn only.
fn walk_history(repo_root: &Utf8Path, max_commits: usize) -> Result<WalkOutcome> {
    let repo = match open_repo(repo_root.as_std_path()) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("No git repository available for git metadata: {}", e);
            return Ok(WalkOutcome {
                paths: HashMap::new(),
                commits_walked: 0,
                truncated: false,
            });
        }
    };

    let head = match repo.head_commit() {
        Ok(h) => h,
        Err(e) => {
            tracing::warn!("No HEAD commit for git metadata: {}", e);
            return Ok(WalkOutcome {
                paths: HashMap::new(),
                commits_walked: 0,
                truncated: false,
            });
        }
    };

    let walk = head
        .id()
        .ancestors()
        .first_parent_only()
        .all()
        .map_err(|e| miette::miette!("Failed to start commit walk for git metadata: {}", e))?;

    let mut paths: HashMap<String, PathWalkStats> = HashMap::new();
    let mut commit_count: u32 = 0;
    let mut truncated = false;

    for res in walk {
        if commit_count as usize >= max_commits {
            truncated = true;
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

        // Author name (NOT committer — see module docs). Used only by
        // collect_git_metadata; PR path history drops it.
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
                match paths.entry(path_str) {
                    std::collections::hash_map::Entry::Vacant(e) => {
                        // First occurrence wins for recency (newest-first walk).
                        e.insert(PathWalkStats {
                            last_commit_at: iso_ts.clone(),
                            author_name: author_name.clone(),
                            churn: 1,
                        });
                    }
                    std::collections::hash_map::Entry::Occupied(mut e) => {
                        e.get_mut().churn = e.get().churn.saturating_add(1);
                    }
                }
            }
        }

        commit_count = commit_count.saturating_add(1);
    }

    // If we stopped because the iterator exhausted at exactly max_commits,
    // check whether more commits remain — but we already break only when
    // about to process beyond max. `truncated` is set when we hit the bound
    // before processing a remaining commit. If the repo has exactly
    // max_commits, truncated stays false (we walked them all without
    // skipping). That matches "hit the DEFAULT_MAX_COMMITS bound" as
    // "stopped early because of the bound".

    Ok(WalkOutcome {
        paths,
        commits_walked: commit_count,
        truncated,
    })
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

/// Lookup for PR path history: exact path, then forward-slash-normalized.
pub fn lookup_path_history<'a>(
    map: &'a HashMap<String, PathHistoryEntry>,
    file_path: &str,
) -> Option<&'a PathHistoryEntry> {
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

    #[test]
    fn test_collect_path_history_no_git_repo_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let result =
            collect_path_history(camino::Utf8Path::from_path(dir.path()).unwrap(), 1000).unwrap();
        assert!(result.by_path.is_empty());
        assert_eq!(result.history_window_commits, 0);
        assert!(!result.history_truncated);
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

    #[test]
    fn test_collect_path_history_churn_and_window() {
        use std::process::Command;

        let dir = tempfile::tempdir().unwrap();
        let dir_path = dir.path();

        Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(dir_path)
            .output()
            .unwrap();

        std::fs::create_dir_all(dir_path.join("src")).unwrap();
        std::fs::write(dir_path.join("src/a.rs"), "v1\n").unwrap();
        let add = Command::new("git")
            .args(["add", "src/a.rs"])
            .current_dir(dir_path)
            .output()
            .unwrap();
        assert!(add.status.success());
        let c1 = Command::new("git")
            .args([
                "-c",
                "user.name=T",
                "-c",
                "user.email=t@t.local",
                "commit",
                "-m",
                "c1",
            ])
            .current_dir(dir_path)
            .output()
            .unwrap();
        assert!(
            c1.status.success(),
            "{}",
            String::from_utf8_lossy(&c1.stderr)
        );

        std::fs::write(dir_path.join("src/a.rs"), "v2\n").unwrap();
        std::fs::write(dir_path.join("src/b.rs"), "b\n").unwrap();
        let add2 = Command::new("git")
            .args(["add", "-A"])
            .current_dir(dir_path)
            .output()
            .unwrap();
        assert!(add2.status.success());
        let c2 = Command::new("git")
            .args([
                "-c",
                "user.name=T",
                "-c",
                "user.email=t@t.local",
                "commit",
                "-m",
                "c2",
            ])
            .current_dir(dir_path)
            .output()
            .unwrap();
        assert!(
            c2.status.success(),
            "{}",
            String::from_utf8_lossy(&c2.stderr)
        );

        let result =
            collect_path_history(camino::Utf8Path::from_path(dir_path).unwrap(), 1000).unwrap();

        assert_eq!(result.history_window_commits, 2);
        assert!(!result.history_truncated);

        let a = result.by_path.get("src/a.rs").expect("src/a.rs present");
        assert_eq!(a.churn, 2, "a.rs touched in both commits");
        assert!(!a.last_commit_at.is_empty());

        let b = result.by_path.get("src/b.rs").expect("src/b.rs present");
        assert_eq!(b.churn, 1);

        // Truncation bound: max_commits = 1 walks only the newest commit.
        let truncated =
            collect_path_history(camino::Utf8Path::from_path(dir_path).unwrap(), 1).unwrap();
        assert_eq!(truncated.history_window_commits, 1);
        assert!(truncated.history_truncated);
        let a_trunc = truncated.by_path.get("src/a.rs").expect("a in window");
        assert_eq!(a_trunc.churn, 1);
    }

    #[test]
    fn test_path_history_has_no_author_fields() {
        // Structural guard: PathHistoryEntry must not grow an author field without
        // an explicit product decision. Serialize-shape is enforced at the PR
        // report boundary; this checks the type surface.
        let entry = PathHistoryEntry {
            churn: 1,
            last_commit_at: "2024-01-01T00:00:00+00:00".into(),
        };
        // Field access only — if an author field is added, this test should be
        // revisited rather than silently carrying names into PR JSON.
        assert_eq!(entry.churn, 1);
        assert!(!entry.last_commit_at.is_empty());
    }
}
