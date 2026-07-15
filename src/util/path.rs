use path_clean::PathClean;
use std::path::Path;
/// Securely normalizes a path relative to a repository root.
/// Does NOT depend on canonicalize (filesystem access), making it safe for
/// non-existent or deleted files.
pub fn normalize_relative_path(repo_root: &Path, input: &str) -> Result<String, String> {
    // Handle inputs that may be absolute or contain backslashes. Ledgerful
    // stores paths with forward slashes on every platform, so validation must
    // use those same separator semantics. We handle this by:
    // 1. If the input starts with a backslash (UNC) or a Windows drive
    //    letter (e.g. `C:`), reject it — these are never valid relative
    //    paths on any platform and always escape on Linux via path_clean.
    // 2. For forward-slash absolute paths (`/tmp/...`), allow them only if
    //    they resolve inside the repo root (the strip_prefix check below
    //    catches escapes; on Windows `Path::push` replaces the base, which
    //    also fails the strip_prefix check).
    if input.starts_with('\\') {
        return Err(format!(
            "Security violation: path '{}' is outside the repository root (UNC or backslash prefix)",
            input
        ));
    }

    // Normalize separators before constructing and validating the path. Doing
    // this after the containment check is unsafe on Unix: a literal backslash
    // can pass validation and then become a leading slash in the returned
    // value (for example, `a../../\\`).
    let normalized_input = input.replace('\\', "/");
    // Reject Windows drive-letter paths on non-Windows platforms only.
    // On Linux/macOS, separator normalization turns `D:\..\..` into
    // `D:/../..`, which must not be interpreted as repo-relative. On
    // Windows, `Path::push` handles drive letters correctly by replacing
    // the base, and `strip_prefix` catches escapes.
    #[cfg(not(windows))]
    if input.len() >= 2 && input.as_bytes()[0].is_ascii_alphabetic() && input.as_bytes()[1] == b':'
    {
        return Err(format!(
            "Security violation: path '{}' is outside the repository root (drive-letter prefix)",
            input
        ));
    }

    // If the input is already an absolute path inside the repo root (e.g.
    // `/tmp/repo/src/main.rs` when repo_root is `/tmp/repo`), strip the
    // root prefix before cleaning to avoid Path::push replacing the base.
    let path = if normalized_input.starts_with('/') {
        let input_path = Path::new(&normalized_input);
        if input_path.starts_with(repo_root) {
            let stripped = input_path.strip_prefix(repo_root).map_err(|_| {
                format!(
                    "Security violation: path '{}' is outside the repository root",
                    input
                )
            })?;
            repo_root.join(stripped)
        } else {
            // Absolute path outside repo root — push will replace the base
            // on Windows, and clean will keep it absolute on Unix. The
            // strip_prefix check below will catch it.
            repo_root.join(&normalized_input)
        }
    } else {
        let mut p = repo_root.to_path_buf();
        p.push(&normalized_input);
        p
    };

    // Lexically clean the path (resolves .. without filesystem access)
    let cleaned = path.clean();

    // Ensure the path is still within the repo_root. On some platforms
    // `clean()` may treat backslashes as separators, causing `..` traversal
    // to escape. The strip_prefix check catches absolute escapes.
    let relative = cleaned.strip_prefix(repo_root).map_err(|_| {
        format!(
            "Security violation: path '{}' is outside the repository root",
            input
        )
    })?;

    // Additional guard: the relative path must not start with `..` —
    // `strip_prefix` can succeed on some platforms even when the cleaned
    // path resolves above the root via backslash-as-separator behavior.
    let relative_str = relative.to_string_lossy();
    if relative_str.starts_with("..") {
        return Err(format!(
            "Security violation: path '{}' is outside the repository root (traversal in cleaned path)",
            input
        ));
    }

    // Normalize to forward slashes for internal storage
    Ok(relative_str.replace('\\', "/"))
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
