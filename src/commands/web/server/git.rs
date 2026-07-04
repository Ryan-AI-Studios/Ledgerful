//! Git diff history analysis helpers for the web dashboard.

use crate::commands::web::types::ChangeResponse;
use crate::git::ChangeType;
use crate::git::repo::open_repo;
use crate::git::status::get_repo_status;
use crate::state::layout::Layout;
use miette::{Result, miette};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

pub(crate) fn fetch_changes(
    layout: &Layout,
    days: u64,
    include_working_tree: bool,
) -> Result<Vec<ChangeResponse>> {
    let repo = match open_repo(layout.root.as_std_path()) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("No git repository available for /api/changes: {}", e);
            return Ok(Vec::new());
        }
    };

    let mut changes = Vec::new();

    if include_working_tree {
        let file_changes = get_repo_status(&repo)
            .map_err(|e| miette!("Failed to get repository status: {}", e))?;
        if !file_changes.is_empty() {
            let file_count = file_changes.len();
            let (additions, deletions) = count_worktree_diff_stats(
                &repo,
                &file_changes
                    .iter()
                    .map(|c| c.path.to_string_lossy().to_string())
                    .collect::<Vec<_>>(),
            );
            let summary = file_changes
                .first()
                .map(|c| {
                    format!(
                        "{}: {}",
                        status_label(&c.change_type),
                        c.path.to_string_lossy()
                    )
                })
                .unwrap_or_else(|| "Uncommitted changes".to_string());
            changes.push(ChangeResponse {
                id: "working-tree".to_string(),
                path: if file_count == 1 {
                    file_changes[0].path.to_string_lossy().to_string()
                } else {
                    format!("{} files", file_count)
                },
                status: "Uncommitted".to_string(),
                summary,
                author: current_user(),
                time_ago: "now".to_string(),
                file_count,
                additions,
                deletions,
                risk: "MEDIUM".to_string(),
            });
        }
    }

    if days == 0 {
        return Ok(changes);
    }

    let max_commits = 50;
    let commit_changes = fetch_recent_commits(&repo, days, max_commits)
        .map_err(|e| miette!("Failed to walk recent commits: {}", e))?;
    changes.extend(commit_changes);

    Ok(changes)
}

fn status_label(change_type: &ChangeType) -> &str {
    match change_type {
        ChangeType::Added => "Added",
        ChangeType::Modified => "Modified",
        ChangeType::Deleted => "Deleted",
        ChangeType::Renamed { .. } => "Renamed",
    }
}

pub(crate) fn current_user() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_else(|_| "unknown".to_string())
}

fn count_worktree_diff_stats(repo: &gix::Repository, _paths: &[String]) -> (usize, usize) {
    // Single `git diff HEAD --numstat` call instead of one per file.
    let repo_root = repo.workdir().unwrap_or(repo.path());
    let mut additions = 0usize;
    let mut deletions = 0usize;
    let output = Command::new("git")
        .args(["--no-pager", "diff", "HEAD", "--numstat"])
        .current_dir(repo_root)
        .output();
    if let Ok(output) = output
        && output.status.success()
    {
        let text = String::from_utf8_lossy(&output.stdout);
        for line in text.lines() {
            let mut parts = line.split('\t');
            if let (Some(a), Some(d)) = (
                parts.next().and_then(|s| s.parse::<usize>().ok()),
                parts.next().and_then(|s| s.parse::<usize>().ok()),
            ) {
                additions += a;
                deletions += d;
            }
        }
    }
    (additions, deletions)
}

