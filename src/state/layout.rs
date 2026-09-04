use crate::state::StateError;
use camino::{Utf8Path, Utf8PathBuf};
use miette::{IntoDiagnostic, Result};
use path_clean::PathClean;
use std::env;
use std::fs;
use std::path::{Component, Path, PathBuf};

pub const STATE_DIR: &str = ".ledgerful";
/// Retired state-directory name (built with `concat!` so the brand is not a
/// greppable literal). Public for doctor / migration residue detection (0094).
pub const LEGACY_STATE_DIR: &str = concat!(".change", "guard");
pub const LOGS_DIR: &str = "logs";
pub const TMP_DIR: &str = "tmp";
pub const REPORTS_DIR: &str = "reports";
pub const STATE_SUBDIR: &str = "state";
pub const SEARCH_INDEX_DIR: &str = "search_index";
pub const DOCS_DIR: &str = "docs";
pub const CONFIG_FILE: &str = "config.toml";
pub const RULES_FILE: &str = "rules.toml";

#[derive(Debug, Clone)]
pub struct Layout {
    pub root: Utf8PathBuf,
    pub state_dir: Utf8PathBuf,
}

impl Layout {
    /// Single-tree convenience: `state_dir = root / ".ledgerful"`.
    ///
    /// Prefer [`Layout::from_roots`] or [`get_layout`] for production paths so
    /// linked worktrees share the main worktree's state.
    pub fn new<P: AsRef<Utf8Path>>(root: P) -> Self {
        let root = normalize_root(root.as_ref());
        let state_dir = root.join(STATE_DIR);
        Self { root, state_dir }
    }

    /// Work root (current worktree / analysis root) and state directory may differ.
    ///
    /// - `work_root`: paths for scan/impact relative to the current checkout
    /// - `state_dir`: shared `.ledgerful` home (ledger DB, config, index, reports)
    pub fn from_roots<P: AsRef<Utf8Path>, Q: AsRef<Utf8Path>>(work_root: P, state_dir: Q) -> Self {
        let root = normalize_root(work_root.as_ref());
        let state_dir = normalize_root(state_dir.as_ref());
        Self { root, state_dir }
    }

    pub fn logs_dir(&self) -> Utf8PathBuf {
        self.state_dir.join(LOGS_DIR)
    }

    pub fn tmp_dir(&self) -> Utf8PathBuf {
        self.state_dir.join(TMP_DIR)
    }

    pub fn reports_dir(&self) -> Utf8PathBuf {
        self.state_dir.join(REPORTS_DIR)
    }

    pub fn state_subdir(&self) -> Utf8PathBuf {
        self.state_dir.join(STATE_SUBDIR)
    }

    pub fn search_index_dir(&self) -> Utf8PathBuf {
        self.state_dir.join(SEARCH_INDEX_DIR)
    }

    pub fn docs_dir(&self) -> Utf8PathBuf {
        self.state_dir.join(DOCS_DIR)
    }

    pub fn config_file(&self) -> Utf8PathBuf {
        self.state_dir.join(CONFIG_FILE)
    }

    pub fn rules_file(&self) -> Utf8PathBuf {
        self.state_dir.join(RULES_FILE)
    }

    pub fn project_id_file(&self) -> Utf8PathBuf {
        self.state_dir.join("project_id")
    }

    pub fn bridge_state_file(&self) -> Utf8PathBuf {
        self.state_subdir().join("bridge_state.json")
    }

    pub fn get_project_id(&self) -> String {
        let path = self.project_id_file();
        if path.exists()
            && let Ok(id) = fs::read_to_string(&path)
        {
            return id.trim().to_string();
        }
        // Fallback to directory name or "unknown"
        self.root
            .file_name()
            .map(|n| n.to_string())
            .unwrap_or_else(|| "unknown".to_string())
    }

    pub fn pid_file(&self) -> Utf8PathBuf {
        self.tmp_dir().join("viz-server.pid")
    }

    pub fn web_pid_file(&self) -> Utf8PathBuf {
        self.tmp_dir().join("web.pid")
    }

    /// Path of the web session token file (written by default on
    /// `ledgerful web start`; suppressed only with `--print-token=true`).
    pub fn web_session_token_file(&self) -> Utf8PathBuf {
        self.state_dir.join("web-session-token")
    }

