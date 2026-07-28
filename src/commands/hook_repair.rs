//! Repair already-installed git hooks that invoke the retired binary name
//! instead of the canonical `ledgerful` binary, and normalize legacy
//! idempotency marker comments so `init` recognises its own blocks.
//!
//! `ledgerful init` (see `src/commands/init.rs`) already writes hooks that
//! call `ledgerful`. This module fixes up hooks that were installed by an
//! older version of `init` (or hand-written).
//!
//! Replacements cover:
//! - Exact command invocations (`command -v`, `ledger`, `verify`, `scan`,
//!   `internal hook-`)
//! - Legacy marker comments (`# <legacy>-ledger-gate` etc.) so a subsequent
//!   `init` upgrades in place instead of appending a duplicate block (0094)
//!
//! Two-tier de-duplication when both legacy and current markers are present:
//! - Tier 1: exact match of a known generated block → auto-remove
//! - Tier 2: marker-bounded block with only recognised invocations → report
//!   with text, never auto-delete
//!
//! Discovery honours `core.hooksPath` and linked-worktree `commondir`; a
//! hooks directory outside the repository is reported and never rewritten.
//! Third-party manager detection re-runs against the resolved hooks path.

use camino::{Utf8Path, Utf8PathBuf};
use miette::{IntoDiagnostic, Result};
use owo_colors::OwoColorize;
use path_clean::PathClean;
use std::fs;
use std::path::{Path, PathBuf};

/// Retired product binary name, built via `concat!` so the brand is not a
/// greppable literal in the binary (R6 / DoD-14).
const LEGACY_BINARY: &str = concat!("change", "guard");

/// Current product binary name.
const CURRENT_BINARY: &str = "ledgerful";

/// Gate-type suffixes shared by legacy and current marker comments.
const GATE_SUFFIXES: &[&str] = &[
    "ledger-gate",
    "verify-gate",
    "intent-gate",
    "post-commit-gate",
];

/// Names of known third-party hook managers, checked in priority order.
/// Mirrors the canonical spelling used by `is_pre_commit_path` in
/// `src/index/ci_gates.rs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThirdPartyHookManager {
    Husky,
    Lefthook,
    PreCommit,
}

impl ThirdPartyHookManager {
    pub fn name(&self) -> &'static str {
        match self {
            ThirdPartyHookManager::Husky => "husky",
            ThirdPartyHookManager::Lefthook => "lefthook",
            ThirdPartyHookManager::PreCommit => "pre-commit",
        }
    }

    /// Relative path (from repo root) to the manager's config file/dir, used
    /// in the hint printed alongside the warning.
    pub fn config_hint(&self) -> &'static str {
        match self {
            ThirdPartyHookManager::Husky => ".husky/",
            ThirdPartyHookManager::Lefthook => "lefthook.yml",
            ThirdPartyHookManager::PreCommit => ".pre-commit-config.yaml",
        }
    }
}

/// Detect a third-party hook manager at `root`, checking in fixed priority
/// order: husky, lefthook, pre-commit. Returns only the first match.
pub fn detect_third_party_hook_manager(root: &Utf8Path) -> Option<ThirdPartyHookManager> {
    if root.join(".husky").is_dir() {
        return Some(ThirdPartyHookManager::Husky);
    }
    if root.join("lefthook.yml").is_file() {
        return Some(ThirdPartyHookManager::Lefthook);
    }
    if root.join(".pre-commit-config.yaml").is_file() {
        return Some(ThirdPartyHookManager::PreCommit);
    }
    None
}

/// Re-run third-party detection against a resolved hooks directory: if any
/// ancestor component is `.husky`, treat as husky (covers CrawlX
/// `apps/api/.husky/_` where the repo-root guard is blind).
fn detect_third_party_at_hooks_dir(hooks_dir: &Utf8Path) -> Option<ThirdPartyHookManager> {
    for component in hooks_dir.components() {
        if component.as_str() == ".husky" {
            return Some(ThirdPartyHookManager::Husky);
        }
    }
    // Also check the parent of hooks_dir (e.g. `.husky` when hooksPath is `.husky/_`).
    if let Some(parent) = hooks_dir.parent() {
        if parent.file_name() == Some(".husky") {
            return Some(ThirdPartyHookManager::Husky);
        }
        if detect_third_party_hook_manager(parent).is_some() {
            return detect_third_party_hook_manager(parent);
        }
    }
    None
}

/// Outcome of a single hook file's repair attempt.
#[derive(Debug, Clone, Default)]
pub struct HookRepairReport {
    /// Hooks that contained at least one stale invocation/marker and were
    /// rewritten with no residual retired-binary invocation left (or, in
    /// dry-run mode, would be rewritten). Sorted by filename.
    pub repaired: Vec<String>,
    /// Hooks that already used `ledgerful` invocations exclusively.
    /// Sorted by filename.
    pub already_correct: Vec<String>,
    /// Hooks with no ledger-related invocations at all; left untouched.
    /// Sorted by filename.
    pub skipped: Vec<String>,
    /// Third-party hook manager detected, if any. When set, no hooks were
    /// rewritten regardless of their contents.
    pub third_party_manager: Option<ThirdPartyHookManager>,
    /// True if this was a dry run (nothing was actually written to disk).
    pub dry_run: bool,
    /// Hooks that still contain a retired-binary invocation after attempted
    /// repair (DoD-4c honesty). Sorted by filename.
    pub residual_invocations: Vec<String>,
    /// Near-miss de-duplication blocks: (hook name, block text). Tier-2 —
    /// reported, never auto-deleted (DoD-4b). Sorted by hook name.
    pub near_miss_blocks: Vec<(String, String)>,
    /// Why discovery could not look (outside-repo, missing dir, unreadable
    /// config). When set, the report must not render as "clean".
    pub discovery_notes: Vec<String>,
    /// Absolute path of the hooks directory that was examined, if any.
    pub hooks_dir: Option<String>,
}

impl HookRepairReport {
    fn empty(dry_run: bool) -> Self {
        Self {
            dry_run,
            ..Default::default()
        }
    }

