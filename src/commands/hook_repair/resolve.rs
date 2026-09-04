use camino::{Utf8Path, Utf8PathBuf};
use path_clean::PathClean;
use std::fs;
use std::path::{Path, PathBuf};

/// How hook discovery resolved (or failed to resolve) the hooks directory.
#[derive(Debug, Clone)]
pub enum HooksDirResolution {
    /// Hooks directory found and inside the repository.
    Found { hooks_dir: Utf8PathBuf },
    /// Resolved path is outside the repository — refuse to rewrite.
    OutsideRepo { hooks_dir: Utf8PathBuf },
    /// Could not determine a hooks directory.
    CannotLook { reason: String },
}

/// Resolve the hooks directory for `repo_root`.
///
/// Order:
/// 1. `core.hooksPath` from local git config (relative values resolve against
///    the work tree / common dir per git rules — we resolve against repo root)
/// 2. Linked worktree: `.git` file → `gitdir:` → `commondir` → `hooks`
/// 3. Default: `repo_root/.git/hooks`
///
/// Pure FS first; if shelling git is needed for config, uses
/// [`crate::git::commit::git_command`] (DoD-9c).
pub fn resolve_hooks_dir(repo_root: &Utf8Path) -> HooksDirResolution {
    // Prefer core.hooksPath when set.
    if let Some(hooks_path) = read_core_hooks_path(repo_root) {
        let resolved = resolve_hooks_path_value(repo_root, &hooks_path);
        return classify_hooks_dir(repo_root, resolved);
    }

    let git_path = repo_root.join(".git");
    if git_path.is_file() {
        // Linked worktree: resolve via commondir, not <gitdir>/hooks.
        match resolve_worktree_hooks_dir(repo_root, &git_path) {
            Ok(hooks) => return classify_hooks_dir(repo_root, hooks),
            Err(reason) => {
                return HooksDirResolution::CannotLook { reason };
            }
        }
    }

    if git_path.is_dir() {
        let hooks = git_path.join("hooks");
        return classify_hooks_dir(repo_root, hooks);
    }

    HooksDirResolution::CannotLook {
        reason: "no .git directory or file found".to_string(),
    }
}

fn classify_hooks_dir(repo_root: &Utf8Path, hooks_dir: Utf8PathBuf) -> HooksDirResolution {
    // Clean `..` components from worktree commondir resolution
    // (e.g. `<gitdir>/../..` → main `.git`).
    let hooks_dir = {
        let cleaned = hooks_dir.as_std_path().clean();
        Utf8PathBuf::from_path_buf(cleaned).unwrap_or(hooks_dir)
    };
    match path_is_inside_repo(repo_root, hooks_dir.as_std_path()) {
        Ok(true) => HooksDirResolution::Found { hooks_dir },
        Ok(false) => {
            // Linked worktrees: hooks live under the common git dir, which is
            // outside the worktree work-tree path but still part of the repo.
            // Allow when the hooks dir is under the resolved common directory.
            if hooks_under_git_common_dir(repo_root, hooks_dir.as_std_path()) {
                HooksDirResolution::Found { hooks_dir }
            } else {
                HooksDirResolution::OutsideRepo { hooks_dir }
            }
        }
        Err(reason) => HooksDirResolution::CannotLook { reason },
    }
}

