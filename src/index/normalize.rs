use std::path::Path;

pub fn normalize_repo_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}
