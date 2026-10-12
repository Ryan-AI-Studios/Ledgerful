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

/// Cheap repo-relative paths compiled into tests via `include_str!`, mapped
/// to nextest `test()` stems (function names). Sorted by path. Each stem
/// slice is non-empty and sorted.
///
/// Do **not** use the dedicated `[[test]]` binary name `install_docs_truth`
/// as a `test()` stem — it matches zero tests. Phase 0 locked these live
/// function names instead of emitting `binary(...)`.
pub(crate) const INCLUDE_STR_MATERIAL: &[(&str, &[&str])] = &[
    (
        "README.md",
        &[
            "no_stale_package_manager_availability_language",
            "readme_has_daily_5_front_door",
        ],
    ),
    (
        "SECURITY.md",
        &["no_stale_package_manager_availability_language"],
    ),
    (
        "docs/Call-Resolution.md",
        &["call_resolution_docs_honesty_ceiling"],
    ),
    (
        "docs/Ledgerful/skill.md",
        &[
            "call_resolution_docs_honesty_ceiling",
            "test_daily5_skill_sot_contains_five_argv_substrings",
            "test_daily5_skill_sot_is_ai_only_procedure",
        ],
    ),
    (
        "docs/agent-output-contract.md",
        &[
            "audit_pipeline_stage_slugs__appear_in_agent_output_contract",
            "stage_slug_for_provider_pins_locked_tokens",
        ],
    ),
    (
        "docs/golden-path.md",
        &["golden_path_doc_mirrors_canonical_argv"],
    ),
    (
        "docs/installation.md",
        &[
            "installation_md_has_live_brew_and_scoop_commands",
            "no_stale_package_manager_availability_language",
        ],
    ),
    (
        "docs/package-distribution.md",
        &["no_stale_package_manager_availability_language"],
    ),
];

/// Nextest `test()` stems for a cheap `include_str!` path, or empty.
pub(crate) fn include_str_material_stems_for(path: &Path) -> &'static [&'static str] {
    let norm = normalize_cheap_path(path);
    INCLUDE_STR_MATERIAL
        .iter()
        .find(|(p, _)| *p == norm)
        .map(|(_, stems)| *stems)
        .unwrap_or(&[])
}

/// True when any classified path is a cheap `include_str!` material path.
pub(crate) fn any_include_str_material(packet: &ImpactPacket) -> bool {
    packet
        .changes
        .iter()
        .any(|c| !include_str_material_stems_for(&c.path).is_empty())
}

/// Union every `include_str!` stem for classified paths into `stems`.
/// Returns true when at least one new stem was added. Sorts and dedups.
pub(crate) fn union_include_str_material_stems(
    packet: &ImpactPacket,
    stems: &mut Vec<String>,
) -> bool {
    let mut added = false;
    for change in &packet.changes {
        for stem in include_str_material_stems_for(&change.path) {
            if !stems.iter().any(|s| s == stem) {
                stems.push((*stem).to_string());
                added = true;
            }
        }
    }
    if added {
        stems.sort();
        stems.dedup();
    }
    added
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
    fn include_str_material_table_is_sorted_with_nonempty_stems() {
        let mut prev: Option<&str> = None;
        for (path, stems) in INCLUDE_STR_MATERIAL {
            if let Some(p) = prev {
                assert!(
                    p < *path,
                    "INCLUDE_STR_MATERIAL paths must be sorted: {p} before {path}"
                );
            }
            assert!(
                !stems.is_empty(),
                "INCLUDE_STR_MATERIAL[{path}] must have stems"
            );
            let mut stem_prev: Option<&str> = None;
            for stem in *stems {
                assert!(
                    *stem != "install_docs_truth",
                    "vacuous test(install_docs_truth) is forbidden"
                );
                if let Some(s) = stem_prev {
                    assert!(
                        s < *stem,
                        "stems for {path} must be sorted: {s} before {stem}"
                    );
                }
                stem_prev = Some(stem);
            }
            prev = Some(path);
        }
    }

    #[test]
    fn include_str_material_stems_union_installation_and_readme() {
        let mixed = packet(&["docs/installation.md", "README.md"]);
        assert!(any_include_str_material(&mixed));
        let mut stems = Vec::new();
        assert!(union_include_str_material_stems(&mixed, &mut stems));
        assert_eq!(
            stems,
            vec![
                "installation_md_has_live_brew_and_scoop_commands".to_string(),
                "no_stale_package_manager_availability_language".to_string(),
                "readme_has_daily_5_front_door".to_string(),
            ]
        );
        assert!(include_str_material_stems_for(Path::new("docs/architecture.md")).is_empty());
        assert!(!any_include_str_material(&packet(&["CHANGELOG.md"])));
    }

    #[test]
    fn src_plus_changelog_is_not_all_cheap() {
        assert!(!all_non_code_cheap(&packet(&[
            "src/foo.rs",
            "CHANGELOG.md"
        ])));
    }
}