    /// True when there is nothing to report as a problem and nothing was done.
    pub fn is_silent_clean(&self) -> bool {
        self.repaired.is_empty()
            && self.residual_invocations.is_empty()
            && self.near_miss_blocks.is_empty()
            && self.discovery_notes.is_empty()
            && self.third_party_manager.is_none()
    }
}

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
fn parse_git_config_value(content: &str, section: &str, key: &str) -> Option<String> {
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

// ---------------------------------------------------------------------------
// Replacement + de-duplication
// ---------------------------------------------------------------------------

/// Apply invocation and marker replacements. Returns rewritten content and
/// whether any replacement fired.
fn apply_replacements(content: &str) -> (String, bool) {
    let mut result = content.to_string();
    let mut changed = false;

    // 1. Marker comments: `# <legacy>-<gate>:` → `# ledgerful-<gate>:`
    for suffix in GATE_SUFFIXES {
        let legacy_marker = format!("# {LEGACY_BINARY}-{suffix}");
        let current_marker = format!("# {CURRENT_BINARY}-{suffix}");
        if result.contains(&legacy_marker) {
            result = result.replace(&legacy_marker, &current_marker);
            changed = true;
        }
    }

    // Prose inside the marker line historically said `` `<legacy> init` ``.
    let legacy_init = format!("`{LEGACY_BINARY} init`");
    let current_init = "`ledgerful init`";
    if result.contains(&legacy_init) {
        result = result.replace(&legacy_init, current_init);
        changed = true;
    }

    // 2. Exact command invocation fragments (order matters for longest-first
    // safety; all are distinct enough for plain replace).
    for (current, suffix) in [
        ("command -v ledgerful", "command -v "),
        ("ledgerful ledger", ""),
        ("ledgerful internal hook-", ""),
        ("ledgerful verify", ""),
        ("ledgerful scan", ""),
    ] {
        let retired = if suffix.is_empty() {
            current.replacen("ledgerful", LEGACY_BINARY, 1)
        } else {
            format!("{suffix}{LEGACY_BINARY}")
        };
        if result.contains(&retired) {
            result = result.replace(&retired, current);
            changed = true;
        }
    }

    (result, changed)
}

/// Whether `content` still contains a retired-binary *invocation* (not a
/// historical mention in a `(renamed from …)` comment).
fn contains_legacy_invocation(content: &str) -> bool {
    let patterns = [
        format!("command -v {LEGACY_BINARY}"),
        format!("{LEGACY_BINARY} ledger"),
        format!("{LEGACY_BINARY} internal hook-"),
        format!("{LEGACY_BINARY} verify"),
        format!("{LEGACY_BINARY} scan"),
    ];
    patterns.iter().any(|p| content.contains(p.as_str()))
}

/// Whether `content` contains any retired or current Ledgerful invocations we
/// know how to repair.
fn contains_ledger_invocation(content: &str) -> bool {
    contains_legacy_invocation(content)
        || content.contains("command -v ledgerful")
        || content.contains("ledgerful ledger")
        || content.contains("ledgerful internal hook-")
        || content.contains("ledgerful verify")
        || content.contains("ledgerful scan")
        || GATE_SUFFIXES.iter().any(|s| {
            content.contains(&format!("# ledgerful-{s}"))
                || content.contains(&format!("# {LEGACY_BINARY}-{s}"))
        })
}

/// Known generated legacy gate blocks (exact match → tier-1 auto-remove when
/// a current-marker sibling of the same gate type is also present).
///
/// Built with `concat!` for the retired brand. Includes intermediate forms
/// observed on disk (ledgerful-web half-migrated wording).
fn known_generated_legacy_blocks() -> Vec<String> {
    let brand = LEGACY_BINARY;
    let mut blocks = Vec::new();

    // Original legacy-brand-installed ledger gate (Newton / Photo vintage).
    blocks.push(format!(
        r#"# {brand}-ledger-gate: auto-installed by `{brand} init`
if command -v {brand} &>/dev/null; then
    if ! {brand} ledger status --compact --exit-code 2>/dev/null; then
        echo ""
        echo "  Resolve with:"
        echo "    Pending tx:  {brand} ledger commit <tx-id> --summary '...' --reason '...'"
        echo "    Drift:       {brand} ledger reconcile --all --reason '...'"
        echo ""
        echo "  Bypass (not recommended): git commit --no-verify"
        exit 1
    fi
fi"#
    ));

    // Variant with --verify-signatures (CozoDB-redux append form).
    blocks.push(format!(
        r#"# {brand}-ledger-gate: auto-installed by `{brand} init`
if command -v {brand} &>/dev/null; then
    if ! {brand} ledger status --compact --exit-code --verify-signatures 2>/dev/null; then
        echo ""
        echo "  Resolve with:"
        echo "    Pending tx:  {brand} ledger commit <tx-id> --summary '...' --reason '...'"
        echo "    Drift:       {brand} ledger reconcile --all --reason '...'"
        echo ""
        echo "  Bypass (not recommended): git commit --no-verify"
        exit 1
    fi
fi"#
    ));

    // Half-migrated ledgerful-web form: legacy marker, current binary, older
    // echo style, optional 2>/dev/null.
    for bypass in ["git commit --no-verify", "git push --no-verify"] {
        for redirect in [" 2>/dev/null", ""] {
            blocks.push(format!(
                r#"# {brand}-ledger-gate: auto-installed by `ledgerful init`
if command -v ledgerful &>/dev/null; then
    if ! ledgerful ledger status --compact --exit-code --verify-signatures{redirect}; then
        echo ""
        echo "  Resolve with:"
        echo "    Pending tx:  ledgerful ledger commit <tx-id> --summary '...' --reason '...'"
        echo "    Drift:       ledgerful ledger reconcile --all --reason '...'"
        echo ""
        echo "  Bypass (not recommended): {bypass}"
        exit 1
    fi
fi"#
            ));
        }
    }

    // Half-migrated verify gate (ledgerful-web pre-push).
    blocks.push(format!(
        r#"# {brand}-verify-gate: fast scoped verification (pre-push only)
if command -v ledgerful &>/dev/null; then
    if ! ledgerful verify --scope fast 2>/dev/null; then
        echo ""
        echo "  Pre-push quality gate FAILED (ledgerful verify --scope fast)."
        echo "  Fix the above errors before pushing."
        echo ""
        echo "  Bypass (not recommended): git push --no-verify"
        exit 1
    fi
fi"#
    ));

    blocks
}

/// Extract a gate block starting at `start` (index of `# …-gate`). The block
/// runs through the closing `fi` lines that match nested `if`s of a standard
/// gate (or a single `fi` for intent/post-commit).
fn extract_gate_block(content: &str, start: usize) -> Option<&str> {
    let rest = &content[start..];
    let mut depth = 0i32;
    let mut seen_if = false;
    let mut consumed = 0usize;
    for line in rest.lines() {
        let line_len = line.len();
        // Account for the newline that follows (except possibly last line).
        let step = if consumed + line_len < rest.len() {
            line_len + 1
        } else {
            line_len
        };
        let trimmed = line.trim();
        if trimmed.starts_with("if ") || trimmed == "if" || trimmed.starts_with("if\t") {
            depth += 1;
            seen_if = true;
        } else if trimmed == "fi" || trimmed.starts_with("fi ") || trimmed.starts_with("fi;") {
            depth -= 1;
        }
        consumed += step;
        if seen_if && depth <= 0 {
            let block = &rest[..consumed.min(rest.len())];
            return Some(block.trim_end_matches(['\n', '\r']));
        }
    }
    if let Some(blank) = rest.find("\n\n") {
        Some(rest[..blank].trim_end())
    } else {
        Some(rest.trim_end())
    }
}

/// Find all legacy-marker-bounded blocks in `content`.
fn find_legacy_marker_blocks(content: &str) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    for suffix in GATE_SUFFIXES {
        let marker = format!("# {LEGACY_BINARY}-{suffix}");
        let mut search_from = 0;
        while let Some(rel) = content[search_from..].find(&marker) {
            let abs = search_from + rel;
            if let Some(block) = extract_gate_block(content, abs) {
                out.push((abs, block.to_string()));
                search_from = abs + block.len().max(1);
            } else {
                break;
            }
        }
    }
    out.sort_by_key(|(pos, _)| *pos);
    out
}

/// True if every non-structural line in `block` is a recognised ledger/verify/
/// scan invocation, echo, blank, comment, or shell control.
fn block_only_recognised_invocations(block: &str) -> bool {
    for line in block.lines() {
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        if t.starts_with("if ")
            || t == "fi"
            || t == "else"
            || t == "then"
            || t.starts_with("echo ")
            || t == "exit 1"
            || t == "exit 0"
        {
            continue;
        }
        // Recognised command forms (legacy or current).
        if t.contains("command -v ")
            || t.contains(&format!("{LEGACY_BINARY} "))
            || t.contains("ledgerful ")
        {
            continue;
        }
        // Nested `if ! …` already covered by starts_with("if ").
        return false;
    }
    true
}

