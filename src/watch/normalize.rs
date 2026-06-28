use camino::Utf8PathBuf;
use std::path::Path;

pub fn normalize_event_path(path: &Path, root: &Path) -> Option<Utf8PathBuf> {
    let relative = path.strip_prefix(root).unwrap_or(path);
    let normalized = relative.to_string_lossy().replace('\\', "/");

    #[cfg(target_os = "windows")]
    let normalized = normalized.to_lowercase();

    if normalized.is_empty() {
        None
    } else {
        Utf8PathBuf::from_path_buf(std::path::PathBuf::from(normalized)).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_normalize_relative_path() {
        let root = Path::new("C:/repo");
        let path = Path::new("C:/repo/src/main.rs");
        let normalized = normalize_event_path(path, root).unwrap();
        assert_eq!(normalized.as_str(), "src/main.rs");
    }

    #[test]
    fn test_normalize_backslashes() {
        let root = Path::new("C:\\repo");
        let path = PathBuf::from("C:\\repo\\src\\nested\\file.rs");
        let normalized = normalize_event_path(&path, root).unwrap();
        #[cfg(target_os = "windows")]
        assert_eq!(normalized.as_str(), "src/nested/file.rs");
        #[cfg(not(target_os = "windows"))]
        assert_eq!(normalized.as_str(), "C:/repo/src/nested/file.rs");
    }
}
