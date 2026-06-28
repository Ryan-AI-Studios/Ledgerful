use crate::config::model::{VerifyConfig, VerifyStep};
use crate::platform::repository::{NodePackageManager, RepositoryProfile};

pub fn build_auto_policy(
    profile: &RepositoryProfile,
    config: &VerifyConfig,
    repo_root: &std::path::Path,
    scope: crate::verify::plan::VerifyScope,
) -> Vec<VerifyStep> {
    let mut steps = Vec::new();

    // 0. Neutral checks
    steps.push(VerifyStep {
        description: "Check for whitespace errors in working tree".to_string(),
        command: "git diff --check".to_string(),
        timeout_secs: None,
    });
    steps.push(VerifyStep {
        description: "Check for whitespace errors in staging area".to_string(),
        command: "git diff --cached --check".to_string(),
        timeout_secs: None,
    });

    // 1. Rust
    if let Some(_rust) = &profile.rust {
        steps.push(VerifyStep {
            description: "Check formatting".to_string(),
            command: "cargo fmt --all -- --check".to_string(),
            timeout_secs: None,
        });
        steps.push(VerifyStep {
            description: "Lint".to_string(),
            command: "cargo clippy --all-targets --all-features -- -D warnings".to_string(),
            timeout_secs: None,
        });
        let use_nextest = match config.prefer_nextest {
            Some(false) => false,
            _ => crate::verify::engine::probe_nextest(),
        };
        if use_nextest {
            let nextest_config_content =
                std::fs::read_to_string(repo_root.join(".config/nextest.toml")).unwrap_or_default();

            let has_ci = if let Ok(parsed) = nextest_config_content.parse::<toml::Value>() {
                parsed.get("profile").and_then(|p| p.get("ci")).is_some()
            } else {
                false
            };

            let command = if has_ci {
                "cargo nextest run --workspace --all-features --profile ci".to_string()
            } else {
                "cargo nextest run --workspace --all-features".to_string()
            };

            steps.push(VerifyStep {
                description: "Test".to_string(),
                command,
                timeout_secs: None,
            });
        } else {
            steps.push(VerifyStep {
                description: "Test".to_string(),
                command: "cargo test --workspace --all-features".to_string(),
                timeout_secs: None,
            });
        }

        // Doctests are only included in full scope (slow/expensive).
        if scope == crate::verify::plan::VerifyScope::Full {
            steps.push(VerifyStep {
                description: "Doc tests".to_string(),
                command: "cargo test --workspace --all-features --doc".to_string(),
                timeout_secs: None,
            });
        }
    }

    // 2. Node
    if let Some(node) = &profile.node {
        // Ambiguous lockfiles: emit only neutral checks, no package manager commands
        if node.package_manager != NodePackageManager::Ambiguous {
            let runner = match node.package_manager {
                NodePackageManager::Npm => "npm run",
                NodePackageManager::Pnpm => "pnpm run",
                NodePackageManager::Yarn => "yarn run",
                NodePackageManager::Bun => "bun run",
                NodePackageManager::Ambiguous => "npm run", // never reached: guarded above
            };

            // Prefer predefined scripts in order: lint, test:ci, build
            let try_add = |target: &str, steps: &mut Vec<VerifyStep>| {
                if node.scripts.contains(target) {
                    steps.push(VerifyStep {
                        description: format!("Run {}", target),
                        command: format!("{} {}", runner, target),
                        timeout_secs: None,
                    });
                    return true;
                }
                false
            };

            try_add("lint", &mut steps);
            try_add("test:ci", &mut steps);
            try_add("build", &mut steps);
        }
    }

    // 3. Deno
    if let Some(deno) = &profile.deno {
        // Format
        if deno.tasks.contains("fmt") {
            steps.push(VerifyStep {
                description: "Run fmt task".to_string(),
                command: "deno task fmt".to_string(),
                timeout_secs: None,
            });
        } else if !deno.workspaces_declared {
            steps.push(VerifyStep {
                description: "Check formatting".to_string(),
                command: "deno fmt --check".to_string(),
                timeout_secs: None,
            });
        }

        // Lint
        if deno.tasks.contains("lint") {
            steps.push(VerifyStep {
                description: "Run lint task".to_string(),
                command: "deno task lint".to_string(),
                timeout_secs: None,
            });
        } else if !deno.workspaces_declared {
            steps.push(VerifyStep {
                description: "Lint".to_string(),
                command: "deno lint".to_string(),
                timeout_secs: None,
            });
        }

        // Test
        if deno.tasks.contains("test:ci") {
            steps.push(VerifyStep {
                description: "Run test:ci task".to_string(),
                command: "deno task test:ci".to_string(),
                timeout_secs: None,
            });
        } else if !deno.workspaces_declared {
            steps.push(VerifyStep {
                description: "Test".to_string(),
                command: "deno test --cached-only".to_string(),
                timeout_secs: None,
            });
        }

        // Build
        if deno.tasks.contains("build") {
            steps.push(VerifyStep {
                description: "Run build task".to_string(),
                command: "deno task build".to_string(),
                timeout_secs: None,
            });
        }
    }

    steps
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::{DenoProfile, NodeProfile, RustProfile};
    use std::collections::BTreeSet;

    fn empty_profile() -> RepositoryProfile {
        RepositoryProfile {
            rust: None,
            node: None,
            deno: None,
            evidence: vec![],
            warnings: vec![],
        }
    }

    fn default_config() -> VerifyConfig {
        VerifyConfig::default()
    }

    #[test]
    fn auto_policy_neutral_repo() {
        let profile = empty_profile();
        let plan = build_auto_policy(
            &profile,
            &default_config(),
            std::path::Path::new("."),
            crate::verify::plan::VerifyScope::Full,
        );
        assert_eq!(
            plan.len(),
            2,
            "Neutral repo should emit only git diff checks"
        );
        assert_eq!(plan[0].command, "git diff --check");
        assert_eq!(plan[1].command, "git diff --cached --check");
    }

    #[test]
    fn auto_policy_rust_repo() {
        let mut profile = empty_profile();
        profile.rust = Some(RustProfile {
            is_virtual_workspace: false,
        });

        // Use a temp directory so there is no .config/nextest.toml (no ci profile).
        let tmp = tempfile::tempdir().unwrap();
        let plan = build_auto_policy(
            &profile,
            &default_config(),
            tmp.path(),
            crate::verify::plan::VerifyScope::Full,
        );
        assert_eq!(plan.len(), 6);
        assert_eq!(plan[0].command, "git diff --check");
        assert_eq!(plan[1].command, "git diff --cached --check");
        assert_eq!(plan[2].command, "cargo fmt --all -- --check");
        assert_eq!(
            plan[3].command,
            "cargo clippy --all-targets --all-features -- -D warnings"
        );
        // H3: use_nextest is now determined by probe_nextest(); no nextest.toml in tmpdir.
        let expected_cmd = if crate::verify::engine::probe_nextest() {
            "cargo nextest run --workspace --all-features"
        } else {
            "cargo test --workspace --all-features"
        };
        assert_eq!(plan[4].command, expected_cmd);
        assert_eq!(
            plan[5].command,
            "cargo test --workspace --all-features --doc"
        );
    }

    #[test]
    fn auto_policy_rust_prefer_nextest_false() {
        let mut profile = empty_profile();
        profile.rust = Some(RustProfile {
            is_virtual_workspace: false,
        });

        let mut config = default_config();
        config.prefer_nextest = Some(false);

        let plan = build_auto_policy(
            &profile,
            &config,
            std::path::Path::new("."),
            crate::verify::plan::VerifyScope::Full,
        );
        assert_eq!(plan.len(), 6);
        assert_eq!(plan[2].command, "cargo fmt --all -- --check");
        assert_eq!(
            plan[3].command,
            "cargo clippy --all-targets --all-features -- -D warnings"
        );
        assert_eq!(plan[4].command, "cargo test --workspace --all-features");
        assert_eq!(
            plan[5].command,
            "cargo test --workspace --all-features --doc"
        );
    }

    #[test]
    fn auto_policy_node_repo() {
        let mut profile = empty_profile();
        let mut scripts = BTreeSet::new();
        scripts.insert("format".to_string()); // should be ignored
        scripts.insert("lint".to_string());
        scripts.insert("typecheck".to_string()); // should be ignored
        scripts.insert("test".to_string()); // should be ignored
        scripts.insert("test:ci".to_string());
        scripts.insert("build".to_string());

        profile.node = Some(NodeProfile {
            package_manager: NodePackageManager::Npm,
            scripts,
            workspaces_declared: false,
        });

        let plan = build_auto_policy(
            &profile,
            &default_config(),
            std::path::Path::new("."),
            crate::verify::plan::VerifyScope::Full,
        );
        assert_eq!(plan.len(), 5);
        // git checks (2) + lint, test:ci, build
        assert_eq!(plan[2].command, "npm run lint");
        assert_eq!(plan[3].command, "npm run test:ci");
        assert_eq!(plan[4].command, "npm run build");
    }

    #[test]
    fn auto_policy_node_yarn() {
        let mut profile = empty_profile();
        let mut scripts = BTreeSet::new();
        scripts.insert("build".to_string());

        profile.node = Some(NodeProfile {
            package_manager: NodePackageManager::Yarn,
            scripts,
            workspaces_declared: false,
        });

        let plan = build_auto_policy(
            &profile,
            &default_config(),
            std::path::Path::new("."),
            crate::verify::plan::VerifyScope::Full,
        );
        assert_eq!(plan.len(), 3);
        assert_eq!(plan[2].command, "yarn run build");
    }

    #[test]
    fn auto_policy_deno_repo() {
        let mut profile = empty_profile();
        let mut tasks = BTreeSet::new();
        tasks.insert("test:ci".to_string());

        profile.deno = Some(DenoProfile {
            config_path: "deno.json".to_string(),
            tasks,
            workspaces_declared: false,
        });

        let plan = build_auto_policy(
            &profile,
            &default_config(),
            std::path::Path::new("."),
            crate::verify::plan::VerifyScope::Full,
        );
        assert_eq!(plan.len(), 5); // git checks (2) + fmt builtin + lint builtin + test:ci task
        assert_eq!(plan[2].command, "deno fmt --check");
        assert_eq!(plan[3].command, "deno lint");
        assert_eq!(plan[4].command, "deno task test:ci");
    }

    #[test]
    fn auto_policy_deno_workspace() {
        let mut profile = empty_profile();
        let tasks = BTreeSet::new();

        profile.deno = Some(DenoProfile {
            config_path: "deno.json".to_string(),
            tasks,
            workspaces_declared: true,
        });

        let plan = build_auto_policy(
            &profile,
            &default_config(),
            std::path::Path::new("."),
            crate::verify::plan::VerifyScope::Full,
        );
        assert_eq!(
            plan.len(),
            2,
            "Deno workspace without root tasks should run only git diff checks (avoid unsafe built-ins)"
        );
    }

    #[test]
    fn auto_policy_mixed_repo() {
        let mut profile = empty_profile();
        profile.rust = Some(RustProfile {
            is_virtual_workspace: false,
        });
        let mut scripts = BTreeSet::new();
        scripts.insert("test:ci".to_string());
        profile.node = Some(NodeProfile {
            package_manager: NodePackageManager::Pnpm,
            scripts,
            workspaces_declared: false,
        });

        let plan = build_auto_policy(
            &profile,
            &default_config(),
            std::path::Path::new("."),
            crate::verify::plan::VerifyScope::Full,
        );
        // Rust tests and Node tests
        assert!(
            plan.iter()
                .any(|s| s.command == "cargo test --workspace --all-features --doc")
        );
        assert!(plan.iter().any(|s| s.command == "pnpm run test:ci"));
    }

    #[test]
    fn auto_policy_deterministic_rendering() {
        let mut profile = empty_profile();
        let mut scripts = BTreeSet::new();
        scripts.insert("lint".to_string());
        scripts.insert("test:ci".to_string());
        profile.node = Some(NodeProfile {
            package_manager: NodePackageManager::Npm,
            scripts,
            workspaces_declared: false,
        });

        let plan1 = build_auto_policy(
            &profile,
            &default_config(),
            std::path::Path::new("."),
            crate::verify::plan::VerifyScope::Full,
        );
        let plan2 = build_auto_policy(
            &profile,
            &default_config(),
            std::path::Path::new("."),
            crate::verify::plan::VerifyScope::Full,
        );

        assert_eq!(plan1.len(), plan2.len());
        for (a, b) in plan1.iter().zip(plan2.iter()) {
            assert_eq!(a.command, b.command);
            assert_eq!(a.description, b.description);
        }
    }

    #[test]
    fn auto_policy_node_ambiguous_produces_only_neutral() {
        let mut profile = empty_profile();
        let mut scripts = BTreeSet::new();
        scripts.insert("lint".to_string());
        scripts.insert("test:ci".to_string());
        profile.node = Some(NodeProfile {
            package_manager: NodePackageManager::Ambiguous,
            scripts,
            workspaces_declared: false,
        });

        let plan = build_auto_policy(
            &profile,
            &default_config(),
            std::path::Path::new("."),
            crate::verify::plan::VerifyScope::Full,
        );
        assert_eq!(
            plan.len(),
            2,
            "Ambiguous package manager should produce only neutral git checks"
        );
        assert_eq!(plan[0].command, "git diff --check");
        assert_eq!(plan[1].command, "git diff --cached --check");
    }

    #[test]
    fn auto_policy_node_bun_uses_run_syntax() {
        let mut profile = empty_profile();
        let mut scripts = BTreeSet::new();
        scripts.insert("build".to_string());
        profile.node = Some(NodeProfile {
            package_manager: NodePackageManager::Bun,
            scripts,
            workspaces_declared: false,
        });

        let plan = build_auto_policy(
            &profile,
            &default_config(),
            std::path::Path::new("."),
            crate::verify::plan::VerifyScope::Full,
        );
        assert_eq!(plan.len(), 3);
        assert_eq!(plan[2].command, "bun run build");
    }
}