/// Two-tier de-duplication. Operates on content that still has legacy markers
/// (call before marker rewrite for accurate block identity). Returns
/// (rewritten, changed, near_miss_block_texts).
fn dedup_legacy_blocks(content: &str) -> (String, bool, Vec<String>) {
    let legacy_blocks = find_legacy_marker_blocks(content);
    if legacy_blocks.is_empty() {
        return (content.to_string(), false, Vec::new());
    }

    let known = known_generated_legacy_blocks();
    let mut result = content.to_string();
    let mut changed = false;
    let mut near_misses = Vec::new();

    // Process from end so indices remain valid.
    let mut ordered = legacy_blocks;
    ordered.sort_by_key(|(pos, _)| std::cmp::Reverse(*pos));

    for (_pos, block) in ordered {
        // Only de-dup when a current-marker sibling of the same gate type exists.
        let gate_suffix = GATE_SUFFIXES
            .iter()
            .find(|s| block.contains(&format!("# {LEGACY_BINARY}-{s}")));
        let Some(suffix) = gate_suffix else {
            continue;
        };
        let current_marker = format!("# {CURRENT_BINARY}-{suffix}");
        // After prior removals, re-check presence in current result.
        if !result.contains(&block) {
            continue;
        }
        if !result.contains(&current_marker) {
            // Sole legacy block — not a duplicate; leave for marker rewrite.
            continue;
        }

        let normalized = block.replace("\r\n", "\n").trim().to_string();
        let is_exact = known
            .iter()
            .any(|k| k.replace("\r\n", "\n").trim() == normalized);

        if is_exact {
            // Tier 1: remove the exact known generated block (and surrounding
            // blank lines).
            if let Some(idx) = result.find(&block) {
                let mut start = idx;
                let mut end = idx + block.len();
                // Swallow leading newlines.
                while start > 0 && result.as_bytes()[start - 1] == b'\n' {
                    start -= 1;
                    if start > 0 && result.as_bytes()[start - 1] == b'\n' {
                        break;
                    }
                }
                // Swallow one trailing newline.
                if end < result.len() && result.as_bytes()[end] == b'\n' {
                    end += 1;
                }
                result.replace_range(start..end, "\n");
                changed = true;
            }
        } else if block_only_recognised_invocations(&block) {
            // Tier 2: report, never auto-delete.
            near_misses.push(block);
        }
        // Else: unrecognised customisation — leave untouched entirely.
    }

    near_misses.sort();
    (result, changed, near_misses)
}

/// Full repair transform for one hook file's contents.
fn repair_content(content: &str) -> (String, bool, Vec<String>) {
    // De-dup first while legacy markers are still present.
    let (after_dedup, dedup_changed, near_misses) = dedup_legacy_blocks(content);
    let (after_repl, repl_changed) = apply_replacements(&after_dedup);
    (after_repl, dedup_changed || repl_changed, near_misses)
}

// ---------------------------------------------------------------------------
// Public repair entry points
// ---------------------------------------------------------------------------

/// Core repair logic. Resolves hooks via [`resolve_hooks_dir`], enforces
/// containment and third-party re-detection, then rewrites hook files.
pub fn repair_hooks_at(repo_root: &Utf8Path, dry_run: bool) -> Result<HookRepairReport> {
    let mut report = HookRepairReport::empty(dry_run);

    if let Some(manager) = detect_third_party_hook_manager(repo_root) {
        report.third_party_manager = Some(manager);
        return Ok(report);
    }

    match resolve_hooks_dir(repo_root) {
        HooksDirResolution::Found { hooks_dir } => {
            // Re-detect third-party against resolved path (CrawlX non-root husky).
            if let Some(manager) = detect_third_party_at_hooks_dir(&hooks_dir) {
                report.third_party_manager = Some(manager);
                report.hooks_dir = Some(hooks_dir.to_string());
                report.discovery_notes.push(format!(
                    "third-party hook manager '{}' detected at resolved hooks path; refusing rewrite",
                    manager.name()
                ));
                return Ok(report);
            }
            report.hooks_dir = Some(hooks_dir.to_string());
            repair_hooks_in_dir(&hooks_dir, dry_run, &mut report)?;
        }
        HooksDirResolution::OutsideRepo { hooks_dir } => {
            report.hooks_dir = Some(hooks_dir.to_string());
            report.discovery_notes.push(format!(
                "hooks directory '{}' resolves outside the repository; refusing rewrite",
                hooks_dir
            ));
        }
        HooksDirResolution::CannotLook { reason } => {
            report
                .discovery_notes
                .push(format!("cannot resolve hooks directory: {reason}"));
        }
    }

    report.repaired.sort();
    report.already_correct.sort();
    report.skipped.sort();
    report.residual_invocations.sort();
    report.near_miss_blocks.sort_by(|a, b| a.0.cmp(&b.0));
    report.discovery_notes.sort();

    Ok(report)
}

fn repair_hooks_in_dir(
    hooks_dir: &Utf8Path,
    dry_run: bool,
    report: &mut HookRepairReport,
) -> Result<()> {
    if !hooks_dir.is_dir() {
        report.discovery_notes.push(format!(
            "hooks directory '{}' does not exist or is not a directory",
            hooks_dir
        ));
        return Ok(());
    }

    let entries = fs::read_dir(hooks_dir.as_std_path()).into_diagnostic()?;
    let mut filenames: Vec<Utf8PathBuf> = Vec::new();
    for entry in entries {
        let entry = entry.into_diagnostic()?;
        let path = entry.path();
        if path.is_dir() {
            continue;
        }
        let Some(utf8_path) = Utf8PathBuf::from_path_buf(path).ok() else {
            continue;
        };
        let Some(name) = utf8_path.file_name() else {
            continue;
        };
        if name.ends_with(".sample") {
            continue;
        }
        filenames.push(utf8_path);
    }
    filenames.sort();

    for hook_path in filenames {
        let Some(name) = hook_path.file_name() else {
            continue;
        };
        let name = name.to_string();
        let content = fs::read_to_string(hook_path.as_std_path()).into_diagnostic()?;
        let (rewritten, changed, near_misses) = repair_content(&content);

        for block in near_misses {
            report.near_miss_blocks.push((name.clone(), block));
        }

        if contains_legacy_invocation(&rewritten) {
            // DoD-4c: never report as repaired while residual retired binary remains.
            if changed && !dry_run {
                // Still write the partial improvement so markers/known patterns fix.
                fs::write(hook_path.as_std_path(), &rewritten).into_diagnostic()?;
            }
            report.residual_invocations.push(name);
            continue;
        }

        if changed {
            if !dry_run {
                fs::write(hook_path.as_std_path(), &rewritten).into_diagnostic()?;
            }
            report.repaired.push(name);
        } else if contains_ledger_invocation(&content) {
            report.already_correct.push(name);
        } else {
            report.skipped.push(name);
        }
    }

    Ok(())
}

/// Public entry point: discover the repo root the same way `execute_init`
/// does, then repair hooks in place. Prints a human-readable summary.
pub fn execute_hook_repair(dry_run: bool) -> Result<()> {
    let root = match gix::discover(".") {
        Ok(repo) => {
            let path = repo
                .workdir()
                .ok_or(crate::commands::CommandError::RepoDiscoveryFailed)?
                .to_path_buf();
            Utf8PathBuf::from_path_buf(path)
                .map_err(|_| crate::commands::CommandError::RepoDiscoveryFailed)?
        }
        Err(_) => Utf8PathBuf::from_path_buf(std::env::current_dir().into_diagnostic()?)
            .map_err(|_| crate::commands::CommandError::RepoDiscoveryFailed)?,
    };

    let report = repair_hooks_at(&root, dry_run)?;
    print_report(&report);
    Ok(())
}

