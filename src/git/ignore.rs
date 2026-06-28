use crate::git::GitError;
use camino::Utf8Path;
use miette::Result;
use std::fs;
use std::io::{Read, Write};

use crate::git::{ChangeType, FileChange};
use globset::{Glob, GlobSetBuilder};

pub fn add_to_gitignore(root: &Utf8Path, pattern: &str) -> Result<bool> {
    let ignore_path = root.join(".gitignore");

    if !ignore_path.exists() {
        let mut file = fs::File::create(&ignore_path).map_err(|e| GitError::WriteIgnoreFailed {
            path: ignore_path.to_string(),
            source: e,
        })?;
        let content = format!("{}\n", pattern);
        file.write_all(content.as_bytes())
            .map_err(|e| GitError::WriteIgnoreFailed {
                path: ignore_path.to_string(),
                source: e,
            })?;
        return Ok(true);
    }

    let mut content = String::new();
    let mut file = fs::File::open(&ignore_path).map_err(|e| GitError::ReadIgnoreFailed {
        path: ignore_path.to_string(),
        source: e,
    })?;
    file.read_to_string(&mut content)
        .map_err(|e| GitError::ReadIgnoreFailed {
            path: ignore_path.to_string(),
            source: e,
        })?;

    // Check if pattern is already there
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed == pattern || trimmed == pattern.trim_end_matches('/') {
            return Ok(false);
        }
    }

    // Append pattern, ensuring it starts on a new line if the file is not empty
    let line_ending = if content.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };

    let mut to_append = String::new();
    if !content.is_empty() && !content.ends_with('\n') && !content.ends_with('\r') {
        to_append.push_str(line_ending);
    }
    to_append.push_str(pattern);
    if !pattern.ends_with('\n') && !pattern.ends_with('\r') {
        to_append.push_str(line_ending);
    }

    let mut file = fs::OpenOptions::new()
        .append(true)
        .open(&ignore_path)
        .map_err(|e| GitError::WriteIgnoreFailed {
            path: ignore_path.to_string(),
            source: e,
        })?;

    file.write_all(to_append.as_bytes())
        .map_err(|e| GitError::WriteIgnoreFailed {
            path: ignore_path.to_string(),
            source: e,
        })?;

    Ok(true)
}

/// Filter changes against config `watch.ignore_patterns` using glob matching.
/// By default, it only filters untracked (unstaged Added) files, preserving
/// tracked changes even if they match an ignore pattern.
pub fn filter_ignored_changes(
    changes: Vec<FileChange>,
    ignore_patterns: &[String],
    filter_tracked: bool,
) -> Result<Vec<FileChange>> {
    if ignore_patterns.is_empty() {
        return Ok(changes);
    }
    let mut builder = GlobSetBuilder::new();
    for pattern in ignore_patterns {
        builder.add(
            Glob::new(pattern)
                .map_err(|e| miette::miette!("Invalid glob pattern '{}': {}", pattern, e))?,
        );
    }
    let ignore_set = builder
        .build()
        .map_err(|e| miette::miette!("Failed to build glob set: {}", e))?;
    Ok(changes
        .into_iter()
        .filter(|change| {
            let should_filter = if filter_tracked {
                true
            } else {
                matches!(change.change_type, ChangeType::Added) && !change.is_staged
            };

            if should_filter {
                let path_str = change.path.to_string_lossy().replace('\\', "/");
                if ignore_set.is_match(path_str) {
                    return false;
                }
            }
            true
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_add_to_gitignore_new_file() {
        let tmp = tempdir().unwrap();
        let root = Utf8Path::from_path(tmp.path()).unwrap();

        let changed = add_to_gitignore(root, ".ledgerful/").unwrap();
        assert!(changed);

        let content = fs::read_to_string(root.join(".gitignore")).unwrap();
        assert_eq!(content, ".ledgerful/\n");
    }

    #[test]
    fn test_add_to_gitignore_existing_no_newline() {
        let tmp = tempdir().unwrap();
        let root = Utf8Path::from_path(tmp.path()).unwrap();
        fs::write(root.join(".gitignore"), "target").unwrap();

        let changed = add_to_gitignore(root, ".ledgerful/").unwrap();
        assert!(changed);

        let content = fs::read_to_string(root.join(".gitignore")).unwrap();
        assert_eq!(content, "target\n.ledgerful/\n");
    }

    #[test]
    fn test_add_to_gitignore_idempotent() {
        let tmp = tempdir().unwrap();
        let root = Utf8Path::from_path(tmp.path()).unwrap();
        fs::write(root.join(".gitignore"), "target\n.ledgerful/\n").unwrap();

        let changed = add_to_gitignore(root, ".ledgerful/").unwrap();
        assert!(!changed);

        let content = fs::read_to_string(root.join(".gitignore")).unwrap();
        assert_eq!(content, "target\n.ledgerful/\n");
    }
}
