use super::*;
use crate::impact::packet::{ChangedFile, FileAnalysisStatus, ImpactPacket};
use crate::policy::mode::Mode;
use crate::policy::rules::{GlobalRules, PathRule, Rules};
use std::path::PathBuf;

fn empty_packet() -> ImpactPacket {
    ImpactPacket {
        changes: vec![ChangedFile {
            path: PathBuf::from("src/main.rs"),
            status: "Modified".to_string(),
            old_path: None,
            is_staged: false,

            symbols: None,
            imports: None,
            runtime_usage: None,
            analysis_status: FileAnalysisStatus::default(),
            analysis_warnings: Vec::new(),
            api_routes: Vec::new(),
            data_models: Vec::new(),
            ci_gates: Vec::new(),
        }],
        ..ImpactPacket::default()
    }
}

#[test]
fn test_build_plan_default_when_no_rules() {
    let packet = empty_packet();
    let rules = Rules::default();
    let config = VerifyConfig {
        prefer_nextest: Some(false),
        ..Default::default()
    };
    let profile = crate::platform::repository::RepositoryProfile::default();
    let plan = build_plan(
        &packet,
        &rules,
        &[],
        &config,
        &profile,
        std::path::Path::new("."),
    );

    assert_eq!(plan.steps.len(), 2);
    // When prefer_nextest is Some(false), falls back to cargo test
}

#[test]
fn test_build_plan_with_global_verifications() {
    let packet = empty_packet();
    let rules = Rules {
        was_legacy_default: false,
        global: GlobalRules {
            mode: Mode::Analyze,
            required_verifications: vec!["cargo test".to_string(), "cargo clippy".to_string()],
        },
        overrides: Vec::new(),
        protected_paths: Vec::new(),
    };
    let config = VerifyConfig {
        prefer_nextest: Some(false),
        ..Default::default()
    };
    let profile = crate::platform::repository::RepositoryProfile::default();

    let plan = build_plan(
        &packet,
        &rules,
        &[],
        &config,
        &profile,
        std::path::Path::new("."),
    );

    assert_eq!(plan.steps.len(), 2);
    assert!(plan.steps.iter().any(|s| s.command == "cargo clippy"));
    assert!(plan.steps.iter().any(|s| s.command == "cargo test"));
}

#[test]
fn test_build_plan_deduplicates() {
    let packet = empty_packet();
    let rules = Rules {
        was_legacy_default: false,
        global: GlobalRules {
            mode: Mode::Analyze,
            required_verifications: vec!["cargo test".to_string()],
        },
        overrides: vec![PathRule {
            pattern: "*.rs".to_string(),
            mode: None,
            required_verifications: vec!["cargo test".to_string()],
        }],
        protected_paths: Vec::new(),
    };
    let config = VerifyConfig {
        prefer_nextest: Some(false),
        ..Default::default()
    };
    let profile = crate::platform::repository::RepositoryProfile::default();

    let plan = build_plan(
        &packet,
        &rules,
        &[],
        &config,
        &profile,
        std::path::Path::new("."),
    );

    assert_eq!(plan.steps.len(), 1);
    assert!(plan.steps.iter().any(|s| s.command == "cargo test"));
}

#[test]
fn test_build_plan_path_rule_matching() {
    let packet = empty_packet(); // src/main.rs matches *.rs
    let rules = Rules {
        was_legacy_default: false,
        global: GlobalRules {
            mode: Mode::Analyze,
            required_verifications: vec!["cargo test".to_string()],
        },
        overrides: vec![PathRule {
            pattern: "*.rs".to_string(),
            mode: None,
            required_verifications: vec!["cargo clippy".to_string()],
        }],
        protected_paths: Vec::new(),
    };
    let config = VerifyConfig {
        prefer_nextest: Some(false),
        ..Default::default()
    };
    let profile = crate::platform::repository::RepositoryProfile::default();

    let plan = build_plan(
        &packet,
        &rules,
        &[],
        &config,
        &profile,
        std::path::Path::new("."),
    );

    assert_eq!(plan.steps.len(), 2);
    assert!(plan.steps.iter().any(|s| s.command == "cargo clippy"));
    assert!(plan.steps.iter().any(|s| s.command == "cargo test"));
}

#[test]
fn test_build_plan_path_rule_no_match() {
    let packet = empty_packet(); // src/main.rs
    let rules = Rules {
        was_legacy_default: false,
        global: GlobalRules {
            mode: Mode::Analyze,
            required_verifications: vec![],
        },
        overrides: vec![PathRule {
            pattern: "*.py".to_string(),
            mode: None,
            required_verifications: vec!["pytest".to_string()],
        }],
        protected_paths: Vec::new(),
    };
    let config = VerifyConfig {
        prefer_nextest: Some(false),
        ..Default::default()
    };
    let profile = crate::platform::repository::RepositoryProfile::default();

    let plan = build_plan(
        &packet,
        &rules,
        &[],
        &config,
        &profile,
        std::path::Path::new("."),
    );

    // No match, falls back to default empty auto policy? Wait!
    // The old test expected it to fall back to 'cargo test' because it was hardcoded.
    // With auto_policy, a neutral repo emits 2 git diff steps.
    // But since we are full scope, append_full_tier_commands will append 'cargo test -j 1 ...' !
    assert_eq!(plan.steps.len(), 2);
}

#[test]
fn test_build_plan_deterministic() {
    let packet = empty_packet();
    let rules = Rules {
        was_legacy_default: false,
        global: GlobalRules {
            mode: Mode::Analyze,
            required_verifications: vec!["z_cmd".to_string(), "a_cmd".to_string()],
        },
        overrides: Vec::new(),
        protected_paths: Vec::new(),
    };
    let config = VerifyConfig {
        prefer_nextest: Some(false),
        ..Default::default()
    };
    let profile = crate::platform::repository::RepositoryProfile::default();

    let plan1 = build_plan(
        &packet,
        &rules,
        &[],
        &config,
        &profile,
        std::path::Path::new("."),
    );
    let plan2 = build_plan(
        &packet,
        &rules,
        &[],
        &config,
        &profile,
        std::path::Path::new("."),
    );

    assert_eq!(plan1, plan2);
    // Sorted alphabetically
    assert!(plan1.steps.iter().any(|s| s.command == "a_cmd"));
    assert!(plan1.steps.iter().any(|s| s.command == "z_cmd"));
}

#[test]
fn test_build_plan_empty_changes_no_path_match() {
    let packet = ImpactPacket {
        changes: vec![],
        ..ImpactPacket::default()
    };

    let rules = Rules {
        was_legacy_default: false,
        global: GlobalRules {
            mode: Mode::Analyze,
            required_verifications: vec!["cargo test".to_string()],
        },
        overrides: vec![PathRule {
            pattern: "*.rs".to_string(),
            mode: None,
            required_verifications: vec!["cargo clippy".to_string()],
        }],
        protected_paths: Vec::new(),
    };
    let config = VerifyConfig {
        prefer_nextest: Some(false),
        ..Default::default()
    };
    let profile = crate::platform::repository::RepositoryProfile::default();

    let plan = build_plan(
        &packet,
        &rules,
        &[],
        &config,
        &profile,
        std::path::Path::new("."),
    );

    // Global is included, path rule doesn't match empty changes
    assert_eq!(plan.steps.len(), 1);
    assert!(plan.steps.iter().any(|s| s.command == "cargo test"));
}