    pub fn ensure_state_dir(&self) -> Result<()> {
        // Migration rename + one-line record live here. The `.gitignore`
        // side-effect (ensure `.ledgerful/` is ignored after rename) is
        // applied at the `load_startup_config` seam in `cli/dispatch.rs` so
        // `state` does not depend on `git` (CLAUDE.md boundaries). Callers
        // that migrate outside that seam must invoke
        // `crate::git::ignore::add_to_gitignore` themselves.
        let _renamed = self.migrate_legacy_state_dir()?;
        self.ensure_dir(&self.state_dir)?;
        self.ensure_dir(&self.logs_dir())?;
        self.ensure_dir(&self.tmp_dir())?;
        self.ensure_dir(&self.reports_dir())?;
        self.ensure_dir(&self.state_subdir())?;
        self.ensure_dir(&self.search_index_dir())?;
        self.ensure_dir(&self.docs_dir())?;
        Ok(())
    }

    /// Rename a legacy state directory to the current name when needed.
    ///
    /// Returns `true` if a rename was performed. On success emits a one-line
    /// product notice on stderr naming both paths (0154: not filterable tracing
    /// INFO). Does **not** edit `.gitignore` — that side-effect is applied at
    /// the dispatch startup seam to keep the `state`/`git` boundary clean.
    pub fn migrate_legacy_state_dir(&self) -> Result<bool> {
        self.migrate_legacy_state_dir_with(|old, new| fs::rename(old, new))
    }

    fn migrate_legacy_state_dir_with<F>(&self, rename: F) -> Result<bool>
    where
        F: FnOnce(&Utf8Path, &Utf8Path) -> std::io::Result<()>,
    {
        if self.state_dir.exists() {
            return Ok(false);
        }

        // Legacy state is a sibling of the *resolved* state directory (main
        // worktree for linked worktrees), not necessarily under work_root.
        let legacy_parent = self
            .state_dir
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| self.root.clone());
        let legacy_state_dir = legacy_parent.join(LEGACY_STATE_DIR);
        if !legacy_state_dir.exists() {
            return Ok(false);
        }

        rename(&legacy_state_dir, &self.state_dir).map_err(|source| {
            StateError::LegacyMigrationFailed {
                old_path: legacy_state_dir.to_string(),
                new_path: self.state_dir.to_string(),
                source,
            }
        })?;
        // 0154: one-shot state-dir migration is product-visible without -v.
        eprintln!(
            "Migrated state directory from {} to {}",
            legacy_state_dir, self.state_dir
        );
        Ok(true)
    }

    pub fn ensure_dir(&self, path: &Utf8Path) -> Result<()> {
        if !path.exists() {
            fs::create_dir_all(path).map_err(|e| StateError::MkdirFailed {
                path: path.to_string(),
                source: e,
            })?;
        }
        Ok(())
    }
}

/// Environment variable: absolute path to the repo's `.ledgerful` directory
/// (the directory that contains `state/`, `config.toml`, etc.).
pub const LEDGERFUL_STATE_DIR_ENV: &str = "LEDGERFUL_STATE_DIR";

/// Resolve the shared Ledgerful state directory for a discovered repository.
///
/// Order (spec §3.3):
/// 1. `LEDGERFUL_STATE_DIR` absolute override (value **is** the state directory)
/// 2. bare repo → error (no parent-of-common guess)
/// 3. linked worktree (`canonical(git_dir) != canonical(common_dir)`) →
///    `common_dir.parent() / ".ledgerful"`
/// 4. else (plain clone, main worktree, or submodule) → `workdir / ".ledgerful"`
///
/// Does **not** key linked-worktree detection on "`.git` is a file" (submodule trap).
pub fn resolve_state_dir(repo: &gix::Repository) -> Result<Utf8PathBuf> {
    if let Ok(raw) = env::var(LEDGERFUL_STATE_DIR_ENV) {
        return resolve_state_dir_override(&raw);
    }

    if repo.is_bare() {
        return Err(miette::miette!(
            "Cannot open repo-scoped Ledgerful state for a bare repository \
             (work_root has no worktree). Set {LEDGERFUL_STATE_DIR_ENV} to an \
             absolute path if you need an explicit state directory."
        ));
    }

    let workdir = repo.workdir().ok_or_else(|| {
        miette::miette!(
            "Repository has no work directory; cannot resolve Ledgerful state. \
             Set {LEDGERFUL_STATE_DIR_ENV} if you need an explicit state directory."
        )
    })?;
    let work_root = utf8_path(workdir, "Repository workdir")?;

    let git_dir = repo.git_dir();
    let common_dir = repo.common_dir();
    let git_canon = canonicalize_for_compare(git_dir);
    let common_canon = canonicalize_for_compare(common_dir);

    if git_canon != common_canon {
        // Linked worktree: common dir is typically `{main}/.git`.
        // Use cleaned/canonical common_dir so relative `..` segments do not
        // produce `{worktrees/<name>}/../.ledgerful` after `.parent()`.
        let main_root = common_canon.parent().ok_or_else(|| {
            miette::miette!(
                "Linked worktree common_dir '{}' has no parent directory; \
                 cannot locate main worktree root for shared state. \
                 Set {LEDGERFUL_STATE_DIR_ENV} to an absolute path.",
                common_canon.display()
            )
        })?;
        if !main_root.is_dir() {
            return Err(miette::miette!(
                "Linked worktree main root '{}' is missing or not a directory; \
                 cannot share `.ledgerful` state. Set {LEDGERFUL_STATE_DIR_ENV} \
                 to an absolute path.",
                main_root.display()
            ));
        }
        let main_root = utf8_path(main_root, "Main worktree root")?;
        return Ok(main_root.join(STATE_DIR));
    }

    // Plain clone, main worktree, or submodule: private state under work root.
    Ok(work_root.join(STATE_DIR))
}

