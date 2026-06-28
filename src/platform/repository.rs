use std::collections::BTreeSet;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodePackageManager {
    Npm,
    Pnpm,
    Yarn,
    Bun,
    Ambiguous,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeProfile {
    pub package_manager: NodePackageManager,
    pub scripts: BTreeSet<String>,
    pub workspaces_declared: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DenoProfile {
    pub config_path: String,
    pub tasks: BTreeSet<String>,
    pub workspaces_declared: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RustProfile {
    pub is_virtual_workspace: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RepositoryProfile {
    pub rust: Option<RustProfile>,
    pub node: Option<NodeProfile>,
    pub deno: Option<DenoProfile>,
    pub evidence: Vec<DetectionEvidence>,
    pub warnings: Vec<DetectionWarning>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum DetectionEvidence {
    FoundCargoToml,
    FoundDenoJson,
    FoundDenoJsonc,
    FoundLockfile(String),
    FoundPackageJson,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum DetectionWarning {
    AmbiguousDenoConfig,
    AmbiguousLockfiles(String),
    ConflictingPackageManager(String),
    DenoWorkspaceWithoutRootTasks,
    MalformedManifest(String),
    NodeWorkspaceWithoutRootScripts,
    UnreadableManifest(String),
}

use serde_json::Value;
use std::fs;

fn parse_cargo(root: &Path, profile: &mut RepositoryProfile) {
    let cargo_path = root.join("Cargo.toml");
    if !cargo_path.exists() {
        return;
    }
    profile.evidence.push(DetectionEvidence::FoundCargoToml);
    let content = match fs::read_to_string(&cargo_path) {
        Ok(c) => c,
        Err(e) => {
            profile
                .warnings
                .push(DetectionWarning::UnreadableManifest(format!(
                    "Cargo.toml: {}",
                    e
                )));
            return;
        }
    };
    let parsed: Result<toml::Value, _> = toml::from_str(&content);
    match parsed {
        Ok(val) => {
            let is_virtual_workspace =
                val.get("package").is_none() && val.get("workspace").is_some();
            profile.rust = Some(RustProfile {
                is_virtual_workspace,
            });
        }
        Err(e) => {
            profile
                .warnings
                .push(DetectionWarning::MalformedManifest(format!(
                    "Cargo.toml: {}",
                    e
                )));
        }
    }
}

fn parse_node(root: &Path, profile: &mut RepositoryProfile) {
    let pkg_path = root.join("package.json");
    if !pkg_path.exists() {
        return;
    }
    profile.evidence.push(DetectionEvidence::FoundPackageJson);
    let content = match fs::read_to_string(&pkg_path) {
        Ok(c) => c,
        Err(e) => {
            profile
                .warnings
                .push(DetectionWarning::UnreadableManifest(format!(
                    "package.json: {}",
                    e
                )));
            return;
        }
    };

    let parsed: Result<Value, _> = serde_json::from_str(&content);
    let val = match parsed {
        Ok(val) => val,
        Err(e) => {
            profile
                .warnings
                .push(DetectionWarning::MalformedManifest(format!(
                    "package.json: {}",
                    e
                )));
            return;
        }
    };

    let mut scripts = BTreeSet::new();
    if let Some(s) = val.get("scripts").and_then(|v| v.as_object()) {
        for (k, v) in s.iter() {
            if v.is_string() && !v.as_str().unwrap().trim().is_empty() {
                scripts.insert(k.clone());
            }
        }
    }

    let workspaces_declared = val.get("workspaces").is_some();

    // Package manager
    let mut declared_pm = None;
    if let Some(pm) = val.get("packageManager").and_then(|v| v.as_str()) {
        let name = pm.split('@').next().unwrap_or("").trim();
        declared_pm = match name {
            "npm" => Some(NodePackageManager::Npm),
            "pnpm" => Some(NodePackageManager::Pnpm),
            "yarn" => Some(NodePackageManager::Yarn),
            "bun" => Some(NodePackageManager::Bun),
            _ => None,
        };
    }

    let mut lockfile_pm = Vec::new();
    let lockfiles = [
        ("npm-shrinkwrap.json", NodePackageManager::Npm),
        ("package-lock.json", NodePackageManager::Npm),
        ("pnpm-lock.yaml", NodePackageManager::Pnpm),
        ("pnpm-workspace.yaml", NodePackageManager::Pnpm),
        ("yarn.lock", NodePackageManager::Yarn),
        ("bun.lock", NodePackageManager::Bun),
        ("bun.lockb", NodePackageManager::Bun),
    ];
    let mut found_locks = Vec::new();

    for (name, pm) in lockfiles {
        if root.join(name).exists() {
            profile
                .evidence
                .push(DetectionEvidence::FoundLockfile(name.to_string()));
            found_locks.push(name.to_string());
            if !lockfile_pm.contains(&pm) {
                lockfile_pm.push(pm);
            }
        }
    }

    let final_pm = if let Some(pm) = declared_pm {
        if !lockfile_pm.is_empty() && !lockfile_pm.contains(&pm) {
            profile
                .warnings
                .push(DetectionWarning::ConflictingPackageManager(format!(
                    "declared {:?} vs lockfiles",
                    pm
                )));
        }
        pm
    } else {
        if lockfile_pm.len() == 1 {
            lockfile_pm[0].clone()
        } else if lockfile_pm.len() > 1 {
            found_locks.sort();
            profile
                .warnings
                .push(DetectionWarning::AmbiguousLockfiles(found_locks.join(", ")));
            NodePackageManager::Ambiguous
        } else {
            NodePackageManager::Npm
        }
    };

    if workspaces_declared && scripts.is_empty() {
        profile
            .warnings
            .push(DetectionWarning::NodeWorkspaceWithoutRootScripts);
    }

    profile.node = Some(NodeProfile {
        package_manager: final_pm,
        scripts,
        workspaces_declared,
    });
}

fn parse_deno(root: &Path, profile: &mut RepositoryProfile) {
    let mut deno_paths = Vec::new();
    let json_path = root.join("deno.json");
    let jsonc_path = root.join("deno.jsonc");

    if json_path.exists() {
        deno_paths.push(("deno.json", json_path));
    }
    if jsonc_path.exists() {
        deno_paths.push(("deno.jsonc", jsonc_path));
    }

    if deno_paths.is_empty() {
        return;
    }

    if deno_paths.len() > 1 {
        profile.warnings.push(DetectionWarning::AmbiguousDenoConfig);
    }

    let (name, path) = &deno_paths[0];

    if *name == "deno.json" {
        profile.evidence.push(DetectionEvidence::FoundDenoJson);
    } else {
        profile.evidence.push(DetectionEvidence::FoundDenoJsonc);
    }

    let content = match fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => {
            profile
                .warnings
                .push(DetectionWarning::UnreadableManifest(format!(
                    "{}: {}",
                    name, e
                )));
            return;
        }
    };

    let parsed_val = if *name == "deno.jsonc" {
        match jsonc_parser::parse_to_serde_value(&content, &jsonc_parser::ParseOptions::default()) {
            Ok(Some(v)) => v,
            Ok(None) => Value::Null,
            Err(e) => {
                profile
                    .warnings
                    .push(DetectionWarning::MalformedManifest(format!(
                        "{}: {}",
                        name, e
                    )));
                return;
            }
        }
    } else {
        match serde_json::from_str(&content) {
            Ok(v) => v,
            Err(e) => {
                profile
                    .warnings
                    .push(DetectionWarning::MalformedManifest(format!(
                        "{}: {}",
                        name, e
                    )));
                return;
            }
        }
    };

    let mut tasks = BTreeSet::new();
    if let Some(t) = parsed_val.get("tasks").and_then(|v| v.as_object()) {
        for (k, _) in t {
            tasks.insert(k.clone());
        }
    }

    let workspaces_declared = parsed_val.get("workspace").is_some();

    if workspaces_declared && tasks.is_empty() {
        profile
            .warnings
            .push(DetectionWarning::DenoWorkspaceWithoutRootTasks);
    }

    profile.deno = Some(DenoProfile {
        config_path: name.to_string(),
        tasks,
        workspaces_declared,
    });
}

pub fn detect_repository(root: &Path) -> RepositoryProfile {
    let mut profile = RepositoryProfile {
        rust: None,
        node: None,
        deno: None,
        evidence: Vec::new(),
        warnings: Vec::new(),
    };

    parse_cargo(root, &mut profile);
    parse_node(root, &mut profile);
    parse_deno(root, &mut profile);

    profile.evidence.sort();
    profile.warnings.sort();
    profile
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn detect_neutral() {
        let dir = tempdir().unwrap();
        let profile = detect_repository(dir.path());
        assert_eq!(profile.rust, None);
        assert_eq!(profile.node, None);
        assert_eq!(profile.deno, None);
        assert!(profile.evidence.is_empty());
        assert!(profile.warnings.is_empty());
    }

    #[test]
    fn detect_cargo_package() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("Cargo.toml"), "[package]\nname=\"foo\"").unwrap();
        let profile = detect_repository(dir.path());
        assert!(profile.rust.is_some());
        assert!(!profile.rust.unwrap().is_virtual_workspace);
        assert_eq!(profile.evidence, vec![DetectionEvidence::FoundCargoToml]);
    }

    #[test]
    fn detect_cargo_workspace() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("Cargo.toml"),
            "[workspace]\nmembers=[\"foo\"]",
        )
        .unwrap();
        let profile = detect_repository(dir.path());
        assert!(profile.rust.is_some());
        assert!(profile.rust.unwrap().is_virtual_workspace);
        assert_eq!(profile.evidence, vec![DetectionEvidence::FoundCargoToml]);
    }

    #[test]
    fn detect_npm_implicit() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("package.json"), "{}").unwrap();
        fs::write(dir.path().join("package-lock.json"), "").unwrap();
        let profile = detect_repository(dir.path());
        let node = profile.node.expect("Node profile expected");
        assert_eq!(node.package_manager, NodePackageManager::Npm);
        assert_eq!(
            profile.evidence,
            vec![
                DetectionEvidence::FoundLockfile("package-lock.json".to_string()),
                DetectionEvidence::FoundPackageJson,
            ]
        );
    }

    #[test]
    fn detect_pnpm_implicit() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("package.json"), "{}").unwrap();
        fs::write(dir.path().join("pnpm-lock.yaml"), "").unwrap();
        let profile = detect_repository(dir.path());
        let node = profile.node.expect("Node profile expected");
        assert_eq!(node.package_manager, NodePackageManager::Pnpm);
    }

    #[test]
    fn detect_yarn_implicit() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("package.json"), "{}").unwrap();
        fs::write(dir.path().join("yarn.lock"), "").unwrap();
        let profile = detect_repository(dir.path());
        let node = profile.node.expect("Node profile expected");
        assert_eq!(node.package_manager, NodePackageManager::Yarn);
    }

    #[test]
    fn detect_bun_current() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("package.json"), "{}").unwrap();
        fs::write(dir.path().join("bun.lock"), "").unwrap();
        let profile = detect_repository(dir.path());
        let node = profile.node.expect("Node profile expected");
        assert_eq!(node.package_manager, NodePackageManager::Bun);
    }

    #[test]
    fn detect_bun_legacy() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("package.json"), "{}").unwrap();
        fs::write(dir.path().join("bun.lockb"), "").unwrap();
        let profile = detect_repository(dir.path());
        let node = profile.node.expect("Node profile expected");
        assert_eq!(node.package_manager, NodePackageManager::Bun);
    }

    #[test]
    fn detect_package_manager_injection() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("package.json"),
            r#"{"packageManager": "npm; rm -rf /"}"#,
        )
        .unwrap();
        let profile = detect_repository(dir.path());
        let node = profile.node.expect("Node profile expected");
        assert_eq!(node.package_manager, NodePackageManager::Npm);
    }

    #[test]
    fn detect_ambiguous_lockfiles() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("package.json"), "{}").unwrap();
        fs::write(dir.path().join("package-lock.json"), "").unwrap();
        fs::write(dir.path().join("yarn.lock"), "").unwrap();
        let profile = detect_repository(dir.path());
        let node = profile.node.expect("Node profile expected");
        assert_eq!(node.package_manager, NodePackageManager::Ambiguous);
        assert_eq!(
            profile.warnings,
            vec![DetectionWarning::AmbiguousLockfiles(
                "package-lock.json, yarn.lock".to_string()
            )]
        );
    }

    #[test]
    fn detect_mixed_repo() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("package.json"), "{}").unwrap();
        fs::write(dir.path().join("Cargo.toml"), "[package]").unwrap();
        fs::write(dir.path().join("deno.json"), "{}").unwrap();
        let profile = detect_repository(dir.path());
        assert!(profile.rust.is_some());
        assert!(profile.node.is_some());
        assert!(profile.deno.is_some());
    }

    #[test]
    fn detect_deno_jsonc() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("deno.jsonc"),
            "{ // comments\n\"tasks\": {\"build\": \"echo build\"} }",
        )
        .unwrap();
        let profile = detect_repository(dir.path());
        let deno = profile.deno.expect("Deno profile expected");
        assert!(deno.tasks.contains("build"));
    }

    #[test]
    fn detect_node_workspace_no_scripts() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("package.json"),
            r#"{"workspaces": ["packages/*"]}"#,
        )
        .unwrap();
        let profile = detect_repository(dir.path());
        assert_eq!(
            profile.warnings,
            vec![DetectionWarning::NodeWorkspaceWithoutRootScripts]
        );
    }

    #[test]
    fn detect_deno_workspace_no_tasks() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("deno.json"),
            r#"{"workspace": ["packages"]}"#,
        )
        .unwrap();
        let profile = detect_repository(dir.path());
        assert_eq!(
            profile.warnings,
            vec![DetectionWarning::DenoWorkspaceWithoutRootTasks]
        );
    }
}