fn fetch_recent_commits(
    repo: &gix::Repository,
    days: u64,
    max_commits: usize,
) -> Result<Vec<ChangeResponse>> {
    let head = repo
        .head_commit()
        .map_err(|e| miette!("Failed to read HEAD: {e}"))?;
    let walk = head
        .id()
        .ancestors()
        .first_parent_only()
        .all()
        .map_err(|e| miette!("Failed to walk git history: {e}"))?;

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let cutoff = now.saturating_sub(days * 86400);

    let mut changes = Vec::new();

    // Batch-fetch numstat for all commits in a single git subprocess instead of
    // spawning one `git diff --numstat` per commit (was 250 subprocesses).
    let head_oid = head.id().to_string();
    let commit_stats = batch_numstat(repo, &head_oid, max_commits, cutoff);

    for res in walk {
        if changes.len() >= max_commits {
            break;
        }
        let info = match res {
            Ok(info) => info,
            Err(e) => {
                tracing::warn!("Failed to retrieve commit info during changes walk: {e}");
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

        let commit_time = commit
            .time()
            .map_err(|e| miette!("Failed to read commit time for {}: {e}", info.id()))?
            .seconds as u64;
        if commit_time < cutoff {
            break;
        }

        if commit.parent_ids().count() > 1 {
            continue; // skip merge commits
        }

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
                _ => {
                    tracing::warn!(
                        "Failed to retrieve parent tree for {}; using empty tree",
                        info.id()
                    );
                    repo.empty_tree()
                }
            },
            None => repo.empty_tree(),
        };

        let diff = match repo.diff_tree_to_tree(Some(&parent_tree), Some(&current_tree), None) {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!("Failed to diff tree for {}: {e}", info.id());
                continue;
            }
        };

        let mut files = Vec::new();
        for change in diff {
            let path = match change {
                gix::object::tree::diff::ChangeDetached::Addition { location, .. }
                | gix::object::tree::diff::ChangeDetached::Deletion { location, .. }
                | gix::object::tree::diff::ChangeDetached::Modification { location, .. } => {
                    String::from_utf8_lossy(&location).replace('\\', "/")
                }
                gix::object::tree::diff::ChangeDetached::Rewrite {
                    location,
                    source_location,
                    ..
                } => {
                    let src = String::from_utf8_lossy(&source_location).replace('\\', "/");
                    let dst = String::from_utf8_lossy(&location).replace('\\', "/");
                    files.push(src);
                    dst
                }
            };
            if !path.is_empty() {
                files.push(path);
            }
        }

        // Diff stats from the batched numstat map (no per-commit subprocess).
        let commit_id = info.id().to_string();
        let (additions, deletions) = commit_stats.get(&commit_id).copied().unwrap_or((0, 0));

        let file_count = files.len();
        let summary = commit
            .message_raw()
            .map(|m| String::from_utf8_lossy(m.as_ref()).to_string())
            .unwrap_or_default()
            .lines()
            .next()
            .unwrap_or("")
            .to_string();
        let author = commit
            .author()
            .map(|a| a.name.to_string())
            .unwrap_or_else(|_| "unknown".to_string());
        let risk = if additions.saturating_add(deletions) > 100 {
            "HIGH"
        } else if additions.saturating_add(deletions) > 20 {
            "MEDIUM"
        } else {
            "LOW"
        };

        changes.push(ChangeResponse {
            id: commit_id[..commit_id.len().min(8)].to_string(),
            path: if file_count == 1 {
                files.into_iter().next().unwrap_or_default()
            } else {
                format!("{} files", file_count)
            },
            status: "Committed".to_string(),
            summary: if summary.is_empty() {
                "(no message)".to_string()
            } else {
                summary
            },
            author,
            time_ago: format_time_ago(commit_time),
            file_count,
            additions,
            deletions,
            risk: risk.to_string(),
        });
    }

    Ok(changes)
}

