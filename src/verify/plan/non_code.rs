use crate::impact::packet::ImpactPacket;
use std::path::Path;

/// Exact cheap paths (normalized `/`, case-sensitive).
const CHEAP_EXACT: &[&str] = &[
    "CHANGELOG.md",
    "README.md",
    "LICENSE",
    "SECURITY.md",
    "AGENTS.md",
    "Agents.md",
    "Claude.md",
    "scripts/bump-manifests.ps1",
    "scripts/bump-manifests.sh",
];

const OPENAPI_JSON: &str = "docs/api/openapi.json";
const BUMP_PS1: &str = "scripts/bump-manifests.ps1";
const BUMP_SH: &str = "scripts/bump-manifests.sh";

/// Normalize a repo-relative path for cheap-glob matching (`\` → `/`, strip `./`).
pub(crate) fn normalize_cheap_path(path: &Path) -> String {
    let mut s = path.to_string_lossy().replace('\\', "/");
    while let Some(stripped) = s.strip_prefix("./") {
        s = stripped.to_string();
    }
    s
}

/// True when `path` is in the `--scope fast` NonCodeCheap glob set.
///
/// `docs/api/openapi.json` is excluded. `.agents/**` is not cheap (skill edits
/// are not docs-cheap; watch-ignore also drops them before the classifier).
pub(crate) fn is_non_code_cheap_path(path: &Path) -> bool {
    let norm = normalize_cheap_path(path);
    if CHEAP_EXACT.contains(&norm.as_str()) {
        return true;
    }
    if norm == OPENAPI_JSON {
        return false;
    }
    if norm == "docs" || norm.starts_with("docs/") {
        return true;
    }
    if norm == "packaging" || norm.starts_with("packaging/") {
        return true;
    }
    false
}

/// True when every changed path is cheap. Empty change sets are EmptyChanges,
/// not NonCodeCheap.
pub(crate) fn all_non_code_cheap(packet: &ImpactPacket) -> bool {
    !packet.changes.is_empty()
        && packet
            .changes
            .iter()
            .all(|c| is_non_code_cheap_path(&c.path))
}

/// Packaging templates or the bump-manifests scripts (A2 inject trigger).
pub(crate) fn is_packaging_or_bump_script(path: &Path) -> bool {
    let norm = normalize_cheap_path(path);
    norm == "packaging" || norm.starts_with("packaging/") || norm == BUMP_PS1 || norm == BUMP_SH
}

/// True when any classified path should union stem `bump_manifests`.
pub(crate) fn any_packaging_or_bump_script(packet: &ImpactPacket) -> bool {
    packet
        .changes
        .iter()
        .any(|c| is_packaging_or_bump_script(&c.path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::impact::packet::{ChangedFile, ImpactPacket};
    use std::path::PathBuf;

    fn packet(paths: &[&str]) -> ImpactPacket {
        ImpactPacket {
            changes: paths
                .iter()
                .map(|p| ChangedFile {
                    path: PathBuf::from(p),
                    ..Default::default()
                })
                .collect(),
            ..ImpactPacket::default()
        }
    }

    #[test]
    fn changelog_and_docs_are_cheap() {
        assert!(is_non_code_cheap_path(Path::new("CHANGELOG.md")));
        assert!(is_non_code_cheap_path(Path::new("docs/installation.md")));
        assert!(is_non_code_cheap_path(Path::new(
            "docs\\package-distribution.md"
        )));
        assert!(all_non_code_cheap(&packet(&[
            "CHANGELOG.md",
            "docs/installation.md"
        ])));
    }

    #[test]
    fn openapi_json_is_not_cheap() {
        assert!(!is_non_code_cheap_path(Path::new("docs/api/openapi.json")));
        assert!(!all_non_code_cheap(&packet(&["docs/api/openapi.json"])));
    }

    #[test]
    fn bump_scripts_are_cheap() {
        assert!(is_non_code_cheap_path(Path::new(
            "scripts/bump-manifests.ps1"
        )));
        assert!(is_non_code_cheap_path(Path::new(
            "scripts/bump-manifests.sh"
        )));
        assert!(is_packaging_or_bump_script(Path::new(
            "scripts/bump-manifests.ps1"
        )));
        assert!(is_packaging_or_bump_script(Path::new(
            "scripts/bump-manifests.sh"
        )));
    }

    #[test]
    fn agents_skill_md_is_not_cheap() {
        assert!(!is_non_code_cheap_path(Path::new(
            ".agents/skills/ledgerful/SKILL.md"
        )));
        assert!(!is_non_code_cheap_path(Path::new(".agents/skills/x.md")));
    }

    #[test]
    fn packaging_is_cheap_and_inject_trigger() {
        assert!(is_non_code_cheap_path(Path::new(
            "packaging/homebrew/ledgerful.rb"
        )));
        assert!(is_packaging_or_bump_script(Path::new(
            "packaging/homebrew/ledgerful.rb"
        )));
        assert!(!is_packaging_or_bump_script(Path::new("CHANGELOG.md")));
    }

    #[test]
    fn empty_packet_is_not_non_code_cheap() {
        assert!(!all_non_code_cheap(&ImpactPacket::default()));
    }

    #[test]
    fn src_plus_changelog_is_not_all_cheap() {
        assert!(!all_non_code_cheap(&packet(&[
            "src/foo.rs",
            "CHANGELOG.md"
        ])));
    }
}
