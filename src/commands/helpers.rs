use crate::config::load::load_config;
use crate::config::model::Config;
use crate::state::layout::{Layout, STATE_DIR};
use camino::Utf8PathBuf;
use miette::{IntoDiagnostic, Result};
use std::env;
use std::path::{Path, PathBuf};

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
pub fn get_layout() -> Result<Layout> {
    let current_dir = env::current_dir().into_diagnostic()?;
    let repo = gix::discover(&current_dir).into_diagnostic()?;
    let work_root = repo
        .workdir()
        .ok_or_else(|| miette::miette!("Failed to find work directory for repository"))?;
    let work_root = utf8_path(work_root, "Repository root")?;
    let state_dir = resolve_state_dir(&repo)?;
    Ok(Layout::from_roots(work_root, state_dir))
}

pub fn load_ledger_config(layout: &Layout) -> Result<Config> {
    load_config(layout)
}

fn utf8_path(path: &Path, label: &str) -> Result<Utf8PathBuf> {
    Utf8PathBuf::from_path_buf(path.to_path_buf())
        .map_err(|_| miette::miette!("{label} is not valid UTF-8: {}", path.display()))
}

/// Canonicalize for path equality on Windows (symlinks / short names / drive case).
/// Falls back to `path_clean` so relative `..` segments collapse when the path
/// does not yet exist on disk (common for freshly created worktree gitdirs).
fn canonicalize_for_compare(path: &Path) -> PathBuf {
    if let Ok(c) = dunce::canonicalize(path) {
        return c;
    }
    use path_clean::PathClean;
    path.clean()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::layout::STATE_DIR;
    use std::fs;
    use std::process::Command;

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
}