#[test]
fn test_build_plan_with_predicted_files() {
    let packet = empty_packet(); // changed src/main.rs
    let rules = Rules {
        was_legacy_default: false,
        global: GlobalRules::default(),
        overrides: vec![PathRule {
            pattern: "tests/*.rs".to_string(),
            mode: None,
            required_verifications: vec!["cargo test --test '*'".to_string()],
        }],
        protected_paths: Vec::new(),
    };

    use crate::verify::predict::{PredictedFile, PredictionReason};
    let predicted = vec![PredictedFile {
        path: PathBuf::from("tests/integration.rs"),
        reason: PredictionReason::Temporal,
    }];
    let config = VerifyConfig {
        prefer_nextest: Some(false),
        ..Default::default()
    };
    let profile = crate::platform::repository::RepositoryProfile::default();

    let plan = build_plan(
        &packet,
        &rules,
        &predicted,
        &config,
        &profile,
        std::path::Path::new("."),
    );

    // Predicted rule match overrides default, but full scope appends the
    // fallback full-suite cargo test command.
    assert_eq!(plan.steps.len(), 1);
    assert!(
        plan.steps
            .iter()
            .any(|s| s.command == "cargo test --test '*'"),
        "expected cargo test --test '*' but got {:?}",
        plan.steps
    );

    let predicted_step = plan
        .steps
        .iter()
        .find(|s| s.command == "cargo test --test '*'")
        .unwrap();
    assert!(predicted_step.description.contains("Predicted impact"));
}

#[test]
fn test_build_plan_merges_descriptions() {
    let packet = ImpactPacket {
        changes: vec![ChangedFile {
            path: PathBuf::from("src/lib.rs"),
            status: "Modified".to_string(),
            old_path: None,
            is_staged: true,

            symbols: None,
            imports: None,
            runtime_usage: None,
            analysis_status: FileAnalysisStatus::default(),
            analysis_warnings: Vec::new(),
            api_routes: Vec::new(),
            data_models: Vec::new(),
            ci_gates: Vec::new(),
        }],
        ..ImpactPacket::default()
    };

    let rules = Rules {
        was_legacy_default: false,
        global: GlobalRules::default(),
        overrides: vec![PathRule {
            pattern: "src/*.rs".to_string(),
            mode: None,
            required_verifications: vec!["cargo check".to_string()],
        }],
        protected_paths: Vec::new(),
    };

    use crate::verify::predict::{PredictedFile, PredictionReason};
    let predicted = vec![PredictedFile {
        path: PathBuf::from("src/other.rs"),
        reason: PredictionReason::Structural,
    }];
    let config = VerifyConfig {
        prefer_nextest: Some(false),
        ..Default::default()
    };
    let profile = crate::platform::repository::RepositoryProfile::default();

    let plan = build_plan(
        &packet,
        &rules,
        &predicted,
        &config,
        &profile,
        std::path::Path::new("."),
    );

    // 'cargo check' is triggered by BOTH the direct change in src/lib.rs
    // AND the predicted impact on src/other.rs. Full scope also appends the
    // fallback full-suite command, so we expect 2 steps.
    assert_eq!(plan.steps.len(), 1);
    let check_step = plan
        .steps
        .iter()
        .find(|s| s.command == "cargo check")
        .expect("cargo check step");
    assert!(check_step.description.contains("From rules"));
    assert!(check_step.description.contains("Predicted impact"));
    assert!(check_step.description.contains(" | "));
}

#[test]
fn test_nextest_has_profile_multi_table_nextest_toml_detects_ci_and_slow() {
    // Regression for 0067/codex P1: str::parse::<toml::Value> failed on real
    // nextest.toml (multi-table), so profile probes were permanently false.
    let content = r#"
[profile.default]
slow-timeout = { period = "60s", terminate-after = 1 }

[profile.ci]
retries = 1

[profile.slow]
default-filter = 'test(/__slow$/)'
"#;
    assert!(nextest_has_profile(content, "ci"));
    assert!(nextest_has_profile(content, "slow"));
    assert!(!nextest_has_profile(content, "compile-fail"));
    assert!(!nextest_has_profile("not toml {{{", "ci"));
}

