//! Path-identity helpers for federated links (0184).
//!
//! Canonical path is the peer key. Directory basename is the display/store
//! name. Live peers require a readable sibling schema; Self and Dead are
//! omitted from status presentation and pruned on scan.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Classification of a raw federated link path relative to the current repo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkClass {
    /// Path is the current repository root.
    Self_,
    /// Path exists and has a readable sibling schema (current or legacy path).
    Live,
    /// Missing path, or directory without a sibling schema.
    Dead,
}

/// One presented live peer after collapse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresentedLink {
    /// On-disk directory basename (not cached `sibling_name` or `schema.repo_name`).
    pub name: String,
    pub path: String,
    pub last_scanned: String,
}

/// Result of presenting raw federated links for status / impact.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PresentedLinks {
    pub live: Vec<PresentedLink>,
    /// Raw rows classified Self (or equivalent self-path extras).
    pub omitted_self: usize,
    /// Raw rows classified Dead.
    pub omitted_dead: usize,
    /// Extra raw rows dropped by same-path collapse (beyond the kept Live).
    pub omitted_dup_extra: usize,
}

impl PresentedLinks {
    pub fn omitted_total(&self) -> usize {
        self.omitted_self + self.omitted_dead + self.omitted_dup_extra
    }
}

/// Resolve sibling schema path (current layout, then legacy). Shared with impact.
pub fn resolve_sibling_schema(path: &str) -> Option<PathBuf> {
    let base = Path::new(path);
    let current = base.join(".ledgerful").join("state").join("schema.json");
    if current.exists() {
        return Some(current);
    }
    let legacy = base.join(".ledgerful").join("schema.json");
    if legacy.exists() {
        return Some(legacy);
    }
    None
}

/// Canonical comparison key for a federated path.
///
/// When the path exists, uses filesystem canonicalize. Otherwise slash-normalizes.
/// On Windows, comparison keys are lowercased (NTFS case-insensitive).
pub fn canonical_link_key(path: &str) -> String {
    let p = Path::new(path);
    let key = if p.exists() {
        match std::fs::canonicalize(p) {
            Ok(c) => path_key_string(&c),
            Err(_) => normalize_path_key(path),
        }
    } else {
        normalize_path_key(path)
    };
    #[cfg(windows)]
    {
        key.to_lowercase()
    }
    #[cfg(not(windows))]
    {
        key
    }
}

fn path_key_string(path: &Path) -> String {
    // Strip Windows UNC long-path prefix from canonicalize for stable compare.
    let s = path.to_string_lossy();
    let stripped = s
        .strip_prefix(r"\\?\")
        .or_else(|| s.strip_prefix("//?/"))
        .unwrap_or(&s);
    normalize_path_key(stripped)
}

fn normalize_path_key(path: &str) -> String {
    path.replace('\\', "/").trim_end_matches('/').to_string()
}

/// Directory basename of a path for store/display name.
pub fn path_basename(path: &str) -> String {
    let trimmed = path.trim_end_matches(['/', '\\']);
    Path::new(trimmed)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| trimmed.to_string())
}

/// Classify a link path relative to `repo_root`.
pub fn classify_link(path: &str, repo_root: &str) -> LinkClass {
    let path_key = canonical_link_key(path);
    let root_key = canonical_link_key(repo_root);
    if path_key == root_key {
        return LinkClass::Self_;
    }
    if resolve_sibling_schema(path).is_some() {
        LinkClass::Live
    } else {
        LinkClass::Dead
    }
}

/// Collapse raw `(name, path, last_scanned)` rows to Live peers by path key.
///
/// Display name is the directory basename of the live path. When multiple Live
/// rows share a key, keeps `max(last_scanned)` (lexicographic RFC3339).
pub fn present_federated_links(
    raw: &[(String, String, String)],
    repo_root: &str,
) -> PresentedLinks {
    let mut out = PresentedLinks::default();
    // key -> (basename, path, last_scanned, raw_count)
    let mut live_by_key: BTreeMap<String, (String, String, String, usize)> = BTreeMap::new();

    for (_name, path, last_scanned) in raw {
        match classify_link(path, repo_root) {
            LinkClass::Self_ => {
                out.omitted_self += 1;
            }
            LinkClass::Dead => {
                out.omitted_dead += 1;
            }
            LinkClass::Live => {
                let key = canonical_link_key(path);
                let basename = path_basename(path);
                match live_by_key.get_mut(&key) {
                    Some((_n, _p, prev_scan, count)) => {
                        *count += 1;
                        if last_scanned.as_str() > prev_scan.as_str() {
                            *prev_scan = last_scanned.clone();
                            *_n = basename;
                            *_p = path.clone();
                        }
                    }
                    None => {
                        live_by_key.insert(key, (basename, path.clone(), last_scanned.clone(), 1));
                    }
                }
            }
        }
    }

    for (_key, (name, path, last_scanned, count)) in live_by_key {
        if count > 1 {
            out.omitted_dup_extra += count - 1;
        }
        out.live.push(PresentedLink {
            name,
            path,
            last_scanned,
        });
    }

    // Deterministic order by name then path
    out.live
        .sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.path.cmp(&b.path)));
    out
}

