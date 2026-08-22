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

/// `git add -A` + commit only when the worktree is dirty (no-op if clean).
#[allow(dead_code)]
pub fn git_add_and_commit_if_dirty(dir: &Path, msg: &str) {
    let status = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(dir)
        .output()
        .expect("git status");
    assert!(status.status.success(), "git status failed");
    if !status.stdout.is_empty() {
        git_add_and_commit(dir, msg);
    }
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

/// Run the ledgerful binary as a subprocess and capture stdout/stderr separately.
/// Required for paths that call `process::exit` (e.g. `index --check --strict`).
#[allow(dead_code)]
pub fn run_cli(dir: &Path, args: &[&str]) -> (String, String, i32) {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_ledgerful"))
        .args(args)
        .current_dir(dir)
        .output()
        .expect("failed to run ledgerful");
    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
        output.status.code().unwrap_or(-1),
    )
}
