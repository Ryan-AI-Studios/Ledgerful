use camino::Utf8Path;
use std::path::{Path, PathBuf};
use std::process::Command;

pub mod env_guard;
pub use env_guard::TempEnv;

pub mod sync;
pub use sync::wait_for_condition;

/// RAII guard for temporarily mutating a process environment variable.
pub fn non_interactive() -> TempEnv {
    TempEnv::set("LEDGERFUL_NON_INTERACTIVE", "1")
}

pub struct DirGuard {
    original: PathBuf,
}

impl DirGuard {
    pub fn new(dir: &Path) -> Self {
        let original = std::env::current_dir().unwrap();
        std::env::set_current_dir(dir).unwrap();
        Self { original }
    }

    #[allow(dead_code)]
    pub fn from_utf8<P: AsRef<Utf8Path>>(dir: P) -> Self {
        Self::new(dir.as_ref().as_std_path())
    }
}

impl Drop for DirGuard {
    fn drop(&mut self) {
        let _ = std::env::set_current_dir(&self.original);
    }
}

#[allow(dead_code)]
pub fn setup_git_repo(dir: &Path) {
    git_cmd(dir, &["init"]);
    git_cmd(dir, &["config", "user.email", "test@test.com"]);
    git_cmd(dir, &["config", "user.name", "Test User"]);
}

#[allow(dead_code)]
pub fn git_add_and_commit(dir: &Path, msg: &str) {
    git_cmd(dir, &["add", "-A"]);
    git_cmd(dir, &["commit", "-m", msg]);
}

#[allow(dead_code)]
pub fn git_add_and_commit_no_verify(dir: &Path, msg: &str) {
    git_cmd(dir, &["add", "-A"]);
    git_cmd(dir, &["commit", "--no-verify", "-m", msg]);
}

pub fn git_cmd(dir: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("Failed to execute git command");
    if !output.status.success() {
        panic!(
            "git command failed: {:?}\nstderr: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
