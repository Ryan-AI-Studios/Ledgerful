use super::files::{ReadFailure, Stamp, read_directory, read_stamped};
use crate::index::languages::Language;
use crate::index::orchestrator::{BINARY_EXTENSIONS, SUPPORTED_EXTENSIONS};
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::DirEntry;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

const MAX_DEPTH: usize = 256;
const MAX_PATH_BYTES: usize = 4096;
const MAX_RULE_BYTES: usize = 4096;
const MAX_RULES_PER_INPUT: usize = 4096;
const MAX_RULES: usize = 50_000;

#[derive(Default)]
pub(super) struct Discovery {
    pub(super) sources: BTreeMap<String, PathBuf>,
    pub(super) stamps: Vec<(PathBuf, Stamp)>,
    pub(super) missing_inputs: Vec<PathBuf>,
    pub(super) other: bool,
    pub(super) reasons: BTreeSet<&'static str>,
}

struct Walk<'a> {
    root: &'a Path,
    result: Discovery,
    entries_left: usize,
    max_depth: usize,
    max_path_bytes: usize,
    rules_left: usize,
    file_limit: u64,
    bytes_left: &'a mut u64,
    git: bool,
    git_exclude: Gitignore,
    global: Gitignore,
    local_rules: Vec<(Gitignore, Gitignore)>,
}

pub(super) fn discover(
    root: &Path,
    repo: Option<&gix::Repository>,
    entry_limit: usize,
    file_limit: u64,
    bytes_left: &mut u64,
) -> Discovery {
    discover_with_walk_bounds(
        root,
        repo,
        entry_limit,
        file_limit,
        bytes_left,
        MAX_DEPTH,
        MAX_PATH_BYTES,
    )
}

pub(super) fn discover_with_walk_bounds(
    root: &Path,
    repo: Option<&gix::Repository>,
    entry_limit: usize,
    file_limit: u64,
    bytes_left: &mut u64,
    max_depth: usize,
    max_path_bytes: usize,
) -> Discovery {
    if !cfg!(any(target_os = "linux", windows)) {
        return Discovery {
            reasons: BTreeSet::from(["discoveryUnavailable"]),
            ..Discovery::default()
        };
    }
    let mut walk = Walk {
        root,
        result: Discovery::default(),
        entries_left: entry_limit.saturating_sub(1),
        max_depth,
        max_path_bytes,
        rules_left: MAX_RULES,
        file_limit,
        bytes_left,
        git: repo.is_some(),
        git_exclude: Gitignore::empty(),
        global: Gitignore::empty(),
        local_rules: Vec::new(),
    };
    if let Some(repo) = repo {
        match std::env::current_dir() {
            Ok(cwd) => {
                if let Some(path) = walk.global_exclude_path(&cwd) {
                    walk.global = walk
                        .load_ignore(&cwd, &path)
                        .unwrap_or_else(Gitignore::empty);
                }
            }
            Err(_) => {
                walk.result.reasons.insert("ignoreInputError");
            }
        }
        let common = repo.common_dir();
        let common = if common.is_absolute() {
            common.to_owned()
        } else {
            root.join(common)
        };
        walk.git_exclude = walk
            .load_ignore(root, &common.join("info/exclude"))
            .unwrap_or_else(Gitignore::empty);
    }
    // The native walker also inherits .ignore filters above the project.
    let mut parents: Vec<_> = root
        .ancestors()
        .skip(1)
        .filter(|dir| dir.has_root())
        .collect();
    if repo.is_some() {
        if parents.len() >= max_depth {
            walk.result.reasons.insert("depthLimit");
        } else {
            parents.reverse();
            for parent in parents {
                let rules = walk.load_ignore(parent, &parent.join(".ignore"));
                if let Some(rules) = rules {
                    walk.local_rules.push((rules, Gitignore::empty()));
                }
            }
        }
    }
    let base_ignores_unresolved = walk.result.reasons.contains("ignoreInputError");
    if entry_limit == 0 || !walk.visit(root, 0) {
        return Discovery {
            reasons: BTreeSet::from(["entryLimit"]),
            ..Discovery::default()
        };
    }
    if walk.result.reasons.iter().any(|reason| {
        matches!(
            *reason,
            "fileLimit" | "contentLimit" | "ignoreRuleLimit" | "depthLimit" | "pathLimit"
        )
    }) {
        return Discovery {
            reasons: walk.result.reasons,
            ..Discovery::default()
        };
    }
    if base_ignores_unresolved {
        walk.result.other = false;
    }
    walk.result
}

