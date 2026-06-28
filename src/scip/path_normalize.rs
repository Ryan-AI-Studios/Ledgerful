use crate::util::path::normalize_relative_path;
use std::path::{Path, PathBuf};

/// Normalizes a SCIP document path to a local OS path relative to the repository root.
/// SCIP paths are typically Unix-style and relative to the project root.
pub fn normalize_scip_path(repo_root: &Path, scip_relative_path: &str) -> Result<PathBuf, String> {
    // First, use the existing utility to clean and validate the path.
    // This also ensures it stays within the repo root.
    let normalized_str = normalize_relative_path(repo_root, scip_relative_path)?;

    // Convert back to a PathBuf. On Windows, this will use backslashes.
    // On Unix, it will keep forward slashes.
    Ok(PathBuf::from(normalized_str))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn test_normalize_scip_path() {
        // Use a mock repo root.
        #[cfg(windows)]
        let root = Path::new("C:\\repo");
        #[cfg(not(windows))]
        let root = Path::new("/repo");

        // Simple relative path
        let path = "src/main.rs";
        let normalized = normalize_scip_path(root, path).unwrap();
        assert_eq!(normalized, PathBuf::from("src/main.rs"));

        // Path with dots
        let path = "src/../src/lib.rs";
        let normalized = normalize_scip_path(root, path).unwrap();
        assert_eq!(normalized, PathBuf::from("src/lib.rs"));

        // Path with backslashes (SCIP should have forward but we handle both)
        let path = "src\\util.rs";
        let normalized = normalize_scip_path(root, path).unwrap();
        assert_eq!(normalized, PathBuf::from("src/util.rs"));

        // Traversal attempt
        let path = "../../etc/passwd";
        let result = normalize_scip_path(root, path);
        assert!(result.is_err());
    }
}