/// True when `hooks` is under this repo's git common directory (covers linked
/// worktrees whose work-tree path does not contain `.git/hooks`).
fn hooks_under_git_common_dir(repo_root: &Utf8Path, hooks: &Path) -> bool {
    let git_path = repo_root.join(".git");
    let common = if git_path.is_file() {
        let Ok(contents) = fs::read_to_string(git_path.as_std_path()) else {
            return false;
        };
        let Some(gitdir_line) = contents
            .lines()
            .find_map(|l| l.trim().strip_prefix("gitdir:"))
            .map(str::trim)
        else {
            return false;
        };
        let gitdir = {
            let p = Path::new(gitdir_line);
            if p.is_absolute() {
                PathBuf::from(p)
            } else {
                repo_root.as_std_path().join(p)
            }
        };
        let commondir_file = gitdir.join("commondir");
        if commondir_file.is_file() {
            let Ok(rel) = fs::read_to_string(&commondir_file) else {
                return false;
            };
            let rel = rel.trim();
            let p = Path::new(rel);
            if p.is_absolute() {
                PathBuf::from(p)
            } else {
                gitdir.join(p).clean()
            }
        } else {
            gitdir
        }
    } else if git_path.is_dir() {
        git_path.into_std_path_buf()
    } else {
        return false;
    };

    let Ok(common_canon) = dunce::canonicalize(&common) else {
        // Common may not need full canonicalize if hooks parent exists.
        let common_clean = common.clean();
        let hooks_clean = hooks.clean();
        return hooks_clean.starts_with(&common_clean)
            || path_is_inside_repo_paths(&common_clean, hooks).unwrap_or(false);
    };
    let common_canon = strip_verbatim_prefix(&common_canon);
    path_is_inside_repo_paths(&common_canon, hooks).unwrap_or(false)
}

fn path_is_inside_repo_paths(root: &Path, candidate: &Path) -> std::result::Result<bool, String> {
    let root_canon = if root.exists() {
        strip_verbatim_prefix(
            &dunce::canonicalize(root).map_err(|e| format!("canonicalize root: {e}"))?,
        )
    } else {
        strip_verbatim_prefix(&root.clean())
    };
    let cand_canon = if candidate.exists() {
        strip_verbatim_prefix(
            &dunce::canonicalize(candidate).map_err(|e| format!("canonicalize candidate: {e}"))?,
        )
    } else if let Some(parent) = candidate.parent() {
        if !parent.as_os_str().is_empty() && parent.exists() {
            let parent_canon =
                dunce::canonicalize(parent).map_err(|e| format!("canonicalize parent: {e}"))?;
            strip_verbatim_prefix(&parent_canon.join(candidate.file_name().unwrap_or_default()))
        } else {
            strip_verbatim_prefix(&candidate.clean())
        }
    } else {
        strip_verbatim_prefix(&candidate.clean())
    };

    #[cfg(windows)]
    {
        let root_s = root_canon.to_string_lossy().to_lowercase();
        let cand_s = cand_canon.to_string_lossy().to_lowercase();
        let root_trim = root_s.trim_end_matches(['/', '\\']);
        Ok(cand_s == root_s
            || cand_s.starts_with(&format!("{root_trim}{}", std::path::MAIN_SEPARATOR))
            || cand_s.starts_with(&format!("{root_trim}/")))
    }
    #[cfg(not(windows))]
    {
        Ok(cand_canon == root_canon || cand_canon.starts_with(&root_canon))
    }
}

/// Read `core.hooksPath` for the repo via pure FS (`.git/config` or worktree
/// common config). Prefer FS over shelling git: `git_command()` always passes
/// `-c core.hooksPath=` which would mask the stored value if used for
/// `config --get` (DoD-9c: any git shell-out must still use `git_command()`,
/// so we avoid shelling here entirely).
fn read_core_hooks_path(repo_root: &Utf8Path) -> Option<String> {
    let git_path = repo_root.join(".git");
    let config_path = if git_path.is_file() {
        // Linked worktree: config lives in the common dir.
        let contents = fs::read_to_string(git_path.as_std_path()).ok()?;
        let gitdir_line = contents
            .lines()
            .find_map(|l| l.trim().strip_prefix("gitdir:"))
            .map(str::trim)?;
        let gitdir = {
            let p = Path::new(gitdir_line);
            if p.is_absolute() {
                PathBuf::from(p)
            } else {
                repo_root.as_std_path().join(p)
            }
        };
        let common = {
            let commondir_file = gitdir.join("commondir");
            if commondir_file.is_file() {
                let rel = fs::read_to_string(&commondir_file).ok()?;
                let rel = rel.trim();
                let p = Path::new(rel);
                if p.is_absolute() {
                    PathBuf::from(p)
                } else {
                    gitdir.join(p)
                }
            } else {
                gitdir
            }
        };
        common.join("config")
    } else if git_path.is_dir() {
        git_path.join("config").into_std_path_buf()
    } else {
        return None;
    };

    let content = fs::read_to_string(&config_path).ok()?;
    parse_git_config_value(&content, "core", "hooksPath")
}

