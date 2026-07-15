use path_clean::PathClean;
use std::path::Path;

/// Securely normalizes a path relative to a repository root.
/// Does NOT depend on canonicalize (filesystem access), making it safe for
/// non-existent or deleted files.
pub fn normalize_relative_path(repo_root: &Path, input: &str) -> Result<String, String> {
    // Reject absolute and UNC-style inputs before joining. On Unix,
    // `PathClean::clean()` converts `\\server\share` to `/server/share`,
    // which escapes the repo root. On Windows, `Path::push` replaces the
    // base when the input is absolute (drive letter or UNC). Reject both
    // patterns up front so neither platform can escape.
    if input.starts_with('/') || input.starts_with('\\') {
        return Err(format!(
            "Security violation: path '{}' is outside the repository root (absolute or UNC prefix)",
            input
        ));
    }

    let mut path = repo_root.to_path_buf();
    path.push(input);

    // Lexically clean the path (resolves .. without filesystem access)
    let cleaned = path.clean();

    // Ensure the path is still within the repo_root
    let relative = cleaned.strip_prefix(repo_root).map_err(|_| {
        format!(
            "Security violation: path '{}' is outside the repository root",
            input
        )
    })?;

    // Normalize to forward slashes for internal storage
    Ok(relative.to_string_lossy().replace('\\', "/"))
}

/// Filesystem-level containment check for a resolved path against a root.
/// Canonicalizes both `resolved` and `repo_root`, then verifies `resolved`
/// starts with `repo_root`. Skips symlinks to prevent symlink escapes.
/// This is intended for use at filesystem use sites, complementing the lexical
/// `normalize_relative_path` which works for non-existent paths.
pub fn ensure_path_within_root(repo_root: &Path, resolved: &Path) -> Result<(), String> {
    let root_metadata = std::fs::symlink_metadata(repo_root).map_err(|e| {
        format!(
            "Security violation: failed to inspect repository root '{}': {e}",
            repo_root.display()
        )
    })?;
    if root_metadata.is_symlink() {
        return Err(format!(
            "Security violation: repository root '{}' is a symlink",
            repo_root.display()
        ));
    }

    let resolved_metadata = std::fs::symlink_metadata(resolved).map_err(|e| {
        format!(
            "Security violation: failed to inspect path '{}': {e}",
            resolved.display()
        )
    })?;
    if resolved_metadata.is_symlink() {
        return Err(format!(
            "Security violation: path '{}' is a symlink",
            resolved.display()
        ));
    }

    let canonical_root = std::fs::canonicalize(repo_root).map_err(|e| {
        format!(
            "Security violation: failed to canonicalize repository root '{}': {e}",
            repo_root.display()
        )
    })?;
    let canonical_resolved = std::fs::canonicalize(resolved).map_err(|e| {
        format!(
            "Security violation: failed to canonicalize path '{}': {e}",
            resolved.display()
        )
    })?;

    if !canonical_resolved.starts_with(&canonical_root) {
        return Err(format!(
            "Security violation: resolved path '{}' is outside the repository root '{}'",
            canonical_resolved.display(),
            canonical_root.display()
        ));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_relative_path() {
        let root = Path::new("/repo");

        assert_eq!(
            normalize_relative_path(root, "src/main.rs").unwrap(),
            "src/main.rs"
        );
        assert_eq!(
            normalize_relative_path(root, "./src/../src/main.rs").unwrap(),
            "src/main.rs"
        );

        // Traversal attempt
        let result = normalize_relative_path(root, "../outside.rs");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("outside the repository root"));

        // Windows-style (even on Unix)
        assert_eq!(
            normalize_relative_path(root, "src\\util.rs").unwrap(),
            "src/util.rs"
        );
    }

    #[test]
    fn ensure_path_within_root_accepts_files_inside_temp_directory() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let child = root.join("child.txt");
        std::fs::write(&child, "contents").unwrap();

        assert!(ensure_path_within_root(root, &child).is_ok());
    }

    #[test]
    fn ensure_path_within_root_rejects_file_outside_root() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let outside = tempfile::tempdir().unwrap();
        let target = outside.path().join("outside.txt");
        std::fs::write(&target, "contents").unwrap();

        assert!(ensure_path_within_root(root, &target).is_err());
    }

    #[test]
    fn ensure_path_within_root_rejects_symlink_escape() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let outside = tempfile::tempdir().unwrap();
        let target = root.join("link");

        let symlink_result = {
            #[cfg(windows)]
            {
                std::os::windows::fs::symlink_file(outside.path().join("x.txt"), &target)
            }
            #[cfg(not(windows))]
            {
                std::os::unix::fs::symlink(outside.path().join("x.txt"), &target)
            }
        };

        if symlink_result.is_err() {
            // Symlink creation requires elevated privileges on some Windows
            // configurations. Skip the test gracefully if the OS rejects it.
            return;
        }

        assert!(ensure_path_within_root(root, &target).is_err());
    }
}
