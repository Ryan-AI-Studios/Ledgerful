use std::path::PathBuf;

pub fn which(executable: &str) -> Option<PathBuf> {
    which::which(executable).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_which_finds_cargo() {
        let path = which("cargo");
        assert!(
            path.is_some(),
            "cargo should be found in PATH on all platforms"
        );
    }

    #[test]
    fn test_which_does_not_find_nonexistent() {
        let path = which("doesnotexist_binary_12345");
        assert!(path.is_none(), "should return None for nonexistent binary");
    }
}