impl Walk<'_> {
    /// Mirror ignore 0.4.33 global selection, with bounded reads and stamps.
    /// The native walker considers these files in order and does not consult
    /// the repository's own config for this setting.
    fn global_exclude_path(&mut self, cwd: &Path) -> Option<PathBuf> {
        static EXCLUDES: LazyLock<Option<regex::bytes::Regex>> = LazyLock::new(|| {
            regex::bytes::Regex::new(r#"(?im-u)^\s*excludesfile\s*=\s*"?\s*(\S+?)\s*"?\s*$"#).ok()
        });
        let Some(excludes) = EXCLUDES.as_ref() else {
            self.result.reasons.insert("ignoreInputError");
            return None;
        };
        #[allow(deprecated)]
        let home = std::env::home_dir();
        let env_path = |name| {
            std::env::var_os(name)
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
        };
        let xdg =
            env_path("XDG_CONFIG_HOME").or_else(|| home.as_ref().map(|home| home.join(".config")));
        let configs = [
            env_path("GIT_CONFIG_GLOBAL"),
            home.as_ref().map(|home| home.join(".gitconfig")),
            xdg.as_ref().map(|xdg| xdg.join("git/config")),
            Some(env_path("GIT_CONFIG_SYSTEM").unwrap_or_else(|| PathBuf::from("/etc/gitconfig"))),
        ];
        for path in configs.into_iter().flatten() {
            let path = cwd.join(path);
            let bytes = match self.read_ignore_input(&path) {
                Some(Some(bytes)) => bytes,
                Some(None) => continue,
                None => return None,
            };
            let Some(path) = excludes
                .captures(&bytes)
                .and_then(|captures| captures.get(1))
                .and_then(|value| std::str::from_utf8(value.as_bytes()).ok())
            else {
                continue;
            };
            let path = match &home {
                Some(home) => PathBuf::from(path.replace('~', &home.to_string_lossy())),
                None => PathBuf::from(path),
            };
            // A relative rule path is interpreted against the same current
            // directory as the native walker's global matcher.
            return Some(cwd.join(path));
        }
        xdg.map(|xdg| cwd.join(xdg).join("git/ignore"))
    }

    // Some(None) is confirmed absent. None means the input did not yield
    // trustworthy bytes and the caller must preserve an unknown result.
    fn read_ignore_input(&mut self, path: &Path) -> Option<Option<Vec<u8>>> {
        let limit = self.file_limit.min(*self.bytes_left);
        let (read, used) = read_stamped(path, limit, false, |input| {
            let mut bytes = Vec::new();
            input.read_to_end(&mut bytes)?;
            Ok(bytes)
        });
        *self.bytes_left = self.bytes_left.saturating_sub(used);
        let (bytes, stamp) = match read {
            Ok(result) => result,
            Err(ReadFailure::Missing) => {
                self.result.missing_inputs.push(path.to_owned());
                return Some(None);
            }
            Err(ReadFailure::Capped) => {
                self.result.reasons.insert(if limit == self.file_limit {
                    "fileLimit"
                } else {
                    "contentLimit"
                });
                self.result.reasons.insert("ignoreInputError");
                return None;
            }
            Err(_) => {
                self.result.reasons.insert("ignoreInputError");
                return None;
            }
        };
        self.result.stamps.push((path.to_owned(), stamp));
        Some(Some(bytes))
    }

    fn load_ignore(&mut self, directory: &Path, path: &Path) -> Option<Gitignore> {
        let Some(bytes) = self.read_ignore_input(path)? else {
            return Some(Gitignore::empty());
        };
        let Ok(text) = std::str::from_utf8(&bytes) else {
            self.result.reasons.insert("ignoreInputError");
            return None;
        };
        let mut builder = GitignoreBuilder::new(directory);
        let text = text.strip_prefix('\u{feff}').unwrap_or(text);
        for (number, line) in text.lines().enumerate() {
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if number >= MAX_RULES_PER_INPUT || self.rules_left == 0 || line.len() > MAX_RULE_BYTES
            {
                self.result.reasons.insert("ignoreInputError");
                self.result.reasons.insert("ignoreRuleLimit");
                return None;
            }
            self.rules_left -= 1;
            if builder.add_line(None, line).is_err() {
                self.result.reasons.insert("ignoreInputError");
                return None;
            }
        }
        match builder.build() {
            Ok(rules) => Some(rules),
            Err(_) => {
                self.result.reasons.insert("ignoreInputError");
                None
            }
        }
    }

    fn visible(&self, path: &Path, hidden: bool, directory: bool) -> bool {
        let layered = self
            .local_rules
            .iter()
            .rev()
            .map(|(ignore, _)| ignore)
            .chain(
                self.local_rules
                    .iter()
                    .rev()
                    .map(|(_, gitignore)| gitignore),
            )
            .chain([&self.git_exclude, &self.global]);
        for filters in layered {
            let matched = filters.matched(path, directory);
            if !matched.is_none() {
                return matched.is_whitelist();
            }
        }
        !hidden
    }

    fn visit(&mut self, path: &Path, depth: usize) -> bool {
        if depth >= self.max_depth {
            self.result.reasons.insert("depthLimit");
            return true;
        }
        if path.as_os_str().as_encoded_bytes().len() > self.max_path_bytes {
            self.result.reasons.insert("pathLimit");
            return true;
        }

        // The locked handle supplies directory entries. The budget is charged
        // before ignoring a child or inspecting its type.
        let listing = read_directory(path, |children| {
            let mut entries = Vec::new();
            for child in children {
                if self.entries_left == 0 {
                    return None;
                }
                self.entries_left -= 1;
                match child.and_then(|entry| {
                    let kind = entry.file_type()?;
                    let (hidden, reparse) = entry_flags(&entry)?;
                    Ok((entry.file_name(), kind, hidden, reparse))
                }) {
                    Ok(entry) => entries.push(entry),
                    Err(_) => {
                        self.result.reasons.insert("walkError");
                    }
                }
            }
            Some(entries)
        });
        let (mut entries, stamp) = match listing {
            Ok((Some(entries), stamp)) => (entries, stamp),
            Ok((None, _)) => return false,
            Err(_) => {
                self.result.reasons.insert("walkError");
                return true;
            }
        };
        self.result.stamps.push((path.to_owned(), stamp));
        // A nested repository has its own precedence and common directory.
        // Leave its inventory unknown rather than attributing outer rules.
        if depth > 0
            && entries
                .iter()
                .any(|(name, _, _, _)| name == ".git" || name == ".jj")
        {
            self.result.reasons.insert("ignoreInputError");
            return true;
        }
        let ignore = self.load_ignore(path, &path.join(".ignore"));
        let gitignore = if self.git {
            self.load_ignore(path, &path.join(".gitignore"))
        } else {
            Some(Gitignore::empty())
        };
        let (Some(ignore), Some(gitignore)) = (ignore, gitignore) else {
            return true;
        };
        self.local_rules.push((ignore, gitignore));

        entries.sort_unstable_by(|left, right| left.0.cmp(&right.0));
        for (name, kind, hidden, reparse) in entries {
            let child = path.join(&name);
            if reparse || !self.visible(&child, hidden, kind.is_dir()) {
                continue;
            }
            if name == ".git" {
                self.result.reasons.insert("ignoreInputError");
                continue;
            }
            if kind.is_dir() {
                if !self.visit(&child, depth + 1) {
                    return false;
                }
            } else if kind.is_file() {
                self.observe_file(&child);
            }
        }
        self.local_rules.pop();
        true
    }

    fn observe_file(&mut self, path: &Path) {
        let Some(extension) = path.extension() else {
            return;
        };
        let Some(extension) = extension.to_str() else {
            self.result.other = true;
            return;
        };
        if extension.is_empty() || BINARY_EXTENSIONS.contains(&extension) {
            return;
        }
        if SUPPORTED_EXTENSIONS.contains(&extension) {
            if let Some(relative) = path.strip_prefix(self.root).ok().and_then(Path::to_str) {
                if relative.len() > self.max_path_bytes {
                    self.result.reasons.insert("pathLimit");
                    return;
                }
                if self
                    .result
                    .sources
                    .insert(relative.replace('\\', "/"), path.to_owned())
                    .is_some()
                {
                    self.result.reasons.insert("inventoryUncertain");
                }
            } else {
                self.result.reasons.insert("unreadablePath");
            }
        } else if Language::from_extension(extension).is_none() {
            self.result.other = true;
        }
    }
}

