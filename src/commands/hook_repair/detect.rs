use camino::Utf8Path;

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
///
/// Public so product template refresh (0121) shares the same guard as legacy repair.
pub fn detect_third_party_at_hooks_dir(hooks_dir: &Utf8Path) -> Option<ThirdPartyHookManager> {
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