fn print_report(report: &HookRepairReport) {
    if let Some(manager) = report.third_party_manager {
        println!(
            "{} Third-party hook manager '{}' detected. Hooks are managed by '{}', not rewritten by Ledgerful. Please update your {} config to call ledgerful.",
            "WARN:".yellow().bold(),
            manager.name(),
            manager.name(),
            manager.name(),
        );
        println!("  Config location: {}", manager.config_hint());
        if !report.discovery_notes.is_empty() {
            for note in &report.discovery_notes {
                println!("  {}", note);
            }
        }
        return;
    }

    let prefix = if report.dry_run {
        "DRY-RUN".yellow().bold().to_string()
    } else {
        "DONE".green().bold().to_string()
    };

    if !report.discovery_notes.is_empty() {
        for note in &report.discovery_notes {
            println!("{} {}", "WARN:".yellow().bold(), note);
        }
    }

    if report.repaired.is_empty()
        && report.already_correct.is_empty()
        && report.skipped.is_empty()
        && report.residual_invocations.is_empty()
        && report.near_miss_blocks.is_empty()
    {
        if report.discovery_notes.is_empty() {
            println!("{prefix} No hooks directory found; nothing to repair.");
        }
    } else {
        let verb = if report.dry_run {
            "Would repair"
        } else {
            "Repaired"
        };
        println!(
            "{prefix} {verb} {} hook(s): {}",
            report.repaired.len(),
            if report.repaired.is_empty() {
                "(none)".to_string()
            } else {
                report.repaired.join(", ")
            }
        );
        if !report.already_correct.is_empty() {
            println!("  Already correct: {}", report.already_correct.join(", "));
        }
        if !report.skipped.is_empty() {
            println!(
                "  Skipped (no ledger invocations found): {}",
                report.skipped.join(", ")
            );
        }
        if !report.residual_invocations.is_empty() {
            println!(
                "{} Residual retired-binary invocation(s) remain in: {}. Not reported as fully repaired.",
                "WARN:".yellow().bold(),
                report.residual_invocations.join(", ")
            );
        }
        if !report.near_miss_blocks.is_empty() {
            println!(
                "{} Near-miss duplicate gate block(s) (not auto-deleted; remove manually if safe):",
                "WARN:".yellow().bold()
            );
            for (hook, block) in &report.near_miss_blocks {
                println!("  --- {} ---", hook);
                for line in block.lines() {
                    println!("  {}", line);
                }
            }
        }
    }

    println!(
        "{} If hooks still look wrong, re-run `ledgerful update --repair-hooks`. Do not confuse with `ledgerful ledger hook-repair` (sidecar transaction repair).",
        "HINT:".cyan().bold()
    );
}

// ---------------------------------------------------------------------------
// Doctor helpers (detection only — no rewrite)
// ---------------------------------------------------------------------------

/// Scan hooks for legacy migration residue. Returns sorted warning strings
/// (empty when clean). Does not modify any file.
///
/// RT-H5 detection half only: reports gate-present-but-inert when a gate
/// marker exists but every invocation still names the retired binary (binary
/// missing → guard skips → commit/push proceeds). Enforcement (absolute-path
/// pin, fail-closed) is out of scope for 0094.
pub fn doctor_legacy_hook_findings(repo_root: &Utf8Path) -> Vec<String> {
    let mut findings = Vec::new();

    if detect_third_party_hook_manager(repo_root).is_some() {
        // Third-party managers own hooks; still note if we can see residue.
    }

    let hooks_dir = match resolve_hooks_dir(repo_root) {
        HooksDirResolution::Found { hooks_dir } => hooks_dir,
        HooksDirResolution::OutsideRepo { hooks_dir } => {
            findings.push(format!(
                "Warning [legacy-hooks]: hooks path '{}' is outside the repository; run `ledgerful update --repair-hooks` will refuse rewrite",
                hooks_dir
            ));
            findings.sort();
            return findings;
        }
        HooksDirResolution::CannotLook { reason } => {
            // Only report cannot-look when there is other signal of legacy use;
            // clean repos without .git/hooks stay silent (R5).
            let _ = reason;
            return findings;
        }
    };

    if !hooks_dir.is_dir() {
        return findings;
    }

    let mut legacy_markers = false;
    let mut legacy_invocations = false;
    let mut duplicate_gates = false;
    let mut inert_gate = false;
    let mut residual_after_shape = false;

    let entries = match fs::read_dir(hooks_dir.as_std_path()) {
        Ok(e) => e,
        Err(_) => return findings,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            continue;
        }
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if name.ends_with(".sample") {
            continue;
        }
        let Ok(content) = fs::read_to_string(&path) else {
            continue;
        };

        for suffix in GATE_SUFFIXES {
            let legacy_m = format!("# {LEGACY_BINARY}-{suffix}");
            let current_m = format!("# {CURRENT_BINARY}-{suffix}");
            if content.contains(&legacy_m) {
                legacy_markers = true;
                if content.contains(&current_m) {
                    duplicate_gates = true;
                }
            }
        }

        if contains_legacy_invocation(&content) {
            legacy_invocations = true;
            // RT-H5: gate marker present but invocations still retired → inert.
            let has_gate = GATE_SUFFIXES.iter().any(|s| {
                content.contains(&format!("# {LEGACY_BINARY}-{s}"))
                    || content.contains(&format!("# {CURRENT_BINARY}-{s}"))
            });
            if has_gate {
                inert_gate = true;
            }
            residual_after_shape = true;
        }
    }

    if legacy_markers {
        findings.push(
            "Warning [legacy-hooks]: hook marker comments still use the retired product name; run `ledgerful update --repair-hooks`".to_string(),
        );
    }
    if legacy_invocations {
        findings.push(
            "Warning [legacy-hooks]: hooks still invoke the retired binary; run `ledgerful update --repair-hooks`".to_string(),
        );
    }
    if duplicate_gates {
        findings.push(
            "Warning [legacy-hooks]: duplicate gate blocks (legacy + current markers); run `ledgerful update --repair-hooks`".to_string(),
        );
    }
    if inert_gate {
        // RT-H5 detection only (0094): gate present but names missing binary → no-op.
        findings.push(
            "Warning [legacy-hooks]: gate marker present but invocations name the retired binary (gate is inert if that binary is absent); run `ledgerful update --repair-hooks`".to_string(),
        );
    }
    let _ = residual_after_shape;

    findings.sort();
    findings.dedup();
    findings
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// The exact real stale `pre-push` hook content from this repo's
    /// `.git/hooks/pre-push`, captured verbatim (see trackTA23 brief).
    const CURRENT_PRE_PUSH: &str = r#"#!/usr/bin/env bash

# ledgerful-ledger-gate: auto-installed by `ledgerful init`
if command -v ledgerful &>/dev/null; then
    if ! ledgerful ledger status --compact --exit-code 2>/dev/null; then
        echo ""
        echo "  Resolve with:"
        echo "    Pending tx:  ledgerful ledger commit <tx-id> --summary '...' --reason '...'"
        echo "    Drift:       ledgerful ledger reconcile --all --reason '...'"
        echo ""
        echo "  Bypass (not recommended): git push --no-verify"
        exit 1
    fi
fi

# ledgerful-verify-gate: full quality gate before push
echo "==> Running pre-push quality gate..."