fn entry_flags(entry: &DirEntry) -> std::io::Result<(bool, bool)> {
    let dotted = entry.file_name().as_encoded_bytes().starts_with(b".");
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_ATTRIBUTE_HIDDEN, FILE_ATTRIBUTE_REPARSE_POINT,
        };
        let flags = entry.metadata()?.file_attributes();
        Ok((
            dotted || flags & FILE_ATTRIBUTE_HIDDEN != 0,
            flags & FILE_ATTRIBUTE_REPARSE_POINT != 0,
        ))
    }
    #[cfg(not(windows))]
    Ok((dotted, false))
}

#[cfg(all(test, any(target_os = "linux", windows)))]
mod tests {
    use super::*;

    #[rstest::rstest]
    #[case(false, false, "depthLimit")]
    #[case(true, false, "depthLimit")]
    #[case(false, true, "pathLimit")]
    #[case(true, true, "pathLimit")]
    fn traversal_bound_discards_provisional_source_and_other(
        #[case] reversed: bool,
        #[case] path_bound: bool,
        #[case] reason: &str,
    ) {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        let order = if reversed {
            ["z-deeper-directory", "a"]
        } else {
            ["a", "z-deeper-directory"]
        };
        for name in order {
            std::fs::create_dir(root.join(name)).unwrap();
        }
        std::fs::write(root.join("a/main.rs"), "fn a() {}").unwrap();
        std::fs::write(root.join("a/other.unknown"), "").unwrap();
        std::fs::create_dir(root.join("z-deeper-directory/extra")).unwrap();
        let mut bytes = 4096;
        let max_depth = if path_bound { MAX_DEPTH } else { 2 };
        let max_path = if path_bound {
            root.as_os_str().as_encoded_bytes().len() + 8
        } else {
            MAX_PATH_BYTES
        };
        let result =
            discover_with_walk_bounds(root, None, 50_000, 4096, &mut bytes, max_depth, max_path);
        assert_eq!(result.reasons, BTreeSet::from([reason]));
        assert!(result.sources.is_empty());
        assert!(!result.other);
        assert!(result.stamps.is_empty());
    }
}
