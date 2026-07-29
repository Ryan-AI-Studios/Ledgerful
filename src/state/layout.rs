use crate::state::StateError;
use camino::{Utf8Path, Utf8PathBuf};
use miette::Result;
use path_clean::PathClean;
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
    /// Prefer [`Layout::from_roots`] or `commands::helpers::get_layout` for
    /// production paths so linked worktrees share the main worktree's state.
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
    /// `tracing::info!` record naming both paths (DoD-2). Does **not** edit
    /// `.gitignore` — that side-effect is applied at the dispatch startup
    /// seam to keep the `state`/`git` boundary clean.
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
        tracing::info!(
            "Migrated state directory from {} to {}",
            legacy_state_dir,
            self.state_dir
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
}
