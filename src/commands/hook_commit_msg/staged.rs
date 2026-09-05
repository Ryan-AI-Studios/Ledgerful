use crate::state::layout::Layout;
use std::path::{Path, PathBuf};

pub fn canonical_entity(files: &[String]) -> String {
    if files.is_empty() {
        return "unknown".to_string();
    }
    if files.len() == 1 {
        return files[0].clone();
    }

    // Try to find a common directory prefix
    let mut common_prefix = PathBuf::new();
    let first_path = Path::new(&files[0]);

    for component in first_path.components() {
        let next_prefix = common_prefix.join(component);
        let all_match = files.iter().all(|f| Path::new(f).starts_with(&next_prefix));
        if all_match {
            common_prefix = next_prefix;
        } else {
            break;
        }
    }

    let prefix_str = common_prefix.to_string_lossy().to_string();
    if !prefix_str.is_empty() && prefix_str != "." && prefix_str != "/" && prefix_str != "\\" {
        prefix_str.replace("\\", "/")
    } else {
        format!("{} (+{} more)", files[0], files.len() - 1)
    }
}

/// Staged-snapshot capture result carried from commit-msg to post-commit via
/// the pending sidecar.
#[derive(Debug, Clone, Copy)]
pub(super) struct CapturedSnapshot {
    pub snapshot_id: i64,
}

/// Capture a snapshot of the staged (pre-commit) working tree so the
/// post-commit hook has `changed_files` rows to attach diff stats to.
///
/// This is best-effort: the packet is persisted with `head_hash` = current
/// HEAD, and the post-commit hook recomputes stats against the new HEAD.
pub(super) fn capture_staged_snapshot(
    layout: &Layout,
    repo_root: &Path,
) -> Option<CapturedSnapshot> {
    use crate::git::repo::{get_head_info, open_repo};
    use crate::git::status::get_repo_status;
    use crate::impact::orchestrator::map_snapshot_to_packet;
    use crate::state::storage::StorageManager;

    let repo = open_repo(repo_root).ok()?;
    let (head_hash, branch_name) = get_head_info(&repo).ok()?;
    let all_changes = get_repo_status(&repo).ok()?;
    let changes: Vec<_> = all_changes.into_iter().filter(|c| c.is_staged).collect();
    let is_clean = changes.is_empty();

    let snapshot = crate::git::RepoSnapshot {
        head_hash,
        branch_name,
        is_clean,
        changes,
    };

    let mut packet = map_snapshot_to_packet(snapshot, repo_root).ok()?;
    packet.finalize();
    crate::impact::redact::redact_secrets(&mut packet);

    let storage = match StorageManager::init_with_layout(layout) {
        Ok(s) => s,
        Err(e) => {
            tracing::debug!(
                "capture_staged_snapshot: StorageManager::init_with_layout failed: {e}"
            );
            return None;
        }
    };
    let snapshot_id = match storage.save_packet(&packet) {
        Ok(id) => id,
        Err(e) => {
            tracing::debug!("capture_staged_snapshot: save_packet failed: {e}");
            return None;
        }
    };
    tracing::debug!("capture_staged_snapshot: saved snapshot_id={snapshot_id}");

    Some(CapturedSnapshot { snapshot_id })
}

pub(super) fn get_staged_files(repo_root: &Path) -> Vec<String> {
    let output = crate::git::git_command().ok().and_then(|mut cmd| {
        cmd.args(["diff", "--name-only", "--cached"])
            .current_dir(repo_root)
            .output()
            .ok()
    });

    if let Some(out) = output
        && out.status.success()
    {
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    } else {
        Vec::new()
    }
}