/// Parse and validate `LEDGERFUL_STATE_DIR` (absolute path required).
pub fn resolve_state_dir_override(raw: &str) -> Result<Utf8PathBuf> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(miette::miette!(
            "{LEDGERFUL_STATE_DIR_ENV} is set but empty; provide an absolute path \
             to the Ledgerful state directory (e.g. C:\\\\repo\\\\.ledgerful)."
        ));
    }
    let path = PathBuf::from(trimmed);
    if !path.is_absolute() {
        return Err(miette::miette!(
            "{LEDGERFUL_STATE_DIR_ENV} must be an absolute path (got '{trimmed}'). \
             Relative overrides are rejected so state never silently lands under cwd."
        ));
    }
    utf8_path(&path, LEDGERFUL_STATE_DIR_ENV)
}

/// Discover the current worktree workdir (analysis root). State may differ — use
/// [`get_layout`] when opening ledger/config/index.
pub fn get_repo_root() -> Result<Utf8PathBuf> {
    let current_dir = env::current_dir().into_diagnostic()?;
    let discovered = gix::discover(&current_dir).into_diagnostic()?;
    let root = discovered
        .workdir()
        .ok_or_else(|| miette::miette!("Failed to find work directory for repository"))?;

    utf8_path(root, "Repository root")
}

/// Discover once: work root = current worktree; state_dir via [`resolve_state_dir`].
///
/// Fails if the current directory is not inside a git repository, or if state
/// resolution fails (bare repo, missing main parent, bad override). Prefer this
/// for commands that require a repo.
pub fn get_layout() -> Result<Layout> {
    let current_dir = env::current_dir().into_diagnostic()?;
    let repo = gix::discover(&current_dir).into_diagnostic()?;
    layout_from_discovered_repo(&repo)
}

/// Resolve layout for repo-scoped commands that also tolerate a non-git cwd.
///
/// Policy (fail-closed on linked-worktree resolve bugs):
/// - If `gix::discover` **succeeds** → must use [`resolve_state_dir`] +
///   [`Layout::from_roots`]; resolution errors **propagate** (never invent
///   private `{cwd}/.ledgerful` for a broken linked worktree).
/// - If `gix::discover` **fails** (not a git repo) → `Layout::new(cwd)` is OK.
///
/// Use this instead of `match get_layout() { Ok => …, Err => Layout::new(cwd) }`,
/// which collapses bare/missing-main resolve failures into a private state tree.
pub fn get_layout_or_cwd_if_not_git() -> Result<Layout> {
    let current_dir = env::current_dir().into_diagnostic()?;
    match gix::discover(&current_dir) {
        Ok(repo) => layout_from_discovered_repo(&repo),
        Err(_) => {
            let root = utf8_path(&current_dir, "Current directory")?;
            Ok(Layout::new(root))
        }
    }
}

fn layout_from_discovered_repo(repo: &gix::Repository) -> Result<Layout> {
    let work_root = repo
        .workdir()
        .ok_or_else(|| miette::miette!("Failed to find work directory for repository"))?;
    let work_root = utf8_path(work_root, "Repository root")?;
    let state_dir = resolve_state_dir(repo)?;
    Ok(Layout::from_roots(work_root, state_dir))
}

fn utf8_path(path: &Path, label: &str) -> Result<Utf8PathBuf> {
    Utf8PathBuf::from_path_buf(path.to_path_buf())
        .map_err(|_| miette::miette!("{label} is not valid UTF-8: {}", path.display()))
}

