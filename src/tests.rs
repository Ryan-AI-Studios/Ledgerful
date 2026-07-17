use std::path::{Path, PathBuf};

/// RAII guard for temporarily changing the process current directory in tests.
///
/// On construction the original directory is captured, and on drop it is
/// restored. This is isolated to a single module so the `unsafe`
/// `std::env::set_current_dir` calls are not scattered across the test suite.
pub struct DirGuard {
    original: PathBuf,
}

impl DirGuard {
    pub fn new(dir: &Path) -> Self {
        let original = std::env::current_dir().unwrap();
        std::env::set_current_dir(dir).unwrap();
        Self { original }
    }
}

impl Drop for DirGuard {
    fn drop(&mut self) {
        let _ = std::env::set_current_dir(&self.original);
    }
}
