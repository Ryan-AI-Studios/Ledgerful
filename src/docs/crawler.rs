use camino::{Utf8Path, Utf8PathBuf};
use std::fs;
use std::time::SystemTime;
use tracing::warn;

pub struct DocFile {
    pub path: Utf8PathBuf,
    pub content: String,
    pub last_modified: SystemTime,
}

const DOC_EXTENSIONS: &[&str] = &["md", "txt", "rst", "adoc"];

/// Walk paths relative to `repo_root` and collect all document files.
/// Returns files in sorted order by path (deterministic).
pub fn crawl_docs(repo_root: &Utf8Path, include_globs: &[String]) -> Result<Vec<DocFile>, String> {
    let mut files: Vec<DocFile> = Vec::new();

    for pattern in include_globs {
        let pattern = pattern.trim();
        if pattern.is_empty() {
            continue;
        }

        let search_path = repo_root.join(pattern);

        if search_path.is_file() {
            if has_doc_extension(&search_path) {
                match read_doc_file(&search_path) {
                    Ok(doc) => files.push(doc),
                    Err(e) => warn!("{}", e),
                }
            }
        } else if search_path.is_dir() {
            walk_dir(&search_path, &mut files);
        } else if pattern.contains('*') || pattern.contains('?') {
            walk_glob(repo_root, pattern, &mut files);
        }
        // Skip non-existent paths silently
    }

    // Sort by path for deterministic output
    files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(files)
}

fn has_doc_extension(path: &Utf8Path) -> bool {
    path.extension()
        .map(|e| DOC_EXTENSIONS.contains(&e))
        .unwrap_or(false)
}

fn walk_dir(dir: &Utf8Path, files: &mut Vec<DocFile>) {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) => {
            warn!("Failed to read directory {}: {}", dir, e);
            return;
        }
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let utf8_path = match Utf8Path::from_path(&path) {
            Some(p) => p.to_path_buf(),
            None => continue,
        };

        if utf8_path.is_dir() {
            let dirname = utf8_path.file_name().unwrap_or("");
            if dirname == ".git" || dirname == "target" || dirname == "node_modules" {
                continue;
            }
            walk_dir(&utf8_path, files);
        } else if has_doc_extension(&utf8_path) {
            match read_doc_file(&utf8_path) {
                Ok(doc) => files.push(doc),
                Err(e) => warn!("{}", e),
            }
        }
    }
}

fn walk_glob(repo_root: &Utf8Path, pattern: &str, files: &mut Vec<DocFile>) {
    // Simple glob matching: * matches anything, ? matches one char
    let full_pattern = repo_root.join(pattern).to_string();
    let entries = match glob_match(&full_pattern) {
        Ok(entries) => entries,
        Err(e) => {
            warn!("Invalid glob pattern '{}': {}", pattern, e);
            return;
        }
    };

    for entry in entries {
        let utf8_path = match Utf8Path::from_path(&entry) {
            Some(p) => p.to_path_buf(),
            None => continue,
        };

        if !utf8_path.is_file() || !has_doc_extension(&utf8_path) {
            continue;
        }

        match read_doc_file(&utf8_path) {
            Ok(doc) => files.push(doc),
            Err(e) => warn!("{}", e),
        }
    }
}

/// Minimal glob matching for simple patterns like `docs/*.md` or `docs/**/*.md`.
fn glob_match(pattern: &str) -> Result<Vec<std::path::PathBuf>, String> {
    let pattern = pattern.replace('\\', "/");
    let mut result = Vec::new();

    // Find the last component before the first wildcard to use as base dir
    let wildcard_pos = pattern.find(['*', '?']);
    let base_dir = if let Some(pos) = wildcard_pos {
        if let Some(slash_pos) = pattern[..pos].rfind('/') {
            &pattern[..slash_pos]
        } else {
            "."
        }
    } else {
        // No wildcard, just read the file directly
        let p = std::path::Path::new(&pattern);
        if p.is_file() {
            return Ok(vec![p.to_path_buf()]);
        }
        return Ok(vec![]);
    };

    let after_base = &pattern[base_dir.len() + 1..];

    // Handle ** pattern
    if after_base == "**" || after_base.starts_with("**/") {
        let suffix = if after_base.len() > 2 {
            &after_base[3..]
        } else {
            "*"
        };
        glob_walk_recursive(std::path::Path::new(base_dir), suffix, &mut result);
    } else {
        glob_walk_single(std::path::Path::new(base_dir), after_base, &mut result);
    }

    Ok(result)
}