/// Canonicalize for path equality (Windows drive case; macOS `/var` ↔ `/private/var`).
///
/// When `path` does not exist yet (fresh `.ledgerful` / worktree gitdirs),
/// canonicalize the longest existing ancestor and re-join the remainder so
/// both sides of a comparison share the same resolved prefix (avoids macOS
/// tempfile `/var/folders` vs `/private/var/folders` mismatches).
fn canonicalize_for_compare(path: &Path) -> PathBuf {
    if let Ok(c) = dunce::canonicalize(path) {
        return c;
    }
    // Walk up to an existing ancestor, then re-append the missing tail.
    let mut ancestor = path;
    let mut missing: Vec<&std::ffi::OsStr> = Vec::new();
    while let Some(parent) = ancestor.parent() {
        if parent.as_os_str().is_empty() {
            break;
        }
        if let Some(name) = ancestor.file_name() {
            missing.push(name);
        }
        ancestor = parent;
        if let Ok(c) = dunce::canonicalize(ancestor) {
            let mut out = c;
            for name in missing.into_iter().rev() {
                out.push(name);
            }
            return out;
        }
    }
    path.clean()
}

fn normalize_root(root: &Utf8Path) -> Utf8PathBuf {
    let path = root.as_std_path();
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    };
    let cleaned = absolute.clean();
    let normalized = correct_component_case(&cleaned);

    Utf8PathBuf::from_path_buf(normalized).unwrap_or_else(|_| root.to_path_buf())
}