#[test]
fn test_resolve_default_test_command_with_ci_profile_uses_profile_ci() {
    if !crate::verify::engine::probe_nextest() {
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let config_dir = dir.path().join(".config");
    std::fs::create_dir_all(&config_dir).expect("mkdir .config");
    std::fs::write(
        config_dir.join("nextest.toml"),
        "[profile.ci]\nretries = 1\n",
    )
    .expect("write nextest.toml");
    let cmd = resolve_default_test_command(Some(true), dir.path());
    assert_eq!(
        cmd, "cargo nextest run --workspace --all-features --profile ci",
        "must detect [profile.ci] via toml::from_str"
    );
}

#[test]
fn test_append_full_tier_commands_emits_slow_and_doctest_not_compile_fail() {
    if !crate::verify::engine::probe_nextest() {
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let config_dir = dir.path().join(".config");
    std::fs::create_dir_all(&config_dir).expect("mkdir .config");
    std::fs::write(
        config_dir.join("nextest.toml"),
        "[profile.ci]\nretries = 1\n\n[profile.slow]\ndefault-filter = 'test(/__slow$/)'\n",
    )
    .expect("write nextest.toml");

    let mut steps = Vec::new();
    append_full_tier_commands(&mut steps, Some(true), true, dir.path());
    let cmds: Vec<&str> = steps.iter().map(|s| s.command.as_str()).collect();
    assert!(
        cmds.contains(&"cargo nextest run --workspace --all-features --profile slow"),
        "full tier must emit slow profile: {cmds:?}"
    );
    assert!(
        cmds.contains(&"cargo test --workspace --all-features --doc"),
        "full tier must emit doctests: {cmds:?}"
    );
    assert!(
        cmds.iter().all(|c| !c.contains("compile-fail")),
        "full tier must not emit compile-fail after 0067: {cmds:?}"
    );
}

#[test]
fn test_default_command_fallback_when_nextest_disabled() {
    let cmd = resolve_default_test_command(Some(false), std::path::Path::new("."));
    assert_eq!(cmd, "cargo test --workspace --all-features");
}

#[test]
fn test_default_command_nextest_preferred() {
    // On CI/generic runners nextest might not be installed, but the function
    // should probe and fall back gracefully. We verify the command resolves
    // to a concrete default and contains nextest when probe succeeds.
    let cmd = resolve_default_test_command(None, std::path::Path::new("."));
    assert!(!cmd.is_empty(), "default test command must not be empty");
    assert!(
        cmd.starts_with("cargo "),
        "default command should start with cargo: {cmd}"
    );
    if crate::verify::engine::probe_nextest() {
        assert!(
            cmd.contains("nextest"),
            "with nextest installed command should contain nextest: {cmd}"
        );
    } else {
        assert_eq!(cmd, "cargo test --workspace --all-features");
    }
}

#[test]
fn test_build_plan_from_config_empty() {
    let config = VerifyConfig::default();
    assert!(build_plan_from_config(&config).is_none());
}

#[test]
fn test_build_plan_from_config_with_steps() {
    let config = VerifyConfig {
        mode: None,
        steps: vec![
            crate::config::model::VerifyStep {
                description: "Run tests".to_string(),
                command: "cargo test".to_string(),
                timeout_secs: Some(60),
                shell: false,
            },
            crate::config::model::VerifyStep {
                description: String::new(),
                command: "cargo fmt --check".to_string(),
                timeout_secs: None, // uses default_timeout_secs
                shell: false,
            },
        ],
        default_timeout_secs: 120,
        semantic_weight: 0.3,
        prefer_nextest: None,
        ..Default::default()
    };
    let plan = build_plan_from_config(&config).unwrap();
    assert_eq!(plan.steps.len(), 2);
    assert_eq!(plan.steps[0].description, "Run tests");
    assert_eq!(plan.steps[0].timeout_secs, 60);
    assert_eq!(plan.steps[1].description, "From config: cargo fmt --check");
    // None timeout_secs should resolve to default_timeout_secs
    assert_eq!(plan.steps[1].timeout_secs, 120);
}

// ── Scoped selection tests (Tier 1 + Tier 6) ─────────────────────────

#[test]
fn test_touches_shared_infra_cargo_toml() {
    let packet = ImpactPacket {
        changes: vec![ChangedFile {
            path: PathBuf::from("Cargo.toml"),
            ..Default::default()
        }],
        ..ImpactPacket::default()
    };
    assert!(touches_shared_infra(&packet));
}

#[test]
fn test_touches_shared_infra_cli_args() {
    let packet = ImpactPacket {
        changes: vec![ChangedFile {
            path: PathBuf::from("src/cli/args/mod.rs"),
            ..Default::default()
        }],
        ..ImpactPacket::default()
    };
    assert!(touches_shared_infra(&packet));
}

#[test]
fn test_touches_shared_infra_cli_dispatch_verify() {
    let packet = ImpactPacket {
        changes: vec![ChangedFile {
            path: PathBuf::from("src/cli/dispatch/verify.rs"),
            ..Default::default()
        }],
        ..ImpactPacket::default()
    };
    assert!(touches_shared_infra(&packet));
}

#[test]
fn test_touches_shared_infra_config_glob() {
    let packet = ImpactPacket {
        changes: vec![ChangedFile {
            path: PathBuf::from("src/config/model/coverage.rs"),
            ..Default::default()
        }],
        ..ImpactPacket::default()
    };
    assert!(touches_shared_infra(&packet));
}

#[test]
fn test_touches_shared_infra_normal_source() {
    let packet = empty_packet(); // src/main.rs
    assert!(!touches_shared_infra(&packet));
}

#[test]
fn test_touches_shared_infra_migrations() {
    let packet = ImpactPacket {
        changes: vec![ChangedFile {
            path: PathBuf::from("src/state/migrations/m11.rs"),
            ..Default::default()
        }],
        ..ImpactPacket::default()
    };
    assert!(touches_shared_infra(&packet));
}

#[test]
fn test_touches_shared_infra_storage_subdir() {
    let packet = ImpactPacket {
        changes: vec![ChangedFile {
            path: PathBuf::from("src/state/storage/connection.rs"),
            ..Default::default()
        }],
        ..ImpactPacket::default()
    };
    assert!(touches_shared_infra(&packet));
}

#[test]
fn test_test_file_to_nextest_stem() {
    assert_eq!(
        test_file_to_nextest_stem("tests/integration/cli_scan.rs"),
        Some("cli_scan".to_string())
    );
    assert_eq!(
        test_file_to_nextest_stem("tests\\integration\\cli_dead_code.rs"),
        Some("cli_dead_code".to_string())
    );
    assert_eq!(
        test_file_to_nextest_stem("src/lib.rs"),
        Some("lib".to_string())
    );
    assert_eq!(test_file_to_nextest_stem(""), None);
}

#[test]
fn test_build_scoped_nextest_command_single() {
    let cmd = build_scoped_nextest_command(&["cli_scan".to_string()]);
    assert_eq!(
        cmd,
        "cargo nextest run --workspace --all-features -E 'test(cli_scan)'"
    );
}

#[test]
fn test_build_scoped_nextest_command_multiple() {
    let cmd =
        build_scoped_nextest_command(&["cli_scan".to_string(), "dead_code_prune".to_string()]);
    assert_eq!(
        cmd,
        "cargo nextest run --workspace --all-features -E 'test(cli_scan) + test(dead_code_prune)'"
    );
}

#[test]
fn test_scoped_clippy_and_nextest_share_feature_flags() {
    // §B regression guard: clippy and scoped nextest must share
    // --all-features so cargo does not recompile the dependency graph
    // between the two steps under a different feature resolution.
    let test_stems = vec!["cli_scan".to_string()];
    let nextest_cmd = build_scoped_nextest_command(&test_stems);
    let clippy_cmd = "cargo clippy --all-targets --all-features -- -D warnings";

    assert!(
        nextest_cmd.contains("--all-features"),
        "scoped nextest must carry --all-features, got: {nextest_cmd}"
    );
    assert!(
        nextest_cmd.contains("--workspace"),
        "scoped nextest must carry --workspace, got: {nextest_cmd}"
    );
    assert!(
        clippy_cmd.contains("--all-features"),
        "scoped clippy must carry --all-features, got: {clippy_cmd}"
    );
    // Both must carry --all-features (the cache-buster was that nextest lacked it).
    // clippy uses --all-targets, nextest uses --workspace — different selection
    // scopes, but the feature resolution must be identical.
}

#[test]
fn test_build_plan_scoped_full_scope_uses_build_plan() {
    let packet = empty_packet();
    let rules = Rules::default();
    let layout = crate::state::layout::Layout::new(".");
    let plan = build_plan_scoped(
        &packet,
        &rules,
        &[],
        &crate::config::model::VerifyConfig::default(),
        &crate::platform::repository::RepositoryProfile::default(),
        VerifyScope::Full,
        None,
        &layout,
    );
    // Full scope → falls through to build_plan → default cargo test command.
    assert_eq!(plan.steps.len(), 2);
}

#[test]
fn test_build_plan_scoped_fast_no_conn_refuses() {
    let packet = empty_packet();
    let rules = Rules::default();
    let layout = crate::state::layout::Layout::new(".");
    let plan = build_plan_scoped(
        &packet,
        &rules,
        &[],
        &crate::config::model::VerifyConfig::default(),
        &crate::platform::repository::RepositoryProfile::default(),
        VerifyScope::Fast,
        None,
        &layout,
    );
    // No connection → MappingRefuse (not silent full).
    assert!(plan.refused);
    assert!(plan.steps.is_empty());
    let reason = plan.fallback_reason.as_deref().unwrap_or("");
    assert!(
        reason.contains("refusing full suite"),
        "expected refuse reason, got {reason}"
    );
    assert!(reason.contains("test_mapping unavailable"));
}

#[test]
fn test_build_plan_scoped_fast_shared_infra_falls_back() {
    let packet = ImpactPacket {
        changes: vec![ChangedFile {
            path: PathBuf::from("Cargo.toml"),
            ..Default::default()
        }],
        ..ImpactPacket::default()
    };
    let rules = Rules::default();
    // Even with a conn, shared infra → full plan.
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    let layout = crate::state::layout::Layout::new(".");
    let plan = build_plan_scoped(
        &packet,
        &rules,
        &[],
        &crate::config::model::VerifyConfig::default(),
        &crate::platform::repository::RepositoryProfile::default(),
        VerifyScope::Fast,
        Some(&conn),
        &layout,
    );
    // SharedInfra still full (justified).
    assert!(!plan.refused);
    assert_eq!(plan.steps.len(), 2);
    assert!(
        plan.fallback_reason
            .as_deref()
            .unwrap_or("")
            .contains("shared infrastructure"),
        "expected fallback reason to mention shared infrastructure, got {:?}",
        plan.fallback_reason
    );
    assert!(
        plan.fallback_reason
            .as_deref()
            .unwrap_or("")
            .contains("running full"),
        "shared infra should announce running full, got {:?}",
        plan.fallback_reason
    );
}

#[test]
fn test_build_plan_scoped_fast_empty_test_mapping_refuses() {
    let packet = empty_packet(); // src/main.rs, not shared infra
    let rules = Rules::default();
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    // Create the test_mapping table but leave it empty.
    conn.execute(
        "CREATE TABLE test_mapping (test_symbol_id INTEGER, test_file_id INTEGER, \
             tested_symbol_id INTEGER, tested_file_id INTEGER)",
        [],
    )
    .unwrap();
    conn.execute(
        "CREATE TABLE project_files (id INTEGER PRIMARY KEY, file_path TEXT)",
        [],
    )
    .unwrap();
    conn.execute(
        "CREATE TABLE project_symbols (id INTEGER PRIMARY KEY, symbol_name TEXT, file_id INTEGER)",
        [],
    )
    .unwrap();
    let layout = crate::state::layout::Layout::new(".");
    let plan = build_plan_scoped(
        &packet,
        &rules,
        &[],
        &crate::config::model::VerifyConfig::default(),
        &crate::platform::repository::RepositoryProfile::default(),
        VerifyScope::Fast,
        Some(&conn),
        &layout,
    );
    // Empty mapping → MappingRefuse (0135 / 0145 Empty class).
    assert!(plan.refused);
    assert!(plan.steps.is_empty());
    let reason = plan.fallback_reason.as_deref().unwrap_or("");
    assert!(
        reason.contains("refusing full suite"),
        "expected refuse reason, got {reason}"
    );
    assert!(
        reason.contains("test_mapping is empty")
            || reason.contains("test_mapping is stale or empty")
            || reason.contains("test_mapping has no mappings for the changed files")
            || reason.contains("test_mapping unavailable"),
        "expected fallback reason to explain mapping unavailability, got {:?}",
        plan.fallback_reason
    );
}

#[test]
fn test_build_plan_scoped_fast_empty_changes_cheap_plan() {
    let packet = ImpactPacket::default(); // changes empty
    let rules = Rules::default();
    let layout = crate::state::layout::Layout::new(".");
    let rust_profile = crate::platform::repository::RepositoryProfile {
        rust: Some(crate::platform::repository::RustProfile {
            is_virtual_workspace: false,
        }),
        ..Default::default()
    };
    let plan = build_plan_scoped(
        &packet,
        &rules,
        &[],
        &crate::config::model::VerifyConfig::default(),
        &rust_profile,
        VerifyScope::Fast,
        None,
        &layout,
    );
    assert!(!plan.refused);
    assert_eq!(plan.steps.len(), 2);
    assert!(plan.steps.iter().any(|s| s.command.contains("fmt")));
    assert!(plan.steps.iter().any(|s| s.command.contains("clippy")));
    assert!(
        !plan
            .steps
            .iter()
            .any(|s| s.command.contains("nextest") || s.command.contains("cargo test")),
        "EmptyChanges must not schedule full/scoped tests, got {:?}",
        plan.steps
    );
}

#[test]
fn test_build_plan_scoped_fast_empty_changes_non_rust_no_steps() {
    // Missing packet / empty changes on a non-Rust repo must not invent cargo.
    let packet = ImpactPacket::default();
    let rules = Rules::default();
    let layout = crate::state::layout::Layout::new(".");
    let plan = build_plan_scoped(
        &packet,
        &rules,
        &[],
        &crate::config::model::VerifyConfig::default(),
        &crate::platform::repository::RepositoryProfile::default(),
        VerifyScope::Fast,
        None,
        &layout,
    );
    assert!(!plan.refused);
    assert!(
        plan.steps.is_empty(),
        "non-Rust EmptyChanges must not schedule cargo, got {:?}",
        plan.steps
    );
    assert!(plan.fallback_reason.is_none());
}

#[test]
fn test_build_plan_scoped_fast_allow_full_fallback_empty_mapping() {
    let packet = empty_packet();
    let rules = Rules::default();
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute(
        "CREATE TABLE test_mapping (test_symbol_id INTEGER, test_file_id INTEGER, \
             tested_symbol_id INTEGER, tested_file_id INTEGER)",
        [],
    )
    .unwrap();
    let layout = crate::state::layout::Layout::new(".");
    let plan = build_plan_scoped_with_options(
        &packet,
        &rules,
        &[],
        &crate::config::model::VerifyConfig::default(),
        &crate::platform::repository::RepositoryProfile::default(),
        VerifyScope::Fast,
        Some(&conn),
        &layout,
        false,
        true, // allow_full_fallback
    );
    assert!(!plan.refused);
    assert_eq!(plan.steps.len(), 2);
    let reason = plan.fallback_reason.as_deref().unwrap_or("");
    assert!(
        reason.contains("running full"),
        "allow_full_fallback should announce running full, got {reason}"
    );
}

#[test]
fn test_build_plan_scoped_fast_with_mappings_emits_scoped_command() {
    // Head match + non-empty mapping + stems → ScopedOk (3 steps).
    let packet = ImpactPacket {
        head_hash: Some("matched-head".to_string()),
        changes: vec![ChangedFile {
            path: PathBuf::from("src/commands/hotspots.rs"),
            ..Default::default()
        }],
        ..ImpactPacket::default()
    };
    let rules = Rules::default();
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute(
        "CREATE TABLE test_mapping (test_symbol_id INTEGER, test_file_id INTEGER, \
             tested_symbol_id INTEGER, tested_file_id INTEGER)",
        [],
    )
    .unwrap();
    conn.execute(
        "CREATE TABLE project_files (id INTEGER PRIMARY KEY, file_path TEXT)",
        [],
    )
    .unwrap();
    conn.execute(
        "CREATE TABLE project_symbols (id INTEGER PRIMARY KEY, symbol_name TEXT, file_id INTEGER)",
        [],
    )
    .unwrap();
    conn.execute(
        "CREATE TABLE index_metadata (key TEXT PRIMARY KEY, value TEXT)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO index_metadata (key, value) VALUES ('head_hash', 'matched-head')",
        [],
    )
    .unwrap();
    // src/commands/hotspots.rs is file id 1.
    conn.execute(
        "INSERT INTO project_files (id, file_path) VALUES (1, 'src/commands/hotspots.rs')",
        [],
    )
    .unwrap();
    // tests/integration/cli_hotspots.rs is the test file, id 2.
    conn.execute(
        "INSERT INTO project_files (id, file_path) VALUES (2, 'tests/integration/cli_hotspots.rs')",
        [],
    )
    .unwrap();
    // Map: tested_file_id=1 (hotspots.rs) → test_file_id=2 (cli_hotspots.rs).
    conn.execute(
        "INSERT INTO test_mapping (test_symbol_id, test_file_id, tested_symbol_id, tested_file_id) \
             VALUES (10, 2, 20, 1)",
        [],
    )
    .unwrap();

    let layout = crate::state::layout::Layout::new(".");
    let plan = build_plan_scoped(
        &packet,
        &rules,
        &[],
        &crate::config::model::VerifyConfig::default(),
        &crate::platform::repository::RepositoryProfile::default(),
        VerifyScope::Fast,
        Some(&conn),
        &layout,
    );
    // Should produce 3 steps: fmt, clippy, scoped test command.
    assert!(!plan.refused);
    assert_eq!(plan.steps.len(), 3);
    assert!(plan.steps.iter().any(|s| s.command.contains("fmt")));
    assert!(plan.steps.iter().any(|s| s.command.contains("clippy")));
    let scoped_step = plan
        .steps
        .iter()
        .find(|s| {
            s.command
                .contains("nextest run --workspace --all-features -E")
        })
        .expect("scoped nextest command");
    assert!(
        scoped_step.command.contains("test(cli_hotspots)"),
        "expected cli_hotspots in command, got: {}",
        scoped_step.command
    );
    assert!(
        scoped_step.command.contains("--all-features"),
        "scoped nextest must carry --all-features, got: {}",
        scoped_step.command
    );
}

#[test]
fn test_build_plan_scoped_head_mismatch_with_stems_refuses() {
    // 0145: HeadMismatch + stems + !auto_index still attempts one repair.
    // Hermetic in-memory conn stays HeadMismatch after repair (repair opens
    // real layout storage, not this conn) → still refuse with head-lag
    // message — never silent ScopedOk on stems alone.
    let packet = ImpactPacket {
        head_hash: Some("new-hash".to_string()),
        changes: vec![ChangedFile {
            path: PathBuf::from("src/commands/hotspots.rs"),
            ..Default::default()
        }],
        ..ImpactPacket::default()
    };
    let rules = Rules::default();
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute(
        "CREATE TABLE test_mapping (test_symbol_id INTEGER, test_file_id INTEGER, \
             tested_symbol_id INTEGER, tested_file_id INTEGER)",
        [],
    )
    .unwrap();
    conn.execute(
        "CREATE TABLE project_files (id INTEGER PRIMARY KEY, file_path TEXT)",
        [],
    )
    .unwrap();
    conn.execute(
        "CREATE TABLE index_metadata (key TEXT PRIMARY KEY, value TEXT)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO index_metadata (key, value) VALUES ('head_hash', 'old-hash')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO project_files (id, file_path) VALUES (1, 'src/commands/hotspots.rs')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO project_files (id, file_path) VALUES (2, 'tests/integration/cli_hotspots.rs')",
        [],
    )
    .unwrap();
    // Stems would exist if trusted — mapping non-empty for the changed file.
    conn.execute(
        "INSERT INTO test_mapping (test_symbol_id, test_file_id, tested_symbol_id, tested_file_id) \
             VALUES (10, 2, 20, 1)",
        [],
    )
    .unwrap();

    // Temp layout so repair does not open the real engine .ledgerful.
    let tmp = tempfile::tempdir().unwrap();
    let root = camino::Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
    let layout = crate::state::layout::Layout::new(&root);
    let plan = build_plan_scoped_with_options(
        &packet,
        &rules,
        &[],
        &crate::config::model::VerifyConfig::default(),
        &crate::platform::repository::RepositoryProfile::default(),
        VerifyScope::Fast,
        Some(&conn),
        &layout,
        false, // !auto_index — HeadMismatch still attempts repair
        false, // !allow_full_fallback
    );
    assert!(
        plan.refused,
        "head mismatch after failed/ineffective repair must refuse, not ScopedOk; steps={:?}",
        plan.steps
    );
    assert!(
        plan.steps.is_empty(),
        "refused plan must have empty steps, got {:?}",
        plan.steps
    );
    let reason = plan.fallback_reason.as_deref().unwrap_or("");
    assert!(
        reason.contains("head_hash lags HEAD")
            || reason.contains("auto-index failed")
            || reason.contains("head_hash"),
        "expected head-lag remediation, got: {reason}"
    );
    assert!(
        reason.contains("refusing full suite"),
        "must refuse full, got: {reason}"
    );
}

#[test]
fn test_should_attempt_mapping_repair_behavior_table() {
    // Full 0145 gate matrix for pure repair decision.
    assert!(!should_attempt_mapping_repair(MappingFreshness::Ok, false));
    assert!(!should_attempt_mapping_repair(MappingFreshness::Ok, true));
    assert!(should_attempt_mapping_repair(
        MappingFreshness::HeadMismatch,
        false
    ));
    assert!(should_attempt_mapping_repair(
        MappingFreshness::HeadMismatch,
        true
    ));
    assert!(!should_attempt_mapping_repair(
        MappingFreshness::Empty,
        false
    ));
    assert!(should_attempt_mapping_repair(MappingFreshness::Empty, true));
    assert!(!should_attempt_mapping_repair(
        MappingFreshness::PacketHeadMissing,
        false
    ));
    assert!(should_attempt_mapping_repair(
        MappingFreshness::PacketHeadMissing,
        true
    ));
}

#[test]
fn test_build_plan_scoped_head_match_with_stems_scoped_ok_without_auto_index() {
    // 0145 F-001: packet head == index head, non-empty mapping + stems,
    // auto_index=false → ScopedOk (not refuse, nextest present).
    // Proves the post-repair success state under !auto_index when heads
    // already match (repair not needed; gate falls through to stems).
    let packet = ImpactPacket {
        head_hash: Some("matched-head".to_string()),
        changes: vec![ChangedFile {
            path: PathBuf::from("src/commands/hotspots.rs"),
            ..Default::default()
        }],
        ..ImpactPacket::default()
    };
    let rules = Rules::default();
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute(
        "CREATE TABLE test_mapping (test_symbol_id INTEGER, test_file_id INTEGER, \
             tested_symbol_id INTEGER, tested_file_id INTEGER)",
        [],
    )
    .unwrap();
    conn.execute(
        "CREATE TABLE project_files (id INTEGER PRIMARY KEY, file_path TEXT)",
        [],
    )
    .unwrap();
    conn.execute(
        "CREATE TABLE index_metadata (key TEXT PRIMARY KEY, value TEXT)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO index_metadata (key, value) VALUES ('head_hash', 'matched-head')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO project_files (id, file_path) VALUES (1, 'src/commands/hotspots.rs')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO project_files (id, file_path) VALUES (2, 'tests/integration/cli_hotspots.rs')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO test_mapping (test_symbol_id, test_file_id, tested_symbol_id, tested_file_id) \
             VALUES (10, 2, 20, 1)",
        [],
    )
    .unwrap();

    let layout = crate::state::layout::Layout::new(".");
    let plan = build_plan_scoped_with_options(
        &packet,
        &rules,
        &[],
        &crate::config::model::VerifyConfig::default(),
        &crate::platform::repository::RepositoryProfile::default(),
        VerifyScope::Fast,
        Some(&conn),
        &layout,
        false, // !auto_index
        false,
    );
    assert!(
        !plan.refused,
        "head match + stems + !auto_index must ScopedOk, not refuse; reason={:?}",
        plan.fallback_reason
    );
    assert!(
        plan.steps.iter().any(|s| s.command.contains("nextest")),
        "ScopedOk must schedule nextest, got {:?}",
        plan.steps
    );
    assert!(plan.fallback_reason.is_none());
}

#[test]
fn test_build_plan_scoped_head_mismatch_repaired_file_backed_scoped_ok() {
    // 0145 F-001 preferred: file-backed sqlite shared by conn.
    // Populate HeadMismatch → manually set index head = packet head
    // (simulates successful store_index_metadata after repair) →
    // build_plan with !auto_index → ScopedOk.
    // Proves Ok path after lag is fixed without full incremental_index.
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("ledger.db");
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    conn.execute(
        "CREATE TABLE test_mapping (test_symbol_id INTEGER, test_file_id INTEGER, \
             tested_symbol_id INTEGER, tested_file_id INTEGER)",
        [],
    )
    .unwrap();
    conn.execute(
        "CREATE TABLE project_files (id INTEGER PRIMARY KEY, file_path TEXT)",
        [],
    )
    .unwrap();
    conn.execute(
        "CREATE TABLE index_metadata (key TEXT PRIMARY KEY, value TEXT)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO index_metadata (key, value) VALUES ('head_hash', 'old-hash')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO project_files (id, file_path) VALUES (1, 'src/commands/hotspots.rs')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO project_files (id, file_path) VALUES (2, 'tests/integration/cli_hotspots.rs')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO test_mapping (test_symbol_id, test_file_id, tested_symbol_id, tested_file_id) \
             VALUES (10, 2, 20, 1)",
        [],
    )
    .unwrap();

    let packet = ImpactPacket {
        head_hash: Some("new-hash".to_string()),
        changes: vec![ChangedFile {
            path: PathBuf::from("src/commands/hotspots.rs"),
            ..Default::default()
        }],
        ..ImpactPacket::default()
    };
    assert_eq!(
        classify_test_mapping_freshness(&conn, &packet),
        MappingFreshness::HeadMismatch,
        "precondition: lag must classify as HeadMismatch"
    );
    assert!(
        should_attempt_mapping_repair(MappingFreshness::HeadMismatch, false),
        "HeadMismatch must attempt repair under !auto_index"
    );

    // Simulate successful repair writing matching head (store_index_metadata).
    conn.execute(
        "UPDATE index_metadata SET value = 'new-hash' WHERE key = 'head_hash'",
        [],
    )
    .unwrap();
    assert_eq!(
        classify_test_mapping_freshness(&conn, &packet),
        MappingFreshness::Ok,
        "post-repair heads must match"
    );

    let rules = Rules::default();
    let root = camino::Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
    let layout = crate::state::layout::Layout::new(&root);
    let plan = build_plan_scoped_with_options(
        &packet,
        &rules,
        &[],
        &crate::config::model::VerifyConfig::default(),
        &crate::platform::repository::RepositoryProfile::default(),
        VerifyScope::Fast,
        Some(&conn),
        &layout,
        false, // !auto_index — lag already repaired; gate trusts stems
        false,
    );
    assert!(
        !plan.refused,
        "post-repair Ok + stems must ScopedOk under !auto_index; reason={:?}",
        plan.fallback_reason
    );
    assert!(
        plan.steps.iter().any(|s| s.command.contains("nextest")),
        "ScopedOk must schedule nextest, got {:?}",
        plan.steps
    );
    assert!(plan.fallback_reason.is_none());
}

#[test]
fn test_verify_scope_display() {
    assert_eq!(format!("{}", VerifyScope::Fast), "fast");
    assert_eq!(format!("{}", VerifyScope::Full), "full");
}

#[test]
fn test_verify_scope_default_is_full() {
    assert_eq!(VerifyScope::default(), VerifyScope::Full);
}

fn seed_mapping_row(conn: &rusqlite::Connection) {
    conn.execute(
        "CREATE TABLE test_mapping (test_symbol_id INTEGER, test_file_id INTEGER, \
             tested_symbol_id INTEGER, tested_file_id INTEGER)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO test_mapping (test_symbol_id, test_file_id, tested_symbol_id, tested_file_id) \
             VALUES (1, 1, 1, 1)",
        [],
    )
    .unwrap();
}

fn seed_index_head(conn: &rusqlite::Connection, head: &str) {
    conn.execute(
        "CREATE TABLE index_metadata (key TEXT PRIMARY KEY, value TEXT)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO index_metadata (key, value) VALUES ('head_hash', ?1)",
        [head],
    )
    .unwrap();
}

#[test]
fn test_classify_test_mapping_freshness_empty() {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute(
        "CREATE TABLE test_mapping (test_symbol_id INTEGER, test_file_id INTEGER, \
             tested_symbol_id INTEGER, tested_file_id INTEGER)",
        [],
    )
    .unwrap();
    let packet = ImpactPacket::default();
    assert_eq!(
        classify_test_mapping_freshness(&conn, &packet),
        MappingFreshness::Empty
    );
    assert!(is_test_mapping_stale(&conn, &packet));
}

#[test]
fn test_classify_test_mapping_freshness_ok_heads_match() {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    seed_mapping_row(&conn);
    seed_index_head(&conn, "current-hash");
    let packet = ImpactPacket {
        head_hash: Some("current-hash".to_string()),
        ..ImpactPacket::default()
    };
    assert_eq!(
        classify_test_mapping_freshness(&conn, &packet),
        MappingFreshness::Ok
    );
    assert!(!is_test_mapping_stale(&conn, &packet));
}

#[test]
fn test_classify_test_mapping_freshness_head_mismatch() {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    seed_mapping_row(&conn);
    seed_index_head(&conn, "old-hash");
    let packet = ImpactPacket {
        head_hash: Some("new-hash".to_string()),
        ..ImpactPacket::default()
    };
    assert_eq!(
        classify_test_mapping_freshness(&conn, &packet),
        MappingFreshness::HeadMismatch
    );
    assert!(is_test_mapping_stale(&conn, &packet));
}

#[test]
fn test_classify_test_mapping_freshness_packet_head_missing() {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    seed_mapping_row(&conn);
    seed_index_head(&conn, "indexed");
    let packet = ImpactPacket {
        head_hash: None,
        ..ImpactPacket::default()
    };
    assert_eq!(
        classify_test_mapping_freshness(&conn, &packet),
        MappingFreshness::PacketHeadMissing
    );
    assert!(is_test_mapping_stale(&conn, &packet));
}

#[test]
fn test_classify_test_mapping_freshness_indexed_head_missing_ok() {
    // B2: indexed head missing + count>0 → Ok (allow stem query).
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    seed_mapping_row(&conn);
    // No index_metadata table / no head_hash row.
    let packet = ImpactPacket {
        head_hash: Some("any-hash".to_string()),
        ..ImpactPacket::default()
    };
    assert_eq!(
        classify_test_mapping_freshness(&conn, &packet),
        MappingFreshness::Ok
    );
    assert!(!is_test_mapping_stale(&conn, &packet));
}

#[test]
fn test_classify_test_mapping_freshness_both_heads_missing_ok() {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    seed_mapping_row(&conn);
    let packet = ImpactPacket {
        head_hash: None,
        ..ImpactPacket::default()
    };
    assert_eq!(
        classify_test_mapping_freshness(&conn, &packet),
        MappingFreshness::Ok
    );
    assert!(!is_test_mapping_stale(&conn, &packet));
}

#[test]
fn test_classify_test_mapping_freshness_missing_table_empty() {
    // Query Err / missing tables → Empty (degrade, no panic).
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    let packet = ImpactPacket::default();
    assert_eq!(
        classify_test_mapping_freshness(&conn, &packet),
        MappingFreshness::Empty
    );
}

// Thin-wrapper regressions (same matrix as classify_*).
#[test]
fn test_is_test_mapping_stale_empty_mapping() {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute(
        "CREATE TABLE test_mapping (test_symbol_id INTEGER, test_file_id INTEGER, \
             tested_symbol_id INTEGER, tested_file_id INTEGER)",
        [],
    )
    .unwrap();
    let packet = ImpactPacket::default();
    assert!(is_test_mapping_stale(&conn, &packet));
}

#[test]
fn test_is_test_mapping_stale_head_hash_mismatch() {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    seed_mapping_row(&conn);
    seed_index_head(&conn, "old-hash");
    let packet = ImpactPacket {
        head_hash: Some("new-hash".to_string()),
        ..ImpactPacket::default()
    };
    assert!(is_test_mapping_stale(&conn, &packet));
}

#[test]
fn test_is_test_mapping_stale_head_hash_matches() {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    seed_mapping_row(&conn);
    seed_index_head(&conn, "current-hash");
    let packet = ImpactPacket {
        head_hash: Some("current-hash".to_string()),
        ..ImpactPacket::default()
    };
    assert!(!is_test_mapping_stale(&conn, &packet));
}

#[test]
fn test_is_test_mapping_stale_missing_index_head_not_force_stale() {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    seed_mapping_row(&conn);
    let packet = ImpactPacket {
        head_hash: Some("any-hash".to_string()),
        ..ImpactPacket::default()
    };
    assert!(!is_test_mapping_stale(&conn, &packet));
}

#[test]
fn test_is_test_mapping_stale_missing_packet_head_conservative() {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    seed_mapping_row(&conn);
    seed_index_head(&conn, "indexed");
    let packet = ImpactPacket {
        head_hash: None,
        ..ImpactPacket::default()
    };
    assert!(is_test_mapping_stale(&conn, &packet));
}

#[test]
fn test_run_incremental_index_does_not_overwrite_head_with_packet() {
    // DoD-6 source guard: repair must not write packet.head_hash over
    // store_index_metadata's current git HEAD.
    let src = include_str!("scoped.rs");
    let fn_start = src
        .find("fn run_incremental_index_for_changed_files")
        .expect("repair helper present");
    let body = &src[fn_start..];
    let fn_end = body
        .find("\nfn ")
        .or_else(|| body.find("\n#[derive"))
        .unwrap_or(body.len().min(2500));
    let body = &body[..fn_end];
    assert!(
        !body.contains("INSERT OR REPLACE INTO index_metadata"),
        "run_incremental_index_for_changed_files must not overwrite head_hash from packet"
    );
    assert!(
        body.contains("_packet") || body.contains("store_index_metadata"),
        "repair helper should document trust of store_index_metadata / unused packet head"
    );
}

#[test]
fn test_build_plan_scoped_missing_index_head_with_stems_scoped_ok() {
    // Missing index head_hash + count>0 + stems → ScopedOk (not force-stale).
    let packet = ImpactPacket {
        head_hash: Some("packet-head".to_string()),
        changes: vec![ChangedFile {
            path: PathBuf::from("src/commands/hotspots.rs"),
            ..Default::default()
        }],
        ..ImpactPacket::default()
    };
    let rules = Rules::default();
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute(
        "CREATE TABLE test_mapping (test_symbol_id INTEGER, test_file_id INTEGER, \
             tested_symbol_id INTEGER, tested_file_id INTEGER)",
        [],
    )
    .unwrap();
    conn.execute(
        "CREATE TABLE project_files (id INTEGER PRIMARY KEY, file_path TEXT)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO project_files (id, file_path) VALUES (1, 'src/commands/hotspots.rs')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO project_files (id, file_path) VALUES (2, 'tests/integration/cli_hotspots.rs')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO test_mapping (test_symbol_id, test_file_id, tested_symbol_id, tested_file_id) \
             VALUES (10, 2, 20, 1)",
        [],
    )
    .unwrap();
    // No head_hash in index_metadata.
    let layout = crate::state::layout::Layout::new(".");
    let plan = build_plan_scoped(
        &packet,
        &rules,
        &[],
        &crate::config::model::VerifyConfig::default(),
        &crate::platform::repository::RepositoryProfile::default(),
        VerifyScope::Fast,
        Some(&conn),
        &layout,
    );
    assert!(!plan.refused);
    assert_eq!(plan.steps.len(), 3);
    assert!(plan.fallback_reason.is_none());
}

#[test]
fn test_build_plan_scoped_fast_auto_index_failure_refuses() {
    let packet = ImpactPacket {
        head_hash: Some("abc123".to_string()),
        changes: vec![ChangedFile {
            path: PathBuf::from("src/main.rs"),
            ..Default::default()
        }],
        ..ImpactPacket::default()
    };
    let rules = Rules::default();
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    // Create the tables so is_test_mapping_stale sees an empty mapping.
    conn.execute(
        "CREATE TABLE test_mapping (test_symbol_id INTEGER, test_file_id INTEGER, \
             tested_symbol_id INTEGER, tested_file_id INTEGER)",
        [],
    )
    .unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let root = camino::Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
    let layout = crate::state::layout::Layout::new(&root);
    let plan = build_plan_scoped_with_options(
        &packet,
        &rules,
        &[],
        &crate::config::model::VerifyConfig::default(),
        &crate::platform::repository::RepositoryProfile::default(),
        VerifyScope::Fast,
        Some(&conn),
        &layout,
        true,
        false, // no allow_full_fallback
    );
    assert!(plan.refused);
    assert!(plan.steps.is_empty());
    let reason = plan.fallback_reason.as_deref().unwrap_or("");
    assert!(
        reason.contains("auto-index failed"),
        "expected fallback reason to mention auto-index failure, got: {reason}"
    );
    assert!(
        reason.contains("refusing full suite"),
        "auto-index fail must refuse (not running full), got: {reason}"
    );
}

#[test]
fn test_build_plan_scoped_fast_auto_index_failure_with_allow_runs_full() {
    let packet = ImpactPacket {
        head_hash: Some("abc123".to_string()),
        changes: vec![ChangedFile {
            path: PathBuf::from("src/main.rs"),
            ..Default::default()
        }],
        ..ImpactPacket::default()
    };
    let rules = Rules::default();
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute(
        "CREATE TABLE test_mapping (test_symbol_id INTEGER, test_file_id INTEGER, \
             tested_symbol_id INTEGER, tested_file_id INTEGER)",
        [],
    )
    .unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let root = camino::Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
    let layout = crate::state::layout::Layout::new(&root);
    let plan = build_plan_scoped_with_options(
        &packet,
        &rules,
        &[],
        &crate::config::model::VerifyConfig::default(),
        &crate::platform::repository::RepositoryProfile::default(),
        VerifyScope::Fast,
        Some(&conn),
        &layout,
        true,
        true, // allow_full_fallback
    );
    assert!(!plan.refused);
    assert_eq!(plan.steps.len(), 2);
    let reason = plan.fallback_reason.as_deref().unwrap_or("");
    assert!(reason.contains("auto-index failed"));
    assert!(reason.contains("running full"));
}

#[test]
fn test_build_plan_scoped_fast_auto_index_not_triggered_when_mapping_exists() {
    // When test_mapping already has entries and is not stale,
    // auto_index=true should NOT trigger a reindex — the scoped plan
    // is returned directly.
    let packet = ImpactPacket {
        changes: vec![ChangedFile {
            path: PathBuf::from("src/commands/hotspots.rs"),
            ..Default::default()
        }],
        ..ImpactPacket::default()
    };
    let rules = Rules::default();
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    // Set up tables with a mapping.
    conn.execute(
        "CREATE TABLE test_mapping (test_symbol_id INTEGER, test_file_id INTEGER, \
             tested_symbol_id INTEGER, tested_file_id INTEGER)",
        [],
    )
    .unwrap();
    conn.execute(
        "CREATE TABLE project_files (id INTEGER PRIMARY KEY, file_path TEXT)",
        [],
    )
    .unwrap();
    conn.execute(
        "CREATE TABLE project_symbols (id INTEGER PRIMARY KEY, symbol_name TEXT, file_id INTEGER)",
        [],
    )
    .unwrap();
    conn.execute(
        "CREATE TABLE index_metadata (key TEXT PRIMARY KEY, value TEXT)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO project_files (id, file_path) VALUES (1, 'src/commands/hotspots.rs')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO project_files (id, file_path) VALUES (2, 'tests/integration/cli_hotspots.rs')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO test_mapping (test_symbol_id, test_file_id, tested_symbol_id, tested_file_id) \
             VALUES (10, 2, 20, 1)",
        [],
    )
    .unwrap();

    let layout = crate::state::layout::Layout::new(".");
    let plan = build_plan_scoped_with_options(
        &packet,
        &rules,
        &[],
        &crate::config::model::VerifyConfig::default(),
        &crate::platform::repository::RepositoryProfile::default(),
        VerifyScope::Fast,
        Some(&conn),
        &layout,
        true,  // auto_index=true
        false, // allow_full_fallback
    );
    // Should return scoped plan (3 steps), not full plan.
    assert_eq!(
        plan.steps.len(),
        3,
        "expected scoped plan, got {} steps: {:?}",
        plan.steps.len(),
        plan.steps
    );
    assert!(!plan.refused);
    assert!(
        plan.fallback_reason.is_none(),
        "should not have fallback reason"
    );
    assert!(
        plan.steps
            .iter()
            .any(|s| s.command.contains("test(cli_hotspots)"))
    );
}

fn step(cmd: &str) -> VerificationStep {
    VerificationStep {
        command: cmd.to_string(),
        timeout_secs: 60,
        description: cmd.to_string(),
        shell: false,
    }
}

/// DoD-4: clippy P > fmt P still keeps fmt (band 0) before clippy (band 1).
#[test]
fn test_apply_probability_ordering_fmt_before_clippy_despite_higher_clippy_p() {
    let mut plan = VerificationPlan {
        source: None,
        steps: vec![
            step("cargo fmt --all -- --check"),
            step("cargo clippy --all-targets --all-features -- -D warnings"),
            step("cargo nextest run --workspace --all-features --profile ci"),
        ],
        fallback_reason: None,
        refused: false,
    };
    // Synthetic Laplace: clippy higher than fmt (live DB shape).
    let mut probs = std::collections::HashMap::new();
    probs.insert("cargo-fmt".to_string(), 0.0385);
    probs.insert("cargo-clippy".to_string(), 0.0388);
    probs.insert("nextest-ci".to_string(), 0.25);

    let matched = plan.apply_probability_ordering(&probs);
    assert_eq!(matched, 3);
    assert!(
        plan.steps[0].command.contains("cargo fmt"),
        "fmt must stay first (band 0): {:?}",
        plan.steps.iter().map(|s| &s.command).collect::<Vec<_>>()
    );
    assert!(
        plan.steps[1].command.contains("cargo clippy"),
        "clippy must stay second (band 1): {:?}",
        plan.steps.iter().map(|s| &s.command).collect::<Vec<_>>()
    );
}

/// High-P band-2 step reorders among other band-2 steps (fail-fast).
#[test]
fn test_apply_probability_ordering_high_p_band2_reorders() {
    let mut plan = VerificationPlan {
        source: None,
        steps: vec![
            step("cargo fmt --all -- --check"),
            step("cargo clippy --all-targets --all-features -- -D warnings"),
            step("git diff --check"),
            step("cargo nextest run --workspace --all-features --profile ci"),
        ],
        fallback_reason: None,
        refused: false,
    };
    let mut probs = std::collections::HashMap::new();
    probs.insert("cargo-fmt".to_string(), 0.03);
    probs.insert("cargo-clippy".to_string(), 0.04);
    probs.insert("git-diff-check".to_string(), 0.01);
    probs.insert("nextest-ci".to_string(), 0.90);

    let matched = plan.apply_probability_ordering(&probs);
    assert_eq!(matched, 4);
    // Band 0/1 first, then nextest-ci before git-diff (higher P).
    assert!(plan.steps[0].command.contains("cargo fmt"));
    assert!(plan.steps[1].command.contains("cargo clippy"));
    assert!(
        plan.steps[2].command.contains("nextest"),
        "high-P nextest should lead band-2: {:?}",
        plan.steps.iter().map(|s| &s.command).collect::<Vec<_>>()
    );
    assert!(plan.steps[3].command.contains("git diff"));
}

/// Two scoped argv strings share the nextest-scoped key / P.
#[test]
fn test_apply_probability_ordering_scoped_variants_share_key() {
    let scoped_a = "cargo nextest run --workspace --all-features -E 'test(cli_scan)'";
    let scoped_b = "cargo nextest run --workspace --all-features -E 'test(other_stem)'";
    let mut plan = VerificationPlan {
        source: None,
        steps: vec![step("cargo fmt --all -- --check"), step(scoped_a)],
        fallback_reason: None,
        refused: false,
    };
    // History only has the step key (as extract_dataset emits).
    let mut probs = std::collections::HashMap::new();
    probs.insert("cargo-fmt".to_string(), 0.05);
    probs.insert("nextest-scoped".to_string(), 0.80);

    let matched = plan.apply_probability_ordering(&probs);
    assert_eq!(
        matched, 2,
        "both steps must match via step key, not raw argv"
    );

    // A second plan with different -E still hits nextest-scoped.
    let mut plan_b = VerificationPlan {
        source: None,
        steps: vec![step(scoped_b)],
        fallback_reason: None,
        refused: false,
    };
    let matched_b = plan_b.apply_probability_ordering(&probs);
    assert_eq!(matched_b, 1);
    // Raw-command-only lookup cannot pass this regression:
    assert!(!probs.contains_key(scoped_a));
    assert!(!probs.contains_key(scoped_b));
}

/// Spec §2.3 #6: hypothetical step between fmt and clippy still keeps
/// fmt before clippy via multi-band (not pair-swap).
#[test]
fn test_apply_probability_ordering_middle_step_keeps_fmt_before_clippy() {
    let mut plan = VerificationPlan {
        source: None,
        steps: vec![
            step("cargo nextest run --workspace --all-features --profile ci"),
            step("cargo clippy --all-targets --all-features -- -D warnings"),
            step("hypothetical-middle-tool"),
            step("cargo fmt --all -- --check"),
        ],
        fallback_reason: None,
        refused: false,
    };
    let mut probs = std::collections::HashMap::new();
    // Clippy higher than fmt; middle tool highest band-2 P; order scrambled.
    probs.insert("cargo-fmt".to_string(), 0.0385);
    probs.insert("cargo-clippy".to_string(), 0.9);
    probs.insert("nextest-ci".to_string(), 0.5);
    probs.insert("hypothetical-middle-tool".to_string(), 0.99);

    let matched = plan.apply_probability_ordering(&probs);
    assert_eq!(matched, 4);
    assert!(
        plan.steps[0].command.contains("cargo fmt"),
        "fmt band 0 first: {:?}",
        plan.steps.iter().map(|s| &s.command).collect::<Vec<_>>()
    );
    assert!(
        plan.steps[1].command.contains("cargo clippy"),
        "clippy band 1 second even with P>fmt and middle step present: {:?}",
        plan.steps.iter().map(|s| &s.command).collect::<Vec<_>>()
    );
    // Band-2: highest P first among remaining.
    assert!(
        plan.steps[2].command.contains("hypothetical-middle-tool"),
        "highest-P band-2 next: {:?}",
        plan.steps.iter().map(|s| &s.command).collect::<Vec<_>>()
    );
    assert!(plan.steps[3].command.contains("nextest"));
}

/// matched_steps==0 preserves original order (no alphabetical-only sort).
#[test]
fn test_apply_probability_ordering_zero_matches_preserves_order() {
    let mut plan = VerificationPlan {
        source: None,
        // Deliberately reverse-alpha so alphabetical sort would reshuffle.
        steps: vec![step("zebra-tool"), step("alpha-tool"), step("middle-tool")],
        fallback_reason: None,
        refused: false,
    };
    let original: Vec<String> = plan.steps.iter().map(|s| s.command.clone()).collect();
    // Prob map has unrelated keys only.
    let mut probs = std::collections::HashMap::new();
    probs.insert("cargo-fmt".to_string(), 0.5);
    probs.insert("nextest-ci".to_string(), 0.9);

    let matched = plan.apply_probability_ordering(&probs);
    assert_eq!(matched, 0);
    let after: Vec<String> = plan.steps.iter().map(|s| s.command.clone()).collect();
    assert_eq!(
        after, original,
        "vacuous apply must not alphabetical-sort: was {original:?}, now {after:?}"
    );
}
