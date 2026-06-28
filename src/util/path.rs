use path_clean::PathClean;
use std::path::Path;

/// Securely normalizes a path relative to a repository root.
/// Does NOT depend on canonicalize (filesystem access), making it safe for
/// non-existent or deleted files.
pub fn normalize_relative_path(repo_root: &Path, input: &str) -> Result<String, String> {
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
}