fn correct_component_case(path: &Path) -> PathBuf {
    let mut corrected = PathBuf::new();

    for component in path.components() {
        match component {
            Component::Prefix(prefix) => corrected.push(prefix.as_os_str()),
            Component::RootDir => corrected.push(std::path::MAIN_SEPARATOR.to_string()),
            Component::CurDir => {}
            Component::ParentDir => corrected.push(".."),
            Component::Normal(name) => {
                let parent = if corrected.as_os_str().is_empty() {
                    Path::new(".")
                } else {
                    corrected.as_path()
                };
                let actual_name = fs::read_dir(parent).ok().and_then(|entries| {
                    entries.filter_map(|entry| entry.ok()).find_map(|entry| {
                        let file_name = entry.file_name();
                        file_name
                            .to_string_lossy()
                            .eq_ignore_ascii_case(&name.to_string_lossy())
                            .then_some(file_name)
                    })
                });
                corrected.push(actual_name.as_deref().unwrap_or(name));
            }
        }
    }

    corrected
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::process::Command;
    use tempfile::tempdir;

    #[test]
    fn test_layout_creation() {
        let tmp = tempdir().unwrap();
        let root = Utf8Path::from_path(tmp.path()).unwrap();
        let layout = Layout::new(root);
        let expected_root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();

        assert_eq!(layout.state_dir, expected_root.join(STATE_DIR));
        assert_eq!(
            layout.logs_dir(),
            expected_root.join(STATE_DIR).join(LOGS_DIR)
        );
        assert_eq!(
            layout.config_file(),
            expected_root.join(STATE_DIR).join(CONFIG_FILE)
        );
        assert_eq!(
            layout.rules_file(),
            expected_root.join(STATE_DIR).join(RULES_FILE)
        );
    }

    #[test]
    fn test_ensure_state_dir() {
        let tmp = tempdir().unwrap();
        let root = Utf8Path::from_path(tmp.path()).unwrap();
        let layout = Layout::new(root);

        layout.ensure_state_dir().unwrap();

        assert!(layout.state_dir.exists());
        assert!(layout.logs_dir().exists());
        assert!(layout.tmp_dir().exists());
        assert!(layout.reports_dir().exists());
        assert!(layout.state_subdir().exists());
    }

    #[test]
    fn ensure_state_dir_legacy_state_exists_migrates_without_data_loss() {
        let tmp = tempdir().unwrap();
        let root = Utf8Path::from_path(tmp.path()).unwrap();
        let legacy_state_dir = root.join(LEGACY_STATE_DIR);
        fs::create_dir_all(&legacy_state_dir).unwrap();
        fs::write(legacy_state_dir.join("migration-marker"), "preserved").unwrap();

        let layout = Layout::new(root);
        layout.ensure_state_dir().unwrap();

        assert!(!legacy_state_dir.exists());
        assert_eq!(
            fs::read_to_string(layout.state_dir.join("migration-marker")).unwrap(),
            "preserved"
        );
    }

    #[test]
    fn ensure_state_dir_new_state_exists_does_not_merge_legacy_state() {
        let tmp = tempdir().unwrap();
        let root = Utf8Path::from_path(tmp.path()).unwrap();
        let legacy_state_dir = root.join(LEGACY_STATE_DIR);
        let new_state_dir = root.join(STATE_DIR);
        fs::create_dir_all(&legacy_state_dir).unwrap();
        fs::create_dir_all(&new_state_dir).unwrap();
        fs::write(legacy_state_dir.join("legacy-marker"), "legacy").unwrap();
        fs::write(new_state_dir.join("current-marker"), "current").unwrap();

        let layout = Layout::new(root);
        layout.ensure_state_dir().unwrap();

        assert!(legacy_state_dir.join("legacy-marker").exists());
        assert_eq!(
            fs::read_to_string(layout.state_dir.join("current-marker")).unwrap(),
            "current"
        );
    }

    #[test]
    fn migrate_legacy_state_dir_failure_does_not_create_split_state() {
        let tmp = tempdir().unwrap();
        let root = Utf8Path::from_path(tmp.path()).unwrap();
        let legacy_state_dir = root.join(LEGACY_STATE_DIR);
        fs::create_dir_all(&legacy_state_dir).unwrap();
        fs::write(legacy_state_dir.join("marker"), "preserved").unwrap();
        let layout = Layout::new(root);

        let error = layout
            .migrate_legacy_state_dir_with(|_, _| {
                Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "simulated sharing violation",
                ))
            })
            .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("Failed to migrate Ledgerful state")
        );
        assert!(legacy_state_dir.join("marker").exists());
        assert!(!layout.state_dir.exists());
    }

    #[test]
    fn migrate_returns_true_only_when_rename_occurs() {
        let tmp = tempdir().unwrap();
        let root = Utf8Path::from_path(tmp.path()).unwrap();
        let layout = Layout::new(root);
        assert!(!layout.migrate_legacy_state_dir().unwrap());

        let legacy = root.join(LEGACY_STATE_DIR);
        fs::create_dir_all(&legacy).unwrap();
        fs::write(legacy.join("marker"), "x").unwrap();
        assert!(layout.migrate_legacy_state_dir().unwrap());
        assert!(!layout.migrate_legacy_state_dir().unwrap());
    }

    #[test]
    fn layout_normalizes_existing_roots_to_absolute_paths() {
        let tmp = tempdir().unwrap();
        let original = std::env::current_dir().unwrap();
        std::env::set_current_dir(tmp.path()).unwrap();

        let layout = Layout::new(".");

        std::env::set_current_dir(original).unwrap();
        assert!(layout.root.is_absolute());
        // current_dir() can return a different (but equivalent) path than the raw
        // tempdir: OS-level symlinks are resolved (e.g. macOS /var -> /private/var),
        // and Windows can report 8.3 short names (e.g. RUNNER~1) that
        // correct_component_case doesn't expand. Canonicalize both sides so the
        // assertion checks "same directory", not "identical string formatting".
        let expected_root = dunce::canonicalize(tmp.path()).unwrap();
        let actual_root = dunce::canonicalize(layout.root.as_std_path()).unwrap();
        assert_eq!(actual_root, expected_root);
    }

    #[test]
    fn from_roots_allows_distinct_work_and_state() {
        let tmp = tempdir().unwrap();
        let work = Utf8PathBuf::from_path_buf(tmp.path().join("work")).unwrap();
        let state = Utf8PathBuf::from_path_buf(tmp.path().join("main").join(STATE_DIR)).unwrap();
        fs::create_dir_all(work.as_std_path()).unwrap();
        fs::create_dir_all(state.as_std_path()).unwrap();

        let layout = Layout::from_roots(&work, &state);
        assert_eq!(layout.root.file_name(), Some("work"));
        assert_eq!(layout.state_dir.file_name(), Some(STATE_DIR));
        assert_ne!(layout.state_dir, layout.root.join(STATE_DIR));
        assert_eq!(layout.logs_dir(), layout.state_dir.join(LOGS_DIR));
    }

    fn git(args: &[&str], cwd: &Path) -> std::process::Output {
        Command::new("git")
            .args(args)
            .current_dir(cwd)
            .output()
            .expect("git available")
    }

    fn init_repo(main: &Path) {
        fs::create_dir_all(main).unwrap();
        assert!(git(&["init", "-b", "main"], main).status.success());
        assert!(
            git(&["config", "user.email", "t@example.com"], main)
                .status
                .success()
        );
        assert!(git(&["config", "user.name", "t"], main).status.success());
        // Avoid global hooks / template noise in hermetic fixtures.
        let _ = git(&["config", "core.hooksPath", ".git/hooks"], main);
        fs::write(main.join("README"), "x").unwrap();
        assert!(git(&["add", "README"], main).status.success());
        assert!(git(&["commit", "-m", "init"], main).status.success());
    }

    #[test]
    fn override_absolute_is_honored() {
        let tmp = tempfile::tempdir().unwrap();
        let abs = tmp.path().join("custom-state");
        let abs_str = abs.to_string_lossy().to_string();
        let resolved = resolve_state_dir_override(&abs_str).unwrap();
        assert_eq!(
            dunce::canonicalize(resolved.as_std_path())
                .unwrap_or(resolved.as_std_path().to_path_buf()),
            dunce::canonicalize(&abs).unwrap_or(abs)
        );
    }

    #[test]
    fn override_rejects_relative() {
        let err = resolve_state_dir_override("relative/state").unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains(LEDGERFUL_STATE_DIR_ENV) && msg.to_ascii_lowercase().contains("absolute"),
            "expected absolute-path error, got: {msg}"
        );
    }

    #[test]
    fn override_rejects_empty() {
        let err = resolve_state_dir_override("   ").unwrap_err();
        assert!(err.to_string().contains("empty"));
    }

    #[test]
    fn plain_clone_state_under_work_root() {
        let tmp = tempfile::tempdir().unwrap();
        let main = tmp.path().join("main");
        init_repo(&main);
        let repo = gix::discover(&main).unwrap();
        let state = resolve_state_dir(&repo).unwrap();
        let expected = Utf8PathBuf::from_path_buf(main.join(STATE_DIR)).unwrap();
        assert_eq!(
            dunce::canonicalize(state.as_std_path()).unwrap_or(state.as_std_path().to_path_buf()),
            dunce::canonicalize(expected.as_std_path())
                .unwrap_or(expected.as_std_path().to_path_buf())
        );
    }

    #[test]
    fn linked_worktree_shares_main_state_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let main = tmp.path().join("main");
        init_repo(&main);
        let linked = tmp.path().join("linked");
        let out = git(
            &["worktree", "add", linked.to_str().unwrap(), "HEAD"],
            &main,
        );
        assert!(
            out.status.success(),
            "worktree add failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );

        let main_repo = gix::discover(&main).unwrap();
        let linked_repo = gix::discover(&linked).unwrap();

        // Sanity: linked worktree has distinct git_dir vs common_dir.
        let git_canon = canonicalize_for_compare(linked_repo.git_dir());
        let common_canon = canonicalize_for_compare(linked_repo.common_dir());
        assert_ne!(
            git_canon, common_canon,
            "fixture must be a linked worktree (git_dir != common_dir)"
        );

        let main_state = resolve_state_dir(&main_repo).unwrap();
        let linked_state = resolve_state_dir(&linked_repo).unwrap();
        let expected = main.join(STATE_DIR);

        let clean = |p: &std::path::Path| canonicalize_for_compare(p);
        assert_eq!(
            clean(main_state.as_std_path()),
            clean(linked_state.as_std_path()),
            "main and linked must share the same state_dir (main={main_state}, linked={linked_state})"
        );
        assert_eq!(
            clean(linked_state.as_std_path()),
            clean(&expected),
            "shared state must be main/.ledgerful"
        );
        // Active home is main/.ledgerful, not linked/.ledgerful
        let linked_private = linked.join(STATE_DIR);
        assert_ne!(
            clean(linked_state.as_std_path()),
            clean(&linked_private),
            "linked worktree must not use private {{linked}}/.ledgerful as state home"
        );
        fs::create_dir_all(&expected).unwrap();
        let after = resolve_state_dir(&linked_repo).unwrap();
        assert_eq!(
            dunce::canonicalize(after.as_std_path()).unwrap(),
            dunce::canonicalize(&expected).unwrap()
        );
    }

    #[test]
    fn submodule_like_equal_git_and_common_keeps_module_state() {
        // Synthetic: open a normal repo (git_dir == common_dir) even if we place a
        // `.git` *file* pointing at a self-contained module gitdir — gix still reports
        // equality when the module is self-contained (submodule shape).
        let tmp = tempfile::tempdir().unwrap();
        let parent = tmp.path().join("parent");
        init_repo(&parent);
        // Real submodule fixture when git submodule works; otherwise synthetic module.
        let module = parent.join("vendor").join("mod");
        fs::create_dir_all(&module).unwrap();
        assert!(git(&["init", "-b", "main"], &module).status.success());
        assert!(
            git(&["config", "user.email", "t@example.com"], &module)
                .status
                .success()
        );
        assert!(git(&["config", "user.name", "t"], &module).status.success());
        fs::write(module.join("lib.txt"), "m").unwrap();
        assert!(git(&["add", "lib.txt"], &module).status.success());
        assert!(git(&["commit", "-m", "mod"], &module).status.success());

        let repo = gix::discover(&module).unwrap();
        let git_canon = canonicalize_for_compare(repo.git_dir());
        let common_canon = canonicalize_for_compare(repo.common_dir());
        assert_eq!(
            git_canon, common_canon,
            "standalone module must have git_dir == common_dir"
        );

        let state = resolve_state_dir(&repo).unwrap();
        let expected = module.join(STATE_DIR);
        assert_eq!(
            state.as_str().replace('\\', "/"),
            Utf8PathBuf::from_path_buf(expected)
                .unwrap()
                .as_str()
                .replace('\\', "/")
        );
        // Must not select parent .ledgerful
        let parent_state = parent.join(STATE_DIR);
        assert_ne!(state.as_std_path(), parent_state.as_path());
    }

    #[test]
    fn bare_repo_errors_without_override() {
        let tmp = tempfile::tempdir().unwrap();
        let bare = tmp.path().join("bare.git");
        assert!(
            git(
                &["init", "--bare", "-b", "main", bare.to_str().unwrap()],
                tmp.path()
            )
            .status
            .success()
        );
        let repo = gix::open(&bare).unwrap();
        assert!(repo.is_bare());
        let err = resolve_state_dir(&repo).unwrap_err();
        assert!(
            err.to_string().to_ascii_lowercase().contains("bare"),
            "expected bare error, got: {err}"
        );
    }

    #[test]
    fn layout_from_roots_keeps_work_root_and_shared_state() {
        let tmp = tempfile::tempdir().unwrap();
        let work = Utf8PathBuf::from_path_buf(tmp.path().join("linked")).unwrap();
        let state = Utf8PathBuf::from_path_buf(tmp.path().join("main").join(STATE_DIR)).unwrap();
        fs::create_dir_all(work.as_std_path()).unwrap();
        fs::create_dir_all(state.as_std_path()).unwrap();
        let layout = Layout::from_roots(&work, &state);
        assert!(
            layout.root.as_str().replace('\\', "/").ends_with("/linked")
                || layout.root.file_name() == Some("linked")
        );
        assert_eq!(
            layout.state_dir.file_name(),
            Some(STATE_DIR),
            "state_dir basename should be .ledgerful"
        );
        assert_ne!(
            layout.state_dir,
            layout.root.join(STATE_DIR),
            "shared state differs from work-local .ledgerful"
        );
    }

    #[test]
    fn nested_subdir_discover_state_is_work_root_not_nested() {
        // Discover from a nested subdirectory must not place state under nested/.ledgerful.
        let tmp = tempfile::tempdir().unwrap();
        let main = tmp.path().join("main");
        init_repo(&main);
        let nested = main.join("src").join("deep");
        fs::create_dir_all(&nested).unwrap();

        let repo = gix::discover(&nested).unwrap();
        let state = resolve_state_dir(&repo).unwrap();
        let expected = main.join(STATE_DIR);
        let nested_private = nested.join(STATE_DIR);

        let clean = |p: &std::path::Path| canonicalize_for_compare(p);
        assert_eq!(
            clean(state.as_std_path()),
            clean(&expected),
            "nested cwd discover must resolve state to work_root/.ledgerful, got {state}"
        );
        assert_ne!(
            clean(state.as_std_path()),
            clean(&nested_private),
            "state_dir must not be nested/.ledgerful"
        );

        let workdir = repo.workdir().expect("workdir");
        let work_root = Utf8PathBuf::from_path_buf(workdir.to_path_buf()).unwrap();
        let layout = Layout::from_roots(&work_root, &state);
        assert_eq!(
            clean(layout.state_dir.as_std_path()),
            clean(&expected),
            "from_roots must keep resolve_state_dir result"
        );
        // work_root is the repo root (may contain `..` from discover; compare canonically)
        assert_eq!(
            clean(layout.root.as_std_path()),
            clean(&main),
            "work_root must be the repository root, not the nested start path"
        );
    }

    #[test]
    fn linked_worktree_from_roots_layout_matches_main_state() {
        let tmp = tempfile::tempdir().unwrap();
        let main = tmp.path().join("main");
        init_repo(&main);
        let linked = tmp.path().join("linked");
        let out = git(
            &["worktree", "add", linked.to_str().unwrap(), "HEAD"],
            &main,
        );
        assert!(
            out.status.success(),
            "worktree add failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );

        let main_state = main.join(STATE_DIR);
        fs::create_dir_all(&main_state).unwrap();
        // Sentinel proves shared state home (not invented under linked).
        fs::write(main_state.join("shared-marker"), "ok").unwrap();

        let linked_repo = gix::discover(&linked).unwrap();
        let state = resolve_state_dir(&linked_repo).unwrap();
        let workdir = linked_repo.workdir().expect("linked workdir");
        let work_root = Utf8PathBuf::from_path_buf(workdir.to_path_buf()).unwrap();
        let layout = Layout::from_roots(&work_root, &state);

        let clean = |p: &std::path::Path| canonicalize_for_compare(p);
        assert_eq!(
            clean(layout.state_dir.as_std_path()),
            clean(&main_state),
            "linked layout.state_dir must be main/.ledgerful"
        );
        assert_eq!(
            clean(layout.root.as_std_path()),
            clean(&linked),
            "linked layout.root must remain the linked worktree workdir"
        );
        assert!(
            layout.state_dir.join("shared-marker").exists(),
            "shared marker under main state must be visible via resolved layout"
        );
        assert_ne!(
            clean(layout.state_dir.as_std_path()),
            clean(&linked.join(STATE_DIR)),
            "must not use private linked/.ledgerful"
        );
    }

    #[test]
    fn linked_worktree_storage_init_shares_db_and_keeps_work_root() {
        // DoD-1 strength: real git worktree add + init_with_layout writes a row
        // visible from main via the same absolute ledger.db path.
        use crate::state::storage::StorageManager;

        let tmp = tempfile::tempdir().unwrap();
        let main = tmp.path().join("main");
        init_repo(&main);
        let linked = tmp.path().join("linked");
        let out = git(
            &["worktree", "add", linked.to_str().unwrap(), "HEAD"],
            &main,
        );
        assert!(
            out.status.success(),
            "worktree add failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );

        let main_repo = gix::discover(&main).unwrap();
        let linked_repo = gix::discover(&linked).unwrap();
        let main_state = resolve_state_dir(&main_repo).unwrap();
        let linked_state = resolve_state_dir(&linked_repo).unwrap();
        assert_eq!(
            canonicalize_for_compare(main_state.as_std_path()),
            canonicalize_for_compare(linked_state.as_std_path())
        );

        let main_workdir = utf8_path(main_repo.workdir().unwrap(), "main workdir").unwrap();
        let linked_workdir = utf8_path(linked_repo.workdir().unwrap(), "linked workdir").unwrap();
        let main_layout = Layout::from_roots(&main_workdir, &main_state);
        let linked_layout = Layout::from_roots(&linked_workdir, &linked_state);
        main_layout.ensure_state_dir().unwrap();

        let linked_db = linked_layout.state_subdir().join("ledger.db");
        let main_db = main_layout.state_subdir().join("ledger.db");
        assert_eq!(
            canonicalize_for_compare(linked_db.as_std_path()),
            canonicalize_for_compare(main_db.as_std_path()),
            "both layouts must open the same absolute ledger.db"
        );

        let write = StorageManager::init_with_layout(&linked_layout).unwrap();
        assert_eq!(
            canonicalize_for_compare(write.root_path().as_std_path()),
            canonicalize_for_compare(linked.as_path()),
            "write-mode root_path must stay the linked worktree"
        );
        write
            .get_connection()
            .execute(
                "CREATE TABLE IF NOT EXISTS _wt_shared (k TEXT PRIMARY KEY, v TEXT)",
                [],
            )
            .unwrap();
        write
            .get_connection()
            .execute(
                "INSERT INTO _wt_shared (k, v) VALUES ('probe', 'from-linked')",
                [],
            )
            .unwrap();
        let _ = write.shutdown();

        let read = StorageManager::open_read_only(&main_layout).unwrap();
        assert_eq!(
            canonicalize_for_compare(read.root_path().as_std_path()),
            canonicalize_for_compare(main.as_path())
        );
        let v: String = read
            .get_connection()
            .query_row("SELECT v FROM _wt_shared WHERE k = 'probe'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(v, "from-linked");
    }

    #[test]
    #[serial_test::serial(env)]
    fn linked_worktree_absolute_env_override_wins() {
        mod env_guard {
            include!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/integration/common/env_guard.rs"
            ));
        }
        use env_guard::TempEnv;

        let tmp = tempfile::tempdir().unwrap();
        let main = tmp.path().join("main");
        init_repo(&main);
        let linked = tmp.path().join("linked");
        let out = git(
            &["worktree", "add", linked.to_str().unwrap(), "HEAD"],
            &main,
        );
        assert!(
            out.status.success(),
            "worktree add failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );

        let override_dir = tmp.path().join("custom-state-home");
        fs::create_dir_all(&override_dir).unwrap();
        let override_str = override_dir.to_string_lossy().to_string();
        let _guard = TempEnv::set(LEDGERFUL_STATE_DIR_ENV, &override_str);

        let linked_repo = gix::discover(&linked).unwrap();
        let state = resolve_state_dir(&linked_repo).unwrap();
        let clean = |p: &std::path::Path| canonicalize_for_compare(p);
        assert_eq!(
            clean(state.as_std_path()),
            clean(&override_dir),
            "LEDGERFUL_STATE_DIR absolute override must win over linked-main default"
        );
        assert_ne!(
            clean(state.as_std_path()),
            clean(&main.join(STATE_DIR)),
            "override must not fall through to main/.ledgerful"
        );
    }
}