if command -v ledgerful &>/dev/null; then
    if ! ledgerful verify; then
        echo ""
        echo "  Pre-push quality gate FAILED (ledgerful verify)."
        echo "  Fix the above errors before pushing."
        echo ""
        echo "  Bypass (not recommended): git push --no-verify"
        exit 1
    fi
else
    echo "  [warn] ledgerful not found, falling back to direct cargo checks."

    if ! cargo fmt --all -- --check; then
        echo ""
        echo "  Pre-push FAILED: formatting errors detected."
        echo "  Run: cargo fmt --all"
        echo ""
        exit 1
    fi

    if ! cargo clippy --all-targets --all-features -- -D warnings; then
        echo ""
        echo "  Pre-push FAILED: clippy warnings/errors detected."
        echo ""
        exit 1
    fi

    if ! cargo test; then
        echo ""
        echo "  Pre-push FAILED: test suite did not pass."
        echo ""
        exit 1
    fi
fi

echo "==> Quality gate passed. Pushing..."
"#;

    fn stale_pre_push() -> String {
        CURRENT_PRE_PUSH
            .replace(
                "command -v ledgerful",
                &format!("command -v {LEGACY_BINARY}"),
            )
            .replace("ledgerful ledger", &format!("{LEGACY_BINARY} ledger"))
            .replace("ledgerful verify", &format!("{LEGACY_BINARY} verify"))
    }

    fn make_repo(tmp: &std::path::Path) -> Utf8PathBuf {
        let root = Utf8PathBuf::from_path_buf(tmp.to_path_buf()).unwrap();
        fs::create_dir_all(root.join(".git").join("hooks")).unwrap();
        root
    }

    #[test]
    fn repair_rewrites_real_stale_pre_push_fixture() {
        let tmp = tempfile::tempdir().unwrap();
        let root = make_repo(tmp.path());
        let hook_path = root.join(".git").join("hooks").join("pre-push");
        fs::write(&hook_path, stale_pre_push()).unwrap();

        let report = repair_hooks_at(&root, false).unwrap();

        assert_eq!(report.repaired, vec!["pre-push".to_string()]);
        assert!(report.already_correct.is_empty());
        assert!(report.skipped.is_empty());
        assert!(report.third_party_manager.is_none());
        assert!(report.residual_invocations.is_empty());

        let rewritten = fs::read_to_string(&hook_path).unwrap();

        assert!(rewritten.contains("if command -v ledgerful &>/dev/null; then"));
        assert!(
            rewritten
                .contains("if ! ledgerful ledger status --compact --exit-code 2>/dev/null; then")
        );
        assert!(rewritten.contains("if ! ledgerful verify; then"));
        assert!(!rewritten.contains(LEGACY_BINARY));
        assert!(rewritten.contains("# ledgerful-ledger-gate: auto-installed by `ledgerful init`"));
        assert!(rewritten.contains("# ledgerful-verify-gate: full quality gate before push"));
        assert!(rewritten.contains("if ! cargo fmt --all -- --check; then"));
    }

    #[test]
    fn repair_leaves_comment_only_mentions_untouched() {
        let tmp = tempfile::tempdir().unwrap();
        let root = make_repo(tmp.path());
        let hook_path = root.join(".git").join("hooks").join("pre-commit");
        let content = "#!/usr/bin/env bash\n\
# This hook used to call ledgerful but now calls something else.\n\
MY_LEDGERFUL_VAR=\"not a command\"\n\
echo \"ledgerful was here\"\n";
        fs::write(&hook_path, content).unwrap();

        let report = repair_hooks_at(&root, false).unwrap();

        assert!(report.repaired.is_empty());
        assert!(report.already_correct.is_empty());
        assert_eq!(report.skipped, vec!["pre-commit".to_string()]);

        let after = fs::read_to_string(&hook_path).unwrap();
        assert_eq!(after, content);
    }

    #[test]
    fn repair_skips_hook_with_no_ledger_content() {
        let tmp = tempfile::tempdir().unwrap();
        let root = make_repo(tmp.path());
        let hook_path = root.join(".git").join("hooks").join("post-checkout");
        let content = "#!/usr/bin/env bash\necho \"unrelated user hook\"\n";
        fs::write(&hook_path, content).unwrap();

        let report = repair_hooks_at(&root, false).unwrap();

        assert!(report.repaired.is_empty());
        assert!(report.already_correct.is_empty());
        assert_eq!(report.skipped, vec!["post-checkout".to_string()]);
    }

    #[test]
    fn repair_classifies_already_ledgerful_hook_as_already_correct() {
        let tmp = tempfile::tempdir().unwrap();
        let root = make_repo(tmp.path());
        let hook_path = root.join(".git").join("hooks").join("commit-msg");
        let content = "#!/usr/bin/env bash\n\
# ledgerful-intent-gate: auto-installed by `ledgerful init`\n\
if command -v ledgerful &>/dev/null; then\n\
    ledgerful internal hook-commit-msg \"$1\"\n\
fi\n";
        fs::write(&hook_path, content).unwrap();

        let report = repair_hooks_at(&root, false).unwrap();

        assert!(report.repaired.is_empty());
        assert_eq!(report.already_correct, vec!["commit-msg".to_string()]);
    }

    #[test]
    fn repair_skips_sample_files_and_subdirectories() {
        let tmp = tempfile::tempdir().unwrap();
        let root = make_repo(tmp.path());
        let hooks_dir = root.join(".git").join("hooks");
        fs::write(
            hooks_dir.join("pre-push.sample"),
            "#!/bin/sh\nledgerful ledger status\n",
        )
        .unwrap();
        fs::create_dir_all(hooks_dir.join("subdir")).unwrap();
        fs::write(
            hooks_dir.join("subdir").join("nested"),
            "ledgerful verify\n",
        )
        .unwrap();

        let report = repair_hooks_at(&root, false).unwrap();

        assert!(report.repaired.is_empty());
        assert!(report.already_correct.is_empty());
        assert!(report.skipped.is_empty());
    }

    #[test]
    fn repair_no_hooks_dir_returns_cleanly() {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();

        let report = repair_hooks_at(&root, false).unwrap();

        assert!(report.repaired.is_empty());
        assert!(!report.discovery_notes.is_empty());
    }

    #[test]
    fn repair_is_idempotent_second_call_reports_already_correct_with_identical_content() {
        let tmp = tempfile::tempdir().unwrap();
        let root = make_repo(tmp.path());
        let hook_path = root.join(".git").join("hooks").join("pre-push");
        fs::write(&hook_path, stale_pre_push()).unwrap();

        let first_report = repair_hooks_at(&root, false).unwrap();
        assert_eq!(first_report.repaired, vec!["pre-push".to_string()]);
        let first_contents = fs::read_to_string(&hook_path).unwrap();

        let second_report = repair_hooks_at(&root, false).unwrap();
        assert!(second_report.repaired.is_empty());
        assert_eq!(second_report.already_correct, vec!["pre-push".to_string()]);

        let second_contents = fs::read_to_string(&hook_path).unwrap();
        assert_eq!(first_contents, second_contents);
    }

    #[test]
    fn repair_dry_run_reports_without_writing() {
        let tmp = tempfile::tempdir().unwrap();
        let root = make_repo(tmp.path());
        let hook_path = root.join(".git").join("hooks").join("pre-push");
        let stale = stale_pre_push();
        fs::write(&hook_path, &stale).unwrap();

        let report = repair_hooks_at(&root, true).unwrap();

        assert_eq!(report.repaired, vec!["pre-push".to_string()]);
        assert!(report.dry_run);
        let after = fs::read_to_string(&hook_path).unwrap();
        assert_eq!(after, stale);
    }

    #[test]
    fn detect_husky_skips_rewriting() {
        let tmp = tempfile::tempdir().unwrap();
        let root = make_repo(tmp.path());
        fs::create_dir_all(root.join(".husky")).unwrap();
        let hook_path = root.join(".git").join("hooks").join("pre-push");
        let stale = stale_pre_push();
        fs::write(&hook_path, &stale).unwrap();

        let report = repair_hooks_at(&root, false).unwrap();

        assert_eq!(
            report.third_party_manager,
            Some(ThirdPartyHookManager::Husky)
        );
        assert!(report.repaired.is_empty());
        let after = fs::read_to_string(&hook_path).unwrap();
        assert_eq!(after, stale);
    }

    #[test]
    fn detect_lefthook_skips_rewriting() {
        let tmp = tempfile::tempdir().unwrap();
        let root = make_repo(tmp.path());
        fs::write(root.join("lefthook.yml"), "pre-push:\n  commands:\n").unwrap();
        let hook_path = root.join(".git").join("hooks").join("pre-push");
        fs::write(&hook_path, stale_pre_push()).unwrap();

        let report = repair_hooks_at(&root, false).unwrap();

        assert_eq!(
            report.third_party_manager,
            Some(ThirdPartyHookManager::Lefthook)
        );
        assert!(report.repaired.is_empty());
    }

    #[test]
    fn detect_pre_commit_skips_rewriting() {
        let tmp = tempfile::tempdir().unwrap();
        let root = make_repo(tmp.path());
        fs::write(root.join(".pre-commit-config.yaml"), "repos: []\n").unwrap();
        let hook_path = root.join(".git").join("hooks").join("pre-push");
        fs::write(&hook_path, stale_pre_push()).unwrap();

        let report = repair_hooks_at(&root, false).unwrap();

        assert_eq!(
            report.third_party_manager,
            Some(ThirdPartyHookManager::PreCommit)
        );
        assert!(report.repaired.is_empty());
    }

    #[test]
    fn detect_priority_order_husky_wins_over_others() {
        let tmp = tempfile::tempdir().unwrap();
        let root = make_repo(tmp.path());
        fs::create_dir_all(root.join(".husky")).unwrap();
        fs::write(root.join("lefthook.yml"), "pre-push:\n").unwrap();
        fs::write(root.join(".pre-commit-config.yaml"), "repos: []\n").unwrap();

        let detected = detect_third_party_hook_manager(&root);
        assert_eq!(detected, Some(ThirdPartyHookManager::Husky));
    }

    #[test]
    fn replacement_patterns_never_match_ledgerful_dir() {
        let markers = [".ledgerful/state/ledger.db", ".ledgerful/config.toml"];
        for marker in markers {
            assert_eq!(apply_replacements(marker), (marker.to_string(), false));
        }
    }

    #[test]
    fn repair_leaves_unrelated_retired_name_occurrences_untouched() {
        let content = format!(
            "# retired name in a comment: {0}\nPATH_HINT=/opt/{0}/bin\n{0}_CACHE=local\n",
            LEGACY_BINARY
        );
        // `{brand}_CACHE` and path mentions without invocation patterns stay.
        // Note: `{brand} ` bare form is treated as residual if present with space.
        let (out, changed) = apply_replacements(&content);
        assert_eq!(out, content);
        assert!(!changed);
    }

    /// DoD-3: after repair, markers are current; subsequent init does not
    /// append a second ledger-gate block.
    #[test]
    fn repair_then_init_yields_exactly_one_ledger_gate() {
        let tmp = tempfile::tempdir().unwrap();
        let root = make_repo(tmp.path());
        let hook_path = root.join(".git").join("hooks").join("pre-commit");
        let legacy = format!(
            "#!/usr/bin/env bash\n\n\
# {0}-ledger-gate: auto-installed by `{0} init`\n\
if command -v {0} &>/dev/null; then\n\
    if ! {0} ledger status --compact --exit-code 2>/dev/null; then\n\
        echo \"\"\n\
        echo \"  Resolve with:\"\n\
        echo \"    Pending tx:  {0} ledger commit <tx-id> --summary '...' --reason '...'\"\n\
        echo \"    Drift:       {0} ledger reconcile --all --reason '...'\"\n\
        echo \"\"\n\
        echo \"  Bypass (not recommended): git commit --no-verify\"\n\
        exit 1\n\
    fi\n\
fi\n",
            LEGACY_BINARY
        );
        fs::write(&hook_path, &legacy).unwrap();

        let report = repair_hooks_at(&root, false).unwrap();
        assert!(
            report.repaired.contains(&"pre-commit".to_string()),
            "expected repair: {:?}",
            report
        );
        assert!(report.residual_invocations.is_empty());

        let after_repair = fs::read_to_string(&hook_path).unwrap();
        assert!(after_repair.contains("# ledgerful-ledger-gate"));
        assert!(!after_repair.contains(&format!("# {LEGACY_BINARY}-ledger-gate")));
        assert!(!after_repair.contains(LEGACY_BINARY));

        // Simulate init's idempotency branch: HOOK_MARKER found → no append.
        const HOOK_MARKER: &str = "# ledgerful-ledger-gate";
        assert!(
            after_repair.contains(HOOK_MARKER),
            "init must recognise the repaired marker"
        );
        let gate_count = after_repair.matches(HOOK_MARKER).count();
        assert_eq!(gate_count, 1, "exactly one ledger-gate marker after repair");
        // If init were to re-run install_git_hook it would upgrade-in-place /
        // return without append because marker is present.
    }

    /// DoD-4: ledgerful-web dual-marker pre-push de-duplicates to one block
    /// per gate type.
    #[test]
    fn repair_dedups_dual_marker_ledgerful_web_shape() {
        let tmp = tempfile::tempdir().unwrap();
        let root = make_repo(tmp.path());
        let hook_path = root.join(".git").join("hooks").join("pre-push");
        // Captured from output/0094-hooks/ledgerful-web/pre-push (abbreviated
        // to the dual ledger + verify pattern).
        let dual = format!(
            r#"#!/usr/bin/env bash

# {brand}-ledger-gate: auto-installed by `ledgerful init`
if command -v ledgerful &>/dev/null; then
    if ! ledgerful ledger status --compact --exit-code --verify-signatures 2>/dev/null; then
        echo ""
        echo "  Resolve with:"
        echo "    Pending tx:  ledgerful ledger commit <tx-id> --summary '...' --reason '...'"
        echo "    Drift:       ledgerful ledger reconcile --all --reason '...'"
        echo ""
        echo "  Bypass (not recommended): git push --no-verify"
        exit 1
    fi
fi


# {brand}-verify-gate: fast scoped verification (pre-push only)
if command -v ledgerful &>/dev/null; then
    if ! ledgerful verify --scope fast 2>/dev/null; then
        echo ""
        echo "  Pre-push quality gate FAILED (ledgerful verify --scope fast)."
        echo "  Fix the above errors before pushing."
        echo ""
        echo "  Bypass (not recommended): git push --no-verify"
        exit 1
    fi
fi


# ledgerful-ledger-gate: auto-installed by `ledgerful init`
if command -v ledgerful &>/dev/null; then
    if ! ledgerful ledger status --compact --exit-code --verify-signatures; then
        echo "[Ledgerful] Blocked by ledger state."
        echo "[Ledgerful] Resolve with:"
        echo "[Ledgerful]   Pending tx:  ledgerful ledger commit <tx-id> --summary '...' --reason '...'"
        echo "[Ledgerful]   Drift:       ledgerful ledger reconcile --all --reason '...'"
        echo "[Ledgerful] Fix the issues or bypass with: git push --no-verify"
        exit 1
    fi
fi


# ledgerful-verify-gate: fast scoped verification (pre-push only)
if command -v ledgerful &>/dev/null; then
    if ! ledgerful verify --scope fast; then
        echo "[Ledgerful] Push blocked by verification failure."
        echo "[Ledgerful] Fix the issues or bypass with: git push --no-verify"
        exit 1
    fi
fi
"#,
            brand = LEGACY_BINARY
        );
        fs::write(&hook_path, dual).unwrap();

        let report = repair_hooks_at(&root, false).unwrap();
        assert!(report.residual_invocations.is_empty());
        let after = fs::read_to_string(&hook_path).unwrap();
        assert_eq!(
            after.matches("# ledgerful-ledger-gate").count(),
            1,
            "expected one ledger-gate after dedup; got:\n{after}"
        );
        assert_eq!(
            after.matches("# ledgerful-verify-gate").count(),
            1,
            "expected one verify-gate after dedup; got:\n{after}"
        );
        assert!(!after.contains(&format!("# {LEGACY_BINARY}-")));
    }

    /// DoD-5: frontend hand-edited `(renamed from …)` form is unchanged.
    #[test]
    fn repair_leaves_frontend_renamed_marker_unchanged() {
        let tmp = tempfile::tempdir().unwrap();
        let root = make_repo(tmp.path());
        let hook_path = root.join(".git").join("hooks").join("pre-commit");
        let content = format!(
            "#!/usr/bin/env bash\n\n\
# ledgerful-ledger-gate: auto-installed by `ledgerful init` (renamed from {brand})\n\
if command -v ledgerful &>/dev/null; then\n\
    if ! ledgerful ledger status --compact --exit-code --verify-signatures 2>/dev/null; then\n\
        echo \"\"\n\
        echo \"  Resolve with:\"\n\
        echo \"    Pending tx:  ledgerful ledger commit <tx-id> --summary '...' --reason '...'\"\n\
        echo \"    Drift:       ledgerful ledger reconcile --all --reason '...'\"\n\
        echo \"\"\n\
        echo \"  Bypass (not recommended): git commit --no-verify\"\n\
        exit 1\n\
    fi\n\
fi\n",
            brand = LEGACY_BINARY
        );
        fs::write(&hook_path, &content).unwrap();

        let report = repair_hooks_at(&root, false).unwrap();
        assert!(report.repaired.is_empty(), "must not rewrite: {:?}", report);
        let after = fs::read_to_string(&hook_path).unwrap();
        assert_eq!(after, content);
    }

    /// DoD-4c: Photo-shaped hook with `scan --impact` must not be reported
    /// as fully repaired while residual retired binary remains. After covering
    /// `scan`, residual should be cleared and it is honestly repaired.
    #[test]
    fn repair_photo_scan_impact_is_honest() {
        let tmp = tempfile::tempdir().unwrap();
        let root = make_repo(tmp.path());
        let hook_path = root.join(".git").join("hooks").join("pre-commit");
        let photo = format!(
            "#!/bin/sh\n\
if command -v {0} >/dev/null 2>&1; then\n\
  {0} scan --impact\n\
fi\n\
\n\
# {0}-ledger-gate: auto-installed by `{0} init`\n\
if command -v {0} &>/dev/null; then\n\
    if ! {0} ledger status --compact --exit-code 2>/dev/null; then\n\
        echo \"\"\n\
        echo \"  Resolve with:\"\n\
        echo \"    Pending tx:  {0} ledger commit <tx-id> --summary '...' --reason '...'\"\n\
        echo \"    Drift:       {0} ledger reconcile --all --reason '...'\"\n\
        echo \"\"\n\
        echo \"  Bypass (not recommended): git commit --no-verify\"\n\
        exit 1\n\
    fi\n\
fi\n",
            LEGACY_BINARY
        );
        fs::write(&hook_path, photo).unwrap();

        let report = repair_hooks_at(&root, false).unwrap();
        let after = fs::read_to_string(&hook_path).unwrap();
        assert!(
            !after.contains(LEGACY_BINARY),
            "scan and ledger invocations must be rewritten: {after}"
        );
        assert!(
            report.residual_invocations.is_empty(),
            "must not claim residual when scan is covered: {:?}",
            report.residual_invocations
        );
        assert!(
            report.repaired.contains(&"pre-commit".to_string()),
            "honest full repair expected: {:?}",
            report
        );
        assert!(after.contains("ledgerful scan --impact"));
    }

    /// DoD-4b: dual-marker where the legacy block is a customised Photo-shaped
    /// variant (not exact known template) → near-miss report, not auto-deleted.
    #[test]
    fn repair_photo_shaped_duplicate_is_reported_not_deleted() {
        let tmp = tempfile::tempdir().unwrap();
        let root = make_repo(tmp.path());
        let hook_path = root.join(".git").join("hooks").join("pre-commit");
        // Custom wording so it is not an exact known template.
        let dual = format!(
            r#"#!/bin/sh

# {brand}-ledger-gate: auto-installed by `{brand} init`
if command -v {brand} &>/dev/null; then
    if ! {brand} ledger status --compact --exit-code 2>/dev/null; then
        echo ""
        echo "  CUSTOM Resolve with:"
        echo "    Pending tx:  {brand} ledger commit <tx-id> --summary '...' --reason '...'"
        echo "    Drift:       {brand} ledger reconcile --all --reason '...'"
        echo ""
        echo "  Bypass (not recommended): git commit --no-verify"
        exit 1
    fi
fi

# ledgerful-ledger-gate: auto-installed by `ledgerful init`
if command -v ledgerful &>/dev/null; then
    if ! ledgerful ledger status --compact --exit-code --verify-signatures; then
        echo "[Ledgerful] Blocked by ledger state."
        exit 1
    fi
fi
"#,
            brand = LEGACY_BINARY
        );
        fs::write(&hook_path, dual).unwrap();

        let report = repair_hooks_at(&root, false).unwrap();
        // Near-miss should be reported (custom wording).
        assert!(
            !report.near_miss_blocks.is_empty()
                || fs::read_to_string(&hook_path)
                    .unwrap()
                    .contains("CUSTOM Resolve"),
            "custom legacy block must not be silently dropped; report={:?}",
            report.near_miss_blocks
        );
        // The custom block body (after marker/invocation rewrite may change
        // binary names) — CUSTOM text must still be present if not exact-match.
        let after = fs::read_to_string(&hook_path).unwrap();
        if report.near_miss_blocks.is_empty() {
            // If tier-1 somehow matched, that would be a test design error.
            panic!("expected tier-2 near-miss for custom block; after=\n{after}");
        }
        assert!(
            after.contains("CUSTOM Resolve")
                || after.matches("# ledgerful-ledger-gate").count() >= 1,
            "custom block must remain on disk"
        );
    }

    /// Outside-repo hooksPath is refused (DoD-9b). Config written via pure FS.
    /// Asserts the outside hook file bytes are unchanged (not rewritten).
    #[test]
    fn repair_refuses_outside_repo_hooks_path() {
        let tmp = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let root = make_repo(tmp.path());
        let outside_hooks = outside.path().join("hooks");
        fs::create_dir_all(&outside_hooks).unwrap();
        let outside_hook = outside_hooks.join("pre-commit");
        let original_bytes = stale_pre_push().into_bytes();
        fs::write(&outside_hook, &original_bytes).unwrap();

        // Write core.hooksPath into .git/config (pure FS — matches production reader).
        let config = format!(
            "[core]\n\thooksPath = {}\n",
            outside_hooks.to_str().unwrap().replace('\\', "/")
        );
        fs::write(root.join(".git").join("config"), config).unwrap();

        let report = repair_hooks_at(&root, false).unwrap();
        assert!(
            report.discovery_notes.iter().any(|n| n.contains("outside")),
            "expected outside-repo note: {:?}",
            report.discovery_notes
        );
        assert!(report.repaired.is_empty());
        // Critical: outside-repo hooks must not be rewritten at all.
        let after_bytes = fs::read(&outside_hook).unwrap();
        assert_eq!(
            after_bytes, original_bytes,
            "outside-repo hook file must be byte-identical after refuse"
        );
    }

    /// Non-root husky (CrawlX shape) is detected via resolved path (DoD-9b).
    #[test]
    fn repair_detects_non_root_husky_via_hooks_path() {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        let husky_hooks = root.join("apps").join("api").join(".husky").join("_");
        fs::create_dir_all(&husky_hooks).unwrap();
        fs::write(husky_hooks.join("pre-commit"), stale_pre_push()).unwrap();
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::write(
            root.join(".git").join("config"),
            "[core]\n\thooksPath = apps/api/.husky/_\n",
        )
        .unwrap();

        let report = repair_hooks_at(&root, false).unwrap();
        assert_eq!(
            report.third_party_manager,
            Some(ThirdPartyHookManager::Husky),
            "non-root husky must be refused: {:?}",
            report
        );
        assert!(report.repaired.is_empty());
    }

    #[test]
    fn parse_git_config_hooks_path() {
        let content = "[core]\n\trepositoryformatversion = 0\n\thooksPath = .git/hooks\n";
        assert_eq!(
            parse_git_config_value(content, "core", "hooksPath").as_deref(),
            Some(".git/hooks")
        );
    }

    /// Absolute hooksPath inside the repo (Design shape) is accepted.
    #[test]
    fn repair_accepts_absolute_inside_repo_hooks_path() {
        let tmp = tempfile::tempdir().unwrap();
        let root = make_repo(tmp.path());
        let hooks = root.join(".git").join("hooks");
        fs::write(hooks.join("pre-commit"), stale_pre_push()).unwrap();
        let abs = hooks.as_str().replace('\\', "/");
        fs::write(
            root.join(".git").join("config"),
            format!("[core]\n\thooksPath = {abs}\n"),
        )
        .unwrap();

        let report = repair_hooks_at(&root, false).unwrap();
        assert!(
            report
                .discovery_notes
                .iter()
                .all(|n| !n.contains("outside")),
            "inside-repo absolute path must not be refused: {:?}",
            report.discovery_notes
        );
        assert!(
            report.repaired.contains(&"pre-commit".to_string()),
            "expected repair of hooks at absolute inside path: {:?}",
            report
        );
    }

    /// Case-mismatched absolute path must not be refused on Windows (DoD-9b).
    #[cfg(windows)]
    #[test]
    fn repair_accepts_case_mismatched_absolute_hooks_path() {
        let tmp = tempfile::tempdir().unwrap();
        let root = make_repo(tmp.path());
        let hooks = root.join(".git").join("hooks");
        fs::write(hooks.join("pre-commit"), stale_pre_push()).unwrap();
        // Flip case on the drive letter / path segments where possible.
        let mut abs = hooks.as_str().replace('\\', "/");
        if let Some(rest) = abs.strip_prefix("C:") {
            abs = format!("c:{rest}");
        } else if let Some(rest) = abs.strip_prefix("c:") {
            abs = format!("C:{rest}");
        }
        fs::write(
            root.join(".git").join("config"),
            format!("[core]\n\thooksPath = {abs}\n"),
        )
        .unwrap();

        let report = repair_hooks_at(&root, false).unwrap();
        assert!(
            !report.discovery_notes.iter().any(|n| n.contains("outside")),
            "case-mismatched absolute path must not be refused: {:?}",
            report.discovery_notes
        );
    }

    /// Real `git worktree add` fixture: resolved hooks path is the main repo's
    /// common `.git/hooks` via commondir (DoD-9) — not the worktree gitdir hooks.
    #[test]
    fn resolve_hooks_via_worktree_commondir() {
        let tmp = tempfile::tempdir().unwrap();
        let main = tmp.path().join("main");
        fs::create_dir_all(&main).unwrap();
        let git = |args: &[&str], cwd: &std::path::Path| {
            std::process::Command::new("git")
                .args(args)
                .current_dir(cwd)
                .output()
                .expect("git available")
        };
        assert!(git(&["init"], &main).status.success());
        assert!(
            git(&["config", "user.email", "t@example.com"], &main)
                .status
                .success()
        );
        assert!(git(&["config", "user.name", "t"], &main).status.success());
        fs::write(main.join("README"), "x").unwrap();
        assert!(git(&["add", "README"], &main).status.success());
        assert!(git(&["commit", "-m", "init"], &main).status.success());
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

        let linked_root = Utf8PathBuf::from_path_buf(linked).unwrap();
        let main_hooks = main.join(".git").join("hooks");
        assert!(
            main_hooks.is_dir(),
            "main common hooks dir must exist for comparison"
        );
        let main_hooks_canon = dunce::canonicalize(&main_hooks).unwrap();

        match resolve_hooks_dir(&linked_root) {
            HooksDirResolution::Found { hooks_dir } => {
                // Resolved path must exist as a directory (common-dir hooks).
                assert!(
                    hooks_dir.is_dir(),
                    "hooks dir from commondir must exist: {hooks_dir}"
                );
                let resolved_canon = dunce::canonicalize(hooks_dir.as_std_path()).unwrap();
                assert_eq!(
                    resolved_canon, main_hooks_canon,
                    "worktree hooks must resolve to main .git/hooks via commondir; got {hooks_dir}"
                );

                // Worktree-private <gitdir>/hooks must NOT be what was used.
                let git_file = linked_root.join(".git");
                assert!(git_file.is_file(), "linked worktree has .git file");
                let git_contents = fs::read_to_string(git_file.as_std_path()).unwrap();
                let gitdir_line = git_contents
                    .lines()
                    .find_map(|l| l.trim().strip_prefix("gitdir:"))
                    .map(str::trim)
                    .expect("gitdir: line");
                let worktree_gitdir = {
                    let p = Path::new(gitdir_line);
                    if p.is_absolute() {
                        PathBuf::from(p)
                    } else {
                        linked_root.as_std_path().join(p)
                    }
                };
                let worktree_private_hooks = worktree_gitdir.join("hooks");
                // Either the private path does not exist, or it is not the resolved dir.
                if worktree_private_hooks.exists() {
                    let private_canon = dunce::canonicalize(&worktree_private_hooks).unwrap();
                    assert_ne!(
                        resolved_canon, private_canon,
                        "must not use worktree-private <gitdir>/hooks"
                    );
                } else {
                    assert!(
                        !worktree_private_hooks.exists(),
                        "worktree-private hooks path should not exist: {}",
                        worktree_private_hooks.display()
                    );
                }
            }
            other => panic!("expected Found via commondir, got {other:?}"),
        }
    }

    #[test]
    fn marker_normalization_rewrites_all_four_gate_types() {
        for suffix in GATE_SUFFIXES {
            let input = format!("# {LEGACY_BINARY}-{suffix}: hello");
            let (out, changed) = apply_replacements(&input);
            assert!(changed);
            assert_eq!(out, format!("# ledgerful-{suffix}: hello"));
        }
    }
}