/// Minimal git-config parser for a single `section.key` string value.
/// Handles `[core]` / `hooksPath = value` and `[core] hooksPath=value`.
pub(super) fn parse_git_config_value(content: &str, section: &str, key: &str) -> Option<String> {
    let section_header = format!("[{section}]");
    let mut in_section = false;
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_section = trimmed.eq_ignore_ascii_case(&section_header)
                || trimmed
                    .to_ascii_lowercase()
                    .starts_with(&format!("[{section} "));
            continue;
        }
        if !in_section {
            continue;
        }
        // key = value  or  key=value
        let Some((k, v)) = trimmed.split_once('=') else {
            continue;
        };
        if k.trim().eq_ignore_ascii_case(key) {
            let value = v.trim().trim_matches('"').trim_matches('\'').to_string();
            if !value.is_empty() {
                return Some(value);
            }
        }
    }
    None
}

fn resolve_hooks_path_value(repo_root: &Utf8Path, value: &str) -> Utf8PathBuf {
    let path = Path::new(value);
    if path.is_absolute() {
        Utf8PathBuf::from_path_buf(path.to_path_buf()).unwrap_or_else(|_| repo_root.join(value))
    } else {
        // Git resolves relative hooksPath against the common dir for bare
        // repos and against the work tree for non-bare. For repair we resolve
        // against the repo root (work tree), which matches the measured
        // shapes (`.husky/_`, `./apps/api/.husky/_`).
        repo_root.join(value)
    }
}

/// Resolve hooks for a linked worktree via `<gitdir>/commondir` → hooks.
fn resolve_worktree_hooks_dir(
    repo_root: &Utf8Path,
    git_file: &Utf8Path,
) -> std::result::Result<Utf8PathBuf, String> {
    let contents = fs::read_to_string(git_file.as_std_path())
        .map_err(|e| format!("cannot read .git file: {e}"))?;
    let gitdir_line = contents
        .lines()
        .find_map(|l| l.trim().strip_prefix("gitdir:"))
        .map(str::trim)
        .ok_or_else(|| "`.git` file has no gitdir: line".to_string())?;

    let gitdir = {
        let p = Path::new(gitdir_line);
        if p.is_absolute() {
            PathBuf::from(p)
        } else {
            repo_root.as_std_path().join(p)
        }
    };

    let commondir_file = gitdir.join("commondir");
    let common = if commondir_file.is_file() {
        let rel = fs::read_to_string(&commondir_file)
            .map_err(|e| format!("cannot read commondir: {e}"))?;
        let rel = rel.trim();
        let p = Path::new(rel);
        if p.is_absolute() {
            PathBuf::from(p)
        } else {
            gitdir.join(p)
        }
    } else {
        // Fall back to gitdir itself (non-worktree layout).
        gitdir
    };

    let hooks = common.join("hooks").clean();
    Utf8PathBuf::from_path_buf(hooks).map_err(|_| "hooks path is not valid UTF-8".to_string())
}

/// Case-insensitive containment check on Windows (reuses dunce canonicalize
/// + strip_verbatim pattern from dispatch path containment).
fn path_is_inside_repo(
    repo_root: &Utf8Path,
    candidate: &Path,
) -> std::result::Result<bool, String> {
    path_is_inside_repo_paths(repo_root.as_std_path(), candidate)
}

fn strip_verbatim_prefix(path: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        use std::path::Component;
        let mut components = path.components();
        if let Some(Component::Prefix(prefix)) = components.next()
            && let Some(disk) = prefix
                .as_os_str()
                .to_str()
                .and_then(|s| s.strip_prefix(r"\\?\"))
        {
            let rest = components.as_path();
            return Path::new(disk).join(rest);
        }
    }
    path.to_path_buf()
}