/// Batch-fetch addition/deletion stats for up to `max_commits` commits in a
/// single `git log --numstat` subprocess, avoiding N individual `git diff`
/// calls. Returns a map of full commit hash → (additions, deletions).
fn batch_numstat(
    repo: &gix::Repository,
    head_oid: &str,
    max_commits: usize,
    cutoff: u64,
) -> std::collections::HashMap<String, (usize, usize)> {
    let repo_root = repo.workdir().unwrap_or(repo.path());
    let mut stats = std::collections::HashMap::new();

    let output = Command::new("git")
        .args([
            "--no-pager",
            "log",
            "--numstat",
            "--format=COMMIT:%H|%at",
            "--no-merges",
            &format!("-n{max_commits}"),
            head_oid,
        ])
        .current_dir(repo_root)
        .output();

    let Ok(output) = output else {
        tracing::warn!("batch_numstat: git log subprocess failed");
        return stats;
    };
    if !output.status.success() {
        tracing::warn!("batch_numstat: git log exited non-zero");
        return stats;
    }

    let text = String::from_utf8_lossy(&output.stdout);
    let mut current_hash: Option<String> = None;
    let mut current_time: Option<u64> = None;
    let mut current_adds = 0usize;
    let mut current_dels = 0usize;

    for line in text.lines() {
        if let Some(meta) = line.strip_prefix("COMMIT:") {
            // Save the previous commit's accumulated stats.
            if let Some(hash) = current_hash.take() {
                let time = current_time.unwrap_or(0);
                if time >= cutoff {
                    stats.insert(hash, (current_adds, current_dels));
                }
                current_adds = 0;
                current_dels = 0;
            }
            let mut parts = meta.split('|');
            current_hash = parts.next().map(|s| s.to_string());
            current_time = parts.next().and_then(|s| s.parse::<u64>().ok());
        } else if !line.is_empty() {
            // numstat line: "additions\tdeletions\tpath"
            let mut parts = line.split('\t');
            if let (Some(a), Some(d)) = (
                parts.next().and_then(|s| s.parse::<usize>().ok()),
                parts.next().and_then(|s| s.parse::<usize>().ok()),
            ) {
                current_adds += a;
                current_dels += d;
            }
        }
    }
    // Flush the last commit.
    if let Some(hash) = current_hash {
        let time = current_time.unwrap_or(0);
        if time >= cutoff {
            stats.insert(hash, (current_adds, current_dels));
        }
    }

    stats
}

fn format_time_ago(commit_time: u64) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let seconds = now.saturating_sub(commit_time);
    if seconds < 60 {
        "just now".to_string()
    } else if seconds < 3600 {
        format!("{}m ago", seconds / 60)
    } else if seconds < 86400 {
        format!("{}h ago", seconds / 3600)
    } else if seconds < 30 * 86400 {
        format!("{}d ago", seconds / 86400)
    } else {
        format!("{}mo ago", seconds / (30 * 86400))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use camino::Utf8Path;
    use std::process::Command;
    use tempfile::tempdir;

    fn init_git_repo_with_commit(root: &std::path::Path) {
        let run = |args: &[&str]| {
            assert!(
                Command::new("git")
                    .args(args)
                    .current_dir(root)
                    .status()
                    .unwrap()
                    .success(),
                "git {} failed",
                args.join(" ")
            );
        };
        run(&["init", "--quiet"]);
        run(&["config", "user.email", "test@example.com"]);
        run(&["config", "user.name", "Test User"]);
        std::fs::write(root.join("marker.txt"), "hello\n").unwrap();
        run(&["add", "."]);
        run(&["commit", "--quiet", "-m", "initial commit"]);
    }

    #[test]
    fn fetch_changes_days_filter() {
        let tmp = tempdir().unwrap();
        let root = Utf8Path::from_path(tmp.path()).unwrap();
        let layout = Layout::new(root);
        init_git_repo_with_commit(layout.root.as_std_path());

        let within_window = fetch_changes(&layout, 1, false).unwrap();
        assert_eq!(
            within_window.len(),
            1,
            "commit within 1 day should be returned"
        );

        let before_cutoff = fetch_changes(&layout, 0, false).unwrap();
        assert!(
            before_cutoff.is_empty(),
            "commit before 0-day cutoff should be excluded"
        );
    }

    #[test]
    fn fetch_changes_includes_commit_metadata() {
        let tmp = tempdir().unwrap();
        let root = Utf8Path::from_path(tmp.path()).unwrap();
        let layout = Layout::new(root);
        init_git_repo_with_commit(layout.root.as_std_path());

        let changes = fetch_changes(&layout, 1, false).unwrap();
        let first = changes.first().unwrap();
        assert_eq!(first.status, "Committed");
        assert_eq!(first.author, "Test User");
        assert!(!first.summary.is_empty());
    }
}