/// Honesty line when status omits raw cache rows.
pub fn omitted_honesty_message(omitted: usize) -> String {
    format!(
        "Omitted {omitted} invalid, duplicate, or self-referential federated link(s). Run 'ledgerful federate scan' to prune."
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn write_schema(dir: &Path) {
        let state = dir.join(".ledgerful").join("state");
        fs::create_dir_all(&state).unwrap();
        fs::write(state.join("schema.json"), r#"{"schema_version":"1.1"}"#).unwrap();
    }

    #[test]
    fn classify_live_with_schema() {
        let dir = tempdir().unwrap();
        write_schema(dir.path());
        let other = tempdir().unwrap();
        let class = classify_link(dir.path().to_str().unwrap(), other.path().to_str().unwrap());
        assert_eq!(class, LinkClass::Live);
    }

    #[test]
    fn classify_dead_missing_path() {
        let class = classify_link(r"C:\dev\does-not-exist-0184-xyz", r"C:\dev\ledgerful");
        assert_eq!(class, LinkClass::Dead);
    }

    #[test]
    fn classify_dead_dir_without_schema() {
        let dir = tempdir().unwrap();
        let other = tempdir().unwrap();
        fs::write(dir.path().join("CLAUDE.md"), "x").unwrap();
        let class = classify_link(dir.path().to_str().unwrap(), other.path().to_str().unwrap());
        assert_eq!(class, LinkClass::Dead);
    }

    #[test]
    fn classify_self() {
        let dir = tempdir().unwrap();
        write_schema(dir.path());
        let p = dir.path().to_str().unwrap();
        assert_eq!(classify_link(p, p), LinkClass::Self_);
    }

    #[test]
    fn collapse_same_path_different_names_to_basename() {
        let peer = tempdir().unwrap();
        write_schema(peer.path());
        let root = tempdir().unwrap();
        let peer_path = peer.path().to_str().unwrap().to_string();
        let basename = path_basename(&peer_path);
        let raw = vec![
            (
                "AI-Brains".into(),
                peer_path.clone(),
                "2026-07-04T00:00:00Z".into(),
            ),
            (
                "ai-brains".into(),
                peer_path.clone(),
                "2026-08-12T00:00:00Z".into(),
            ),
        ];
        let presented = present_federated_links(&raw, root.path().to_str().unwrap());
        assert_eq!(presented.live.len(), 1);
        assert_eq!(presented.live[0].name, basename);
        assert_eq!(presented.live[0].last_scanned, "2026-08-12T00:00:00Z");
        assert_eq!(presented.omitted_dup_extra, 1);
        assert_eq!(presented.omitted_total(), 1);
    }

    #[test]
    fn display_name_is_basename_not_cached_or_schema() {
        let peer = tempdir().unwrap();
        // Folder basename is temp dir name; cached name is changeguard
        write_schema(peer.path());
        let root = tempdir().unwrap();
        let peer_path = peer.path().to_str().unwrap().to_string();
        let raw = vec![(
            "changeguard".into(),
            peer_path.clone(),
            "2026-08-12T00:00:00Z".into(),
        )];
        let presented = present_federated_links(&raw, root.path().to_str().unwrap());
        assert_eq!(presented.live.len(), 1);
        assert_eq!(presented.live[0].name, path_basename(&peer_path));
        assert_ne!(presented.live[0].name, "changeguard");
    }

    #[test]
    fn husk_and_self_omitted() {
        let root = tempdir().unwrap();
        write_schema(root.path());
        let husk = tempdir().unwrap();
        fs::write(husk.path().join("only.md"), "x").unwrap();
        let root_s = root.path().to_str().unwrap().to_string();
        let husk_s = husk.path().to_str().unwrap().to_string();
        let raw = vec![
            ("self".into(), root_s.clone(), "2026-01-01T00:00:00Z".into()),
            ("husk".into(), husk_s, "2026-01-02T00:00:00Z".into()),
        ];
        let presented = present_federated_links(&raw, &root_s);
        assert!(presented.live.is_empty());
        assert_eq!(presented.omitted_self, 1);
        assert_eq!(presented.omitted_dead, 1);
        assert_eq!(presented.omitted_total(), 2);
    }

    #[test]
    fn windows_path_key_case_insensitive() {
        let a = canonical_link_key(r"C:\Dev\AI-Brains");
        let b = canonical_link_key(r"c:\dev\ai-brains");
        // Without existence, both normalize + LOWER on Windows
        #[cfg(windows)]
        assert_eq!(a, b);
        #[cfg(not(windows))]
        {
            let _ = (a, b);
        }
    }

    #[test]
    fn legacy_schema_is_live() {
        let dir = tempdir().unwrap();
        let legacy = dir.path().join(".ledgerful");
        fs::create_dir_all(&legacy).unwrap();
        fs::write(legacy.join("schema.json"), "{}").unwrap();
        let other = tempdir().unwrap();
        assert_eq!(
            classify_link(dir.path().to_str().unwrap(), other.path().to_str().unwrap()),
            LinkClass::Live
        );
    }

    #[test]
    fn honesty_message_suggests_scan() {
        let msg = omitted_honesty_message(2);
        assert!(msg.contains("Omitted 2"));
        assert!(msg.contains("federate scan"));
    }

    #[test]
    fn path_basename_trims_trailing_separators() {
        assert_eq!(path_basename(r"C:\dev\ledgerful\"), "ledgerful");
        assert_eq!(path_basename("C:/dev/ledgerful/"), "ledgerful");
    }
}
