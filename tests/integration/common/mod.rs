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

/// Repo root one directory under `tmp`, so sibling discovery does not walk `%TEMP%`.
///
/// A fixture whose git root is the tempdir itself makes `scan --impact`'s federated
/// stage scan every sibling under the operator temp directory. On this host that
/// walk exceeds the 25s overall budget, the scan exits 0, and `should_skip_persist`
/// leaves `latest-impact.json` unwritten.
#[allow(dead_code)]
pub fn nested_repo_root(tmp: &Path) -> PathBuf {
    let root = tmp.join("repo");
    std::fs::create_dir_all(&root).expect("create nested repo root");
    root
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
    run_cli_env(dir, args, &[])
}

/// Environment names removed from an isolated child (0554 DoD-isolation census).
///
/// Sources: the provider key/endpoint/model lookups in
/// `src/config/model/env.rs`, `GEMINI_API_KEY` in
/// `src/commands/config_verify.rs`, `LEDGERFUL_CONFIG_HOME` in
/// `src/state/rollup.rs`, and `LEDGERFUL_STATE_DIR` in `src/state/layout.rs`
/// (an inherited state override would redirect writes out of the fixture).
/// Template/home names are *set* by [`isolate_cli_env`] instead of removed.
pub const ISOLATED_REMOVED_ENV: &[&str] = &[
    "GEMINI_API_KEY",
    "OLLAMA_CLOUD_API_KEY",
    "OLLAMA_API_KEY",
    "OLLAMA_CLOUD_URL",
    "OLLAMA_CLOUD_MODEL",
    "LEDGERFUL_LOCAL_MODEL_URL",
    "LEDGERFUL_LOCAL_EMBEDDING_URL",
    "LEDGERFUL_LOCAL_GENERATION_URL",
    "LEDGERFUL_EMBEDDING_MODEL",
    "LEDGERFUL_GENERATION_MODEL",
    "LEDGERFUL_RERANK_MODEL",
    "LEDGERFUL_EMBEDDING_DIMENSIONS",
    "LEDGERFUL_LOCAL_CONTEXT_WINDOW",
    "LEDGERFUL_DISABLE_HNSW",
    "LEDGERFUL_BRIDGE",
    "LEDGERFUL_CONFIG_HOME",
    "LEDGERFUL_STATE_DIR",
];

/// Isolate a child `ledgerful` command from the host template and provider keys
/// (0554 DoD-isolation). Homes point at `home`, the default template points at
/// a missing file inside it, and the [`ISOLATED_REMOVED_ENV`] census is removed.
/// Callers keep their own explicit `LEDGERFUL_DEFAULT_CONFIG` by setting it
/// after this.
#[allow(dead_code)]
pub fn isolate_cli_env(cmd: &mut std::process::Command, home: &Path) {
    for key in ISOLATED_REMOVED_ENV {
        cmd.env_remove(key);
    }
    cmd.env("USERPROFILE", home).env("HOME", home).env(
        "LEDGERFUL_DEFAULT_CONFIG",
        home.join("missing-template.toml"),
    );
}

/// Create (if needed) and return `dir/home`, the synthetic home for children
/// run with `dir` as their working directory.
#[allow(dead_code)]
pub fn synthetic_home(dir: &Path) -> std::path::PathBuf {
    let home = dir.join("home");
    std::fs::create_dir_all(&home).expect("create synthetic home");
    home
}

/// Build a `ledgerful` child command isolated from the host template and keys.
#[allow(dead_code)]
pub fn isolated_command(bin: &str, home: &Path) -> std::process::Command {
    let mut cmd = std::process::Command::new(bin);
    isolate_cli_env(&mut cmd, home);
    cmd
}

/// [`run_cli`] with the child isolated by [`isolate_cli_env`].
#[allow(dead_code)]
pub fn run_cli_isolated(dir: &Path, args: &[&str]) -> (String, String, i32) {
    run_cli_isolated_env(dir, args, &[])
}

/// [`run_cli_env`] with the child isolated by [`isolate_cli_env`]. Explicit
/// `extra` entries are applied after isolation and may override it.
#[allow(dead_code)]
pub fn run_cli_isolated_env(
    dir: &Path,
    args: &[&str],
    extra: &[(&str, &str)],
) -> (String, String, i32) {
    let home = synthetic_home(dir);
    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_ledgerful"));
    cmd.args(args).current_dir(dir);
    isolate_cli_env(&mut cmd, &home);
    for (k, v) in extra {
        cmd.env(k, v);
    }
    let output = cmd.output().expect("failed to run ledgerful");
    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
        output.status.code().unwrap_or(-1),
    )
}

/// Like [`run_cli`] but sets extra env on the **child** process (not parent TempEnv).
#[allow(dead_code)]
pub fn run_cli_env(dir: &Path, args: &[&str], extra: &[(&str, &str)]) -> (String, String, i32) {
    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_ledgerful"));
    cmd.args(args).current_dir(dir);
    for (k, v) in extra {
        cmd.env(k, v);
    }
    let output = cmd.output().expect("failed to run ledgerful");
    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
        output.status.code().unwrap_or(-1),
    )
}

/// Working-tree `impact` with `--timeout 0` so persist tests keep a durable
/// packet after 0419's default 25s Instant (0374 skip-on-budget).
#[allow(dead_code)]
pub fn execute_impact_unbounded() -> miette::Result<()> {
    ledgerful::commands::impact::execute_impact_with_opts(
        false,
        false,
        false,
        false,
        false,
        None,
        None,
        Vec::new(),
        false,
        Some(0),
        None,
    )
}

/// Working-tree `scan --impact` with `--timeout 0` for persist-required fixtures.
#[allow(dead_code)]
pub fn execute_scan_impact_unbounded() -> miette::Result<()> {
    ledgerful::commands::scan::execute_scan_with_opts(
        true,
        false,
        false,
        None,
        None,
        None,
        None,
        None,
        Vec::new(),
        false,
        None,
        false,
        Some(0),
    )
}