fn glob_walk_recursive(dir: &std::path::Path, suffix: &str, result: &mut Vec<std::path::PathBuf>) {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if name == ".git" || name == "target" || name == "node_modules" {
                continue;
            }
            glob_walk_recursive(&path, suffix, result);
        } else if path.is_file() {
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if simple_match(name, suffix) {
                result.push(path);
            }
        }
    }
}

fn glob_walk_single(dir: &std::path::Path, pattern: &str, result: &mut Vec<std::path::PathBuf>) {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if path.is_file() && simple_match(name, pattern) {
            result.push(path);
        }
    }
}

fn simple_match(name: &str, pattern: &str) -> bool {
    let name = name.to_lowercase();
    let pattern = pattern.to_lowercase();

    if pattern == "*" {
        return true;
    }

    // Simple glob: only * wildcard
    if let Some(star_pos) = pattern.find('*') {
        let prefix = &pattern[..star_pos];
        let suffix = &pattern[star_pos + 1..];
        name.starts_with(prefix)
            && name.ends_with(suffix)
            && name.len() >= prefix.len() + suffix.len()
    } else {
        name == pattern
    }
}

fn read_doc_file(path: &Utf8Path) -> Result<DocFile, String> {
    let metadata =
        fs::metadata(path).map_err(|e| format!("Failed to read metadata for {}: {}", path, e))?;
    let last_modified = metadata
        .modified()
        .map_err(|e| format!("Failed to get mtime for {}: {}", path, e))?;
    let content =
        fs::read_to_string(path).map_err(|e| format!("Failed to read {}: {}", path, e))?;
    Ok(DocFile {
        path: path.to_path_buf(),
        content,
        last_modified,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn test_crawl_docs_finds_md_files() {
        let tmp = tempdir().unwrap();
        let root = Utf8Path::from_path(tmp.path()).unwrap();

        fs::create_dir_all(root.join("docs")).unwrap();
        fs::write(root.join("docs").join("guide.md"), "# Guide\n\nContent.\n").unwrap();
        fs::write(root.join("docs").join("api.md"), "# API\n\nReference.\n").unwrap();
        fs::write(root.join("docs").join("notes.txt"), "Notes.\n").unwrap();
        fs::write(root.join("docs").join("script.rs"), "fn main() {}").unwrap();
        fs::write(root.join("README.md"), "# README\n").unwrap();

        let include = vec!["docs/".to_string()];
        let files = crawl_docs(root, &include).unwrap();

        // Should find .md and .txt files, but not .rs
        let paths: Vec<&str> = files.iter().map(|f| f.path.file_name().unwrap()).collect();
        assert!(paths.contains(&"guide.md"));
        assert!(paths.contains(&"api.md"));
        assert!(paths.contains(&"notes.txt"));
        assert!(!paths.contains(&"script.rs"));
        // README.md is outside docs/ so should not be found
        assert!(!paths.contains(&"README.md"));
    }

    #[test]
    fn test_crawl_docs_skips_unreadable() {
        let tmp = tempdir().unwrap();
        let root = Utf8Path::from_path(tmp.path()).unwrap();

        fs::create_dir_all(root.join("docs")).unwrap();
        fs::write(root.join("docs").join("good.md"), "# Good\n").unwrap();

        let include = vec!["docs/".to_string(), "nonexistent.md".to_string()];
        let files = crawl_docs(root, &include).unwrap();

        // Should still return the good file
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path.file_name().unwrap(), "good.md");
    }

    #[test]
    fn test_crawl_docs_sorted_by_path() {
        let tmp = tempdir().unwrap();
        let root = Utf8Path::from_path(tmp.path()).unwrap();

        fs::create_dir_all(root.join("docs")).unwrap();
        fs::write(root.join("docs").join("z.md"), "# Z\n").unwrap();
        fs::write(root.join("docs").join("a.md"), "# A\n").unwrap();
        fs::write(root.join("docs").join("m.md"), "# M\n").unwrap();

        let include = vec!["docs/".to_string()];
        let files = crawl_docs(root, &include).unwrap();

        assert_eq!(files.len(), 3);
        assert_eq!(files[0].path.file_name().unwrap(), "a.md");
        assert_eq!(files[1].path.file_name().unwrap(), "m.md");
        assert_eq!(files[2].path.file_name().unwrap(), "z.md");
    }

    #[test]
    fn test_crawl_docs_empty_patterns_returns_empty() {
        let tmp = tempdir().unwrap();
        let root = Utf8Path::from_path(tmp.path()).unwrap();

        let include: Vec<String> = vec![];
        let files = crawl_docs(root, &include).unwrap();
        assert!(files.is_empty());
    }
}
