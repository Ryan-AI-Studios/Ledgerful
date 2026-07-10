use crate::git::GitError;
use std::collections::HashMap;
use std::path::Path;

/// Per-file addition/deletion stats for a single committed diff.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileNumstat {
    /// `None` for binary files (git emits `-` `-`).
    pub additions: Option<u64>,
    /// `None` for binary files (git emits `-` `-`).
    pub deletions: Option<u64>,
}

/// Compute per-file addition/deletion stats for a single commit on the
/// committed-diff basis.
///
/// Uses `git show --numstat -z <commit_ref>` so the output is unambiguous for
/// renames and paths containing spaces or tabs.  Returns a map keyed by the
/// **new** path of each changed file; for renames this is the destination path.
/// Binary files map to `FileNumstat { additions: None, deletions: None }`.
///
/// Errors are returned as `GitError::MetadataError`; callers in the commit path
/// should treat missing stats as a best-effort absence (leave columns NULL)
/// and never block the commit.
///
/// Generated/lockfile files are not specially capped or skipped: `git show
/// --numstat` reads pre-computed diff tree metadata (not file contents), so
/// even large lockfiles cost only the diff-entry size.  Pathological cases
/// would only arise from abnormally large diffs, which git itself struggles
/// with — no special handling is needed.
pub fn per_file_numstat(
    repo_root: &Path,
    commit_ref: &str,
) -> Result<HashMap<String, FileNumstat>, GitError> {
    let output = std::process::Command::new("git")
        .args([
            "--no-pager",
            "show",
            "--numstat",
            "-z",
            "--format=",
            commit_ref,
        ])
        .current_dir(repo_root)
        .output()
        .map_err(|e| GitError::MetadataError {
            source: anyhow::anyhow!("Failed to run git show --numstat: {e}"),
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(GitError::MetadataError {
            source: anyhow::anyhow!("git show --numstat failed: {stderr}"),
        });
    }

    parse_numstat_z(&output.stdout)
}

/// Parse NUL-delimited `git --numstat -z` output.
///
/// Git `-z` numstat format (verified against real output):
/// - Normal file: `adds\tdels\tpath\0` — path is in the same record.
/// - Rename: `adds\tdels\t\0old_path\0new_path\0` — the path field in the
///   first record is **empty** (just `adds\tdels\t`), followed by two
///   separate NUL-delimited records: old_path then new_path.
/// - Binary: `-\t-\t...` (same structure, `-` for both counts).
///
/// Paths with tabs cannot be unambiguously represented in numstat (tab is
/// the field delimiter); git itself has this limitation. We use `splitn(3)`
/// so only the first two tabs are field delimiters and any remaining tabs
/// are part of the path — this preserves paths with tabs in the non-rename
/// case as best the format allows.
fn parse_numstat_z(data: &[u8]) -> Result<HashMap<String, FileNumstat>, GitError> {
    let mut map = HashMap::new();
    let records: Vec<&[u8]> = data.split(|&b| b == 0).collect();

    let mut i = 0;
    while i < records.len() {
        let record = records[i];
        if record.is_empty() {
            i += 1;
            continue;
        }

        let (stats, path) = parse_record(record)?;

        if path.is_empty() {
            // Rename: the record's path field was empty. The next two
            // records are old_path and new_path (separate NUL-delimited
            // fields). We key on new_path (the destination).
            if i + 2 < records.len() {
                let new_path = String::from_utf8_lossy(records[i + 2]).replace('\\', "/");
                map.insert(new_path, stats);
                i += 3;
            } else {
                i += 1;
            }
        } else {
            map.insert(path, stats);
            i += 1;
        }
    }

    Ok(map)
}

/// Parse a single numstat record: `adds\tdels\tpath`.
/// Returns (stats, path). For renames the path is empty (the actual paths
/// follow as separate NUL-delimited records). Uses `splitn(3, '\t')` so
/// paths containing tabs are preserved as-is after the second tab.
fn parse_record(record: &[u8]) -> Result<(FileNumstat, String), GitError> {
    let text = String::from_utf8_lossy(record);
    let mut parts = text.splitn(3, '\t');
    let adds = parts.next().ok_or_else(|| GitError::MetadataError {
        source: anyhow::anyhow!("numstat record missing additions field"),
    })?;
    let dels = parts.next().ok_or_else(|| GitError::MetadataError {
        source: anyhow::anyhow!("numstat record missing deletions field"),
    })?;
    let path = parts.next().unwrap_or("");

    let stats = if adds == "-" && dels == "-" {
        FileNumstat {
            additions: None,
            deletions: None,
        }
    } else {
        let additions = adds.parse::<u64>().ok();
        let deletions = dels.parse::<u64>().ok();
        FileNumstat {
            additions,
            deletions,
        }
    };

    Ok((stats, path.replace('\\', "/")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_text_file() {
        let data = b"3\t2\tsrc/main.rs\x00";
        let stats = parse_numstat_z(data).unwrap();
        assert_eq!(
            stats.get("src/main.rs"),
            Some(&FileNumstat {
                additions: Some(3),
                deletions: Some(2),
            })
        );
    }

    #[test]
    fn parse_binary_file() {
        let data = b"-\t-\tassets/icon.png\x00";
        let stats = parse_numstat_z(data).unwrap();
        assert_eq!(
            stats.get("assets/icon.png"),
            Some(&FileNumstat {
                additions: None,
                deletions: None,
            })
        );
    }

    #[test]
    fn parse_rename() {
        // Real git `-z` format: `adds\tdels\t\0old_path\0new_path\0`
        let data = b"1\t1\t\x00old.rs\x00new.rs\x00";
        let stats = parse_numstat_z(data).unwrap();
        assert!(!stats.contains_key("old.rs"));
        assert_eq!(
            stats.get("new.rs"),
            Some(&FileNumstat {
                additions: Some(1),
                deletions: Some(1),
            })
        );
    }

    #[test]
    fn parse_multiple_files() {
        let data = b"3\t2\ta.rs\x001\t0\tb.rs\x00";
        let stats = parse_numstat_z(data).unwrap();
        assert_eq!(stats.len(), 2);
        assert_eq!(
            stats.get("a.rs"),
            Some(&FileNumstat {
                additions: Some(3),
                deletions: Some(2),
            })
        );
        assert_eq!(
            stats.get("b.rs"),
            Some(&FileNumstat {
                additions: Some(1),
                deletions: Some(0),
            })
        );
    }

    #[test]
    fn parse_path_with_spaces() {
        let data = b"5\t0\tpath with spaces/file.txt\x00";
        let stats = parse_numstat_z(data).unwrap();
        assert_eq!(
            stats.get("path with spaces/file.txt"),
            Some(&FileNumstat {
                additions: Some(5),
                deletions: Some(0),
            })
        );
    }

    #[test]
    fn parse_path_with_tabs() {
        // git's --numstat -z format uses NUL as record separator but still
        // uses TAB as the field delimiter within a record. With `splitn(3)`
        // we preserve everything after the second tab as the path, so
        // tab-containing paths are preserved as best the format allows.
        let data = b"4\t1\tpath\twith\ttabs/file.txt\x00";
        let stats = parse_numstat_z(data).unwrap();
        assert_eq!(
            stats.get("path\twith\ttabs/file.txt"),
            Some(&FileNumstat {
                additions: Some(4),
                deletions: Some(1),
            })
        );
    }

    #[test]
    fn parse_mixed_text_binary_rename() {
        // Real git `-z` format: renames emit `adds\tdels\t\0old\0new\0`
        let data = b"10\t5\tfoo.txt\x00-\t-\tbinary.bin\x001\t1\t\x00old.rs\x00new.rs\x00";
        let stats = parse_numstat_z(data).unwrap();
        assert_eq!(
            stats.get("foo.txt"),
            Some(&FileNumstat {
                additions: Some(10),
                deletions: Some(5),
            })
        );
        assert_eq!(
            stats.get("binary.bin"),
            Some(&FileNumstat {
                additions: None,
                deletions: None,
            })
        );
        assert_eq!(
            stats.get("new.rs"),
            Some(&FileNumstat {
                additions: Some(1),
                deletions: Some(1),
            })
        );
        assert_eq!(stats.len(), 3);
    }
}
