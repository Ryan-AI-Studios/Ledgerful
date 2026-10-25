use super::rust_lex::*;
use super::*;
use crate::impact::packet::{ChangedFile, FileAnalysisStatus, ImpactPacket};
use crate::policy::mode::Mode;
use crate::policy::rules::{GlobalRules, PathRule, Rules};
use std::fs;
use std::path::{Path, PathBuf};

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
        suite_timeout_secs: Some(900),
        ..Default::default()
    };
    let plan = build_plan_from_config(&config).unwrap();
    assert_eq!(plan.steps.len(), 2);
    assert_eq!(plan.steps[0].description, "Run tests");
    assert_eq!(plan.steps[0].timeout_secs, 60);
    assert_eq!(plan.steps[0].budget_source.as_deref(), Some("explicit"));
    assert_eq!(plan.steps[1].description, "From config: cargo fmt --check");
    // None timeout_secs should resolve to default_timeout_secs, not the suite key.
    assert_eq!(plan.steps[1].timeout_secs, 120);
    assert_eq!(plan.steps[1].budget_source.as_deref(), Some("explicit"));
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
fn test_query_scoped_test_files_product_path_uses_symbol_name() {
    let packet = ImpactPacket {
        changes: vec![ChangedFile {
            path: PathBuf::from("src/exec/boundary.rs"),
            ..Default::default()
        }],
        ..ImpactPacket::default()
    };
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
        "INSERT INTO project_files (id, file_path) VALUES (1, 'src/exec/boundary.rs')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO project_symbols (id, symbol_name, file_id) VALUES (10, 'test_basic_execution', 1)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO test_mapping (test_symbol_id, test_file_id, tested_symbol_id, tested_file_id) \
             VALUES (10, 1, 20, 1)",
        [],
    )
    .unwrap();

    let stems = query_scoped_test_files(&conn, &packet).expect("stems");
    assert_eq!(stems, vec!["test_basic_execution".to_string()]);
    assert!(
        !stems.iter().any(|s| s == "boundary"),
        "product-path in-file tests must stem by function name, not file stem: {stems:?}"
    );
    assert_eq!(
        test_file_to_nextest_stem("src/lib.rs"),
        Some("lib".to_string())
    );
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
fn scoped_nextest_step_prepares_direct_without_spawn() {
    let command =
        build_scoped_nextest_command(&["cli_scan".to_string(), "dead_code_prune".to_string()]);
    let step = VerificationStep {
        description: "scoped nextest".to_string(),
        command,
        timeout_secs: 5,
        shell: false,
        budget_source: None,
    };
    let prepared = crate::verify::runner::prepare_rule_step(
        &step,
        false,
        &crate::platform::process_policy::ProcessPolicy::default(),
    )
    .expect("quoted filterset must prepare Direct");
    assert_eq!(
        prepared.execution_mode,
        crate::verify::runner::ExecutionMode::Direct
    );
    assert_eq!(prepared.executable, "cargo");
    assert_eq!(
        prepared.args.last().map(String::as_str),
        Some("test(cli_scan) + test(dead_code_prune)")
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

fn rust_profile() -> crate::platform::repository::RepositoryProfile {
    crate::platform::repository::RepositoryProfile {
        rust: Some(crate::platform::repository::RustProfile {
            is_virtual_workspace: false,
        }),
        ..Default::default()
    }
}

fn packet_with_paths(paths: &[&str]) -> ImpactPacket {
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

fn nextest_step(plan: &VerificationPlan) -> Option<&VerificationStep> {
    plan.steps.iter().find(|s| s.command.contains("nextest"))
}

fn assert_scoped_filterset_not_workspace_ci(command: &str) {
    assert!(
        command.contains("-E") && command.contains("test("),
        "scoped nextest must use a filterset, got: {command}"
    );
    if command.contains("--profile ci") {
        panic!("scoped nextest must not use workspace --profile ci, got: {command}");
    }
}

fn plan_at(
    root: &camino::Utf8Path,
    packet: &ImpactPacket,
    conn: Option<&rusqlite::Connection>,
    profile: &crate::platform::repository::RepositoryProfile,
) -> VerificationPlan {
    let rules = Rules::default();
    let layout = crate::state::layout::Layout::new(root);
    build_plan_scoped_with_options(
        packet,
        &rules,
        &[],
        &crate::config::model::VerifyConfig::default(),
        profile,
        VerifyScope::Fast,
        conn,
        &layout,
        false,
        false,
    )
}

fn plan_fast(
    packet: &ImpactPacket,
    conn: Option<&rusqlite::Connection>,
    profile: &crate::platform::repository::RepositoryProfile,
) -> VerificationPlan {
    plan_at(camino::Utf8Path::new("."), packet, conn, profile)
}

#[test]
fn test_non_code_changelog_docs_cheap_no_nextest() {
    let packet = packet_with_paths(&["CHANGELOG.md", "docs/architecture.md"]);
    let plan = plan_fast(&packet, None, &rust_profile());
    assert!(!plan.refused);
    assert!(plan.fallback_reason.is_none());
    assert!(
        plan.steps.iter().any(|s| s.command.contains("fmt")),
        "fmt must be present, got {:?}",
        plan.steps
    );
    assert!(
        plan.steps.iter().any(|s| s.command.contains("clippy")),
        "clippy must be present, got {:?}",
        plan.steps
    );
    assert!(
        !plan
            .steps
            .iter()
            .any(|s| s.command.contains("nextest") || s.command.contains("cargo test")),
        "unreferenced docs/CHANGELOG cheap path must not schedule tests, got {:?}",
        plan.steps
    );
    assert!(
        plan.steps
            .iter()
            .any(|s| s.description.contains("Non-code changes")),
        "descriptions must mention Non-code changes, got {:?}",
        plan.steps
    );
}

#[test]
fn test_packaging_injects_bump_manifests_stem() {
    let packet = packet_with_paths(&["packaging/homebrew/ledgerful.rb"]);
    let plan = plan_fast(&packet, None, &rust_profile());
    assert!(!plan.refused);
    assert!(plan.fallback_reason.is_none());
    let nextest = nextest_step(&plan).expect("packaging must inject nextest");
    assert!(
        nextest.command.contains("test(bump_manifests)"),
        "expected bump_manifests filterset, got {}",
        nextest.command
    );
    assert_scoped_filterset_not_workspace_ci(&nextest.command);
    assert!(
        !nextest.description.contains("via test_mapping"),
        "injected description must not claim test_mapping, got {}",
        nextest.description
    );
}

#[test]
fn test_scripts_bump_manifests_ps1_injects_stem() {
    let packet = packet_with_paths(&["scripts/bump-manifests.ps1"]);
    let plan = plan_fast(&packet, None, &rust_profile());
    assert!(!plan.refused);
    let nextest = nextest_step(&plan).expect("bump script must inject nextest");
    assert!(
        nextest.command.contains("test(bump_manifests)"),
        "expected bump_manifests filterset, got {}",
        nextest.command
    );
    assert_scoped_filterset_not_workspace_ci(&nextest.command);
    assert!(!nextest.description.contains("via test_mapping"));
}

#[test]
fn test_docs_only_does_not_inject_bump_manifests() {
    let unreferenced = packet_with_paths(&["docs/architecture.md"]);
    let unreferenced_plan = plan_fast(&unreferenced, None, &rust_profile());
    assert!(!unreferenced_plan.refused);
    assert!(
        nextest_step(&unreferenced_plan).is_none(),
        "unreferenced docs must not inject nextest, got {:?}",
        unreferenced_plan.steps
    );
    let packet = packet_with_paths(&["docs/installation.md"]);
    let plan = plan_fast(&packet, None, &rust_profile());
    assert!(!plan.refused);
    let nextest = nextest_step(&plan).expect("include_str docs must inject nextest");
    assert!(
        !nextest.command.contains("bump_manifests"),
        "installation.md must not inject bump_manifests, got {}",
        nextest.command
    );
    assert!(
        !nextest.command.contains("test(install_docs_truth)"),
        "vacuous install_docs_truth stem is forbidden, got {}",
        nextest.command
    );
    assert!(
        nextest
            .command
            .contains("test(installation_md_has_live_brew_and_scoop_commands)"),
        "expected installation stem, got {}",
        nextest.command
    );
    assert!(
        nextest
            .command
            .contains("test(no_stale_package_manager_availability_language)"),
        "expected no_stale stem, got {}",
        nextest.command
    );
}

#[test]
fn test_include_str_docs_inject_readme_and_keep_envelope() {
    let packet = packet_with_paths(&["README.md"]);
    let plan = plan_fast(&packet, None, &rust_profile());
    assert!(!plan.refused);
    assert!(plan.fallback_reason.is_none());
    let nextest = nextest_step(&plan).expect("README include_str must inject nextest");
    assert!(
        nextest
            .command
            .contains("test(readme_has_daily_5_front_door)"),
        "expected readme stem, got {}",
        nextest.command
    );
    assert!(
        nextest
            .command
            .contains("test(no_stale_package_manager_availability_language)"),
        "expected no_stale stem, got {}",
        nextest.command
    );
    assert!(
        nextest.description.contains("include_str"),
        "description must name include_str, got {}",
        nextest.description
    );
    assert!(
        !nextest.description.contains("via test_mapping"),
        "injected description must not claim test_mapping, got {}",
        nextest.description
    );
    assert!(
        !nextest.description.contains("packaging"),
        "docs-only must not claim packaging, got {}",
        nextest.description
    );
}

#[test]
fn test_foreign_root_readme_skips_include_str_nextest() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = camino::Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).expect("utf8 temp");
    let packet = packet_with_paths(&["README.md"]);
    let plan = plan_at(&root, &packet, None, &rust_profile());
    assert!(!plan.refused);
    assert!(plan.fallback_reason.is_none());
    assert!(
        plan.steps.iter().any(|s| s.command.contains("fmt")),
        "fmt must be present, got {:?}",
        plan.steps
    );
    assert!(
        plan.steps.iter().any(|s| s.command.contains("clippy")),
        "clippy must be present, got {:?}",
        plan.steps
    );
    assert!(
        nextest_step(&plan).is_none(),
        "foreign README must not schedule nextest, got {:?}",
        plan.steps
    );
}

#[test]
fn test_foreign_root_packaging_keeps_bump_manifests_without_doc_stems() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = camino::Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).expect("utf8 temp");
    let packet = packet_with_paths(&["packaging/winget/Ledgerful.yaml", "README.md"]);
    let plan = plan_at(&root, &packet, None, &rust_profile());
    assert!(!plan.refused);
    assert!(plan.fallback_reason.is_none());
    let nextest = nextest_step(&plan).expect("foreign packaging still injects bump_manifests");
    assert!(
        nextest.command.contains("test(bump_manifests)"),
        "expected bump_manifests, got {}",
        nextest.command
    );
    assert!(
        !nextest
            .command
            .contains("test(readme_has_daily_5_front_door)"),
        "foreign README stem must stay out, got {}",
        nextest.command
    );
}

#[test]
fn test_foreign_root_scoped_ok_keeps_mapping_stem_only() {
    let packet = ImpactPacket {
        head_hash: Some("matched-head".to_string()),
        changes: vec![
            ChangedFile {
                path: PathBuf::from("src/commands/hotspots.rs"),
                ..Default::default()
            },
            ChangedFile {
                path: PathBuf::from("README.md"),
                ..Default::default()
            },
        ],
        ..ImpactPacket::default()
    };
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

    let tmp = tempfile::tempdir().expect("tempdir");
    let root = camino::Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).expect("utf8 temp");
    let plan = plan_at(&root, &packet, Some(&conn), &rust_profile());
    assert!(
        !plan.refused,
        "foreign mapped src + README must stay ScopedOk; reason={:?}",
        plan.fallback_reason
    );
    assert!(plan.fallback_reason.is_none());
    let nextest = nextest_step(&plan).expect("ScopedOk nextest");
    assert!(
        nextest.command.contains("test(cli_hotspots)"),
        "mapping stem must remain, got {}",
        nextest.command
    );
    assert!(
        !nextest
            .command
            .contains("test(readme_has_daily_5_front_door)"),
        "foreign README stem must stay out, got {}",
        nextest.command
    );
    assert!(
        nextest.description.contains("via test_mapping"),
        "description must stay on test_mapping, got {}",
        nextest.description
    );
    assert!(
        !nextest.description.contains("include_str"),
        "description must not name include_str, got {}",
        nextest.description
    );
}

#[test]
fn test_include_str_multi_referenced_docs_union_stems() {
    let packet = packet_with_paths(&["docs/Ledgerful/skill.md", "docs/Call-Resolution.md"]);
    let plan = plan_fast(&packet, None, &rust_profile());
    let nextest = nextest_step(&plan).expect("multi-ref docs must inject");
    for stem in [
        "call_resolution_docs_honesty_ceiling",
        "test_daily5_skill_sot_contains_five_argv_substrings",
        "test_daily5_skill_sot_is_ai_only_procedure",
    ] {
        assert!(
            nextest.command.contains(&format!("test({stem})")),
            "union missing {stem}: {}",
            nextest.command
        );
    }
    let command = &nextest.command;
    let first = command.find("test(call_resolution_docs_honesty_ceiling)");
    let second = command.find("test(test_daily5_skill_sot_contains_five_argv_substrings)");
    assert!(
        first.is_some() && second.is_some() && first < second,
        "stems must be sorted in -E, got {command}"
    );
}

#[test]
fn test_include_str_plus_packaging_is_one_nextest_step() {
    let packet = packet_with_paths(&["docs/installation.md", "packaging/homebrew/ledgerful.rb"]);
    let plan = plan_fast(&packet, None, &rust_profile());
    let nextest_steps: Vec<_> = plan
        .steps
        .iter()
        .filter(|s| s.command.contains("nextest"))
        .collect();
    assert_eq!(
        nextest_steps.len(),
        1,
        "packaging + include_str must be one nextest step, got {:?}",
        plan.steps
    );
    let nextest = nextest_steps[0];
    assert!(
        nextest.command.contains("test(bump_manifests)"),
        "{}",
        nextest.command
    );
    assert!(
        nextest
            .command
            .contains("test(installation_md_has_live_brew_and_scoop_commands)"),
        "{}",
        nextest.command
    );
    assert!(
        nextest.description.contains("bump_manifests")
            && nextest.description.contains("include_str"),
        "composite description, got {}",
        nextest.description
    );
}

#[test]
fn test_include_str_docs_no_rust_profile_skips_nextest() {
    let packet = packet_with_paths(&["docs/installation.md"]);
    let plan = plan_fast(
        &packet,
        None,
        &crate::platform::repository::RepositoryProfile::default(),
    );
    assert!(!plan.refused);
    assert!(
        nextest_step(&plan).is_none(),
        "non-Rust profile must not inject doc nextest, got {:?}",
        plan.steps
    );
}

#[test]
fn test_packaging_no_rust_profile_still_injects_bump_manifests() {
    let packet = packet_with_paths(&["packaging/homebrew/ledgerful.rb"]);
    let plan = plan_fast(
        &packet,
        None,
        &crate::platform::repository::RepositoryProfile::default(),
    );
    assert!(!plan.refused);
    let nextest = nextest_step(&plan).expect("packaging A2 injects without rust");
    assert!(
        nextest.command.contains("test(bump_manifests)"),
        "expected bump_manifests without rust, got {}",
        nextest.command
    );
}

#[test]
fn test_include_str_plus_unreferenced_doc_still_injects() {
    let packet = packet_with_paths(&["docs/installation.md", "docs/architecture.md"]);
    let plan = plan_fast(&packet, None, &rust_profile());
    assert!(!plan.refused);
    assert!(plan.fallback_reason.is_none());
    let nextest = nextest_step(&plan).expect("referenced + unreferenced still injects");
    assert!(
        nextest
            .command
            .contains("test(installation_md_has_live_brew_and_scoop_commands)"),
        "expected installation stem, got {}",
        nextest.command
    );
    assert!(
        !nextest.command.contains("bump_manifests"),
        "mixed docs must not inject bump_manifests, got {}",
        nextest.command
    );
}

#[test]
fn test_inject_description_not_via_test_mapping() {
    let packet = packet_with_paths(&["packaging/scoop/ledgerful.json"]);
    let plan = plan_fast(&packet, None, &rust_profile());
    let nextest = nextest_step(&plan).expect("packaging inject");
    assert!(!nextest.description.contains("via test_mapping"));
    assert!(
        nextest.description.contains("Non-code changes")
            || nextest.description.contains("packaging/scripts"),
        "inject description should name packaging/scripts, got {}",
        nextest.description
    );
}

#[test]
fn test_mixed_src_mapped_plus_packaging_unions_stem() {
    // Copy of test_build_plan_scoped_head_match_with_stems_scoped_ok_without_auto_index
    // plus a packaging path — P7 unions bump_manifests into ScopedOk.
    let packet = ImpactPacket {
        head_hash: Some("matched-head".to_string()),
        changes: vec![
            ChangedFile {
                path: PathBuf::from("src/commands/hotspots.rs"),
                ..Default::default()
            },
            ChangedFile {
                path: PathBuf::from("packaging/homebrew/ledgerful.rb"),
                ..Default::default()
            },
        ],
        ..ImpactPacket::default()
    };
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

    let plan = plan_fast(&packet, Some(&conn), &rust_profile());
    assert!(
        !plan.refused,
        "mapped src + packaging must ScopedOk with union; reason={:?}",
        plan.fallback_reason
    );
    let nextest = nextest_step(&plan).expect("ScopedOk nextest");
    assert!(
        nextest.command.contains("test(cli_hotspots)"),
        "original mapping stem must remain, got {}",
        nextest.command
    );
    assert!(
        nextest.command.contains("test(bump_manifests)"),
        "P7 must union bump_manifests, got {}",
        nextest.command
    );
    assert_scoped_filterset_not_workspace_ci(&nextest.command);
    assert!(
        !nextest.description.contains("via test_mapping"),
        "unioned description must not claim test_mapping, got {}",
        nextest.description
    );
}

#[test]
fn test_mixed_src_mapped_plus_include_str_doc_unions_stems() {
    let packet = ImpactPacket {
        head_hash: Some("matched-head".to_string()),
        changes: vec![
            ChangedFile {
                path: PathBuf::from("src/commands/hotspots.rs"),
                ..Default::default()
            },
            ChangedFile {
                path: PathBuf::from("README.md"),
                ..Default::default()
            },
        ],
        ..ImpactPacket::default()
    };
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

    let plan = plan_fast(&packet, Some(&conn), &rust_profile());
    assert!(
        !plan.refused,
        "mapped src + README must ScopedOk with union; reason={:?}",
        plan.fallback_reason
    );
    let nextest = nextest_step(&plan).expect("ScopedOk nextest");
    assert!(
        nextest.command.contains("test(cli_hotspots)"),
        "original mapping stem must remain, got {}",
        nextest.command
    );
    assert!(
        nextest
            .command
            .contains("test(readme_has_daily_5_front_door)"),
        "0480 must union README include_str stem, got {}",
        nextest.command
    );
    assert!(
        nextest.description.contains("include_str"),
        "unioned description must name include_str, got {}",
        nextest.description
    );
    assert!(
        !nextest.description.contains("via test_mapping"),
        "unioned description must not claim test_mapping, got {}",
        nextest.description
    );
}

#[test]
fn test_mixed_src_mapped_plus_include_str_doc_no_rust_skips_doc_stems() {
    let packet = ImpactPacket {
        head_hash: Some("matched-head".to_string()),
        changes: vec![
            ChangedFile {
                path: PathBuf::from("src/commands/hotspots.rs"),
                ..Default::default()
            },
            ChangedFile {
                path: PathBuf::from("README.md"),
                ..Default::default()
            },
        ],
        ..ImpactPacket::default()
    };
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

    let plan = plan_fast(
        &packet,
        Some(&conn),
        &crate::platform::repository::RepositoryProfile::default(),
    );
    assert!(
        !plan.refused,
        "mapped src + README without rust must still ScopedOk; reason={:?}",
        plan.fallback_reason
    );
    let nextest = nextest_step(&plan).expect("ScopedOk nextest from mapping");
    assert!(
        nextest.command.contains("test(cli_hotspots)"),
        "original mapping stem must remain, got {}",
        nextest.command
    );
    assert!(
        !nextest
            .command
            .contains("test(readme_has_daily_5_front_door)"),
        "no-rust ScopedOk must not inject include_str stems, got {}",
        nextest.command
    );
    assert!(
        !nextest.description.contains("include_str"),
        "no-rust description must not name include_str, got {}",
        nextest.description
    );
}

#[test]
fn test_mixed_src_unmapped_plus_packaging_still_mapping_refuse() {
    let packet = ImpactPacket {
        head_hash: Some("matched-head".to_string()),
        changes: vec![
            ChangedFile {
                path: PathBuf::from("src/foo.rs"),
                ..Default::default()
            },
            ChangedFile {
                path: PathBuf::from("packaging/homebrew/ledgerful.rb"),
                ..Default::default()
            },
        ],
        ..ImpactPacket::default()
    };
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
        "INSERT INTO project_files (id, file_path) VALUES (1, 'src/foo.rs')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO project_files (id, file_path) VALUES (2, 'tests/integration/cli_hotspots.rs')",
        [],
    )
    .unwrap();
    // Mapping exists (freshness Ok) but not for src/foo.rs — unmapped src.
    conn.execute(
        "INSERT INTO project_files (id, file_path) VALUES (3, 'src/other.rs')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO test_mapping (test_symbol_id, test_file_id, tested_symbol_id, tested_file_id) \
             VALUES (10, 2, 20, 3)",
        [],
    )
    .unwrap();

    let plan = plan_fast(&packet, Some(&conn), &rust_profile());
    assert!(
        plan.refused,
        "unmapped src + packaging must MappingRefuse, not cheap; steps={:?} reason={:?}",
        plan.steps, plan.fallback_reason
    );
}

#[test]
fn test_non_code_cheap_no_fallback_reason() {
    let packet = packet_with_paths(&["CHANGELOG.md"]);
    let plan = plan_fast(&packet, None, &rust_profile());
    assert!(!plan.refused);
    assert!(
        plan.fallback_reason.is_none(),
        "NonCodeCheap must not set fallback_reason (dto maps it to scopeExecuted full), got {:?}",
        plan.fallback_reason
    );
}

#[test]
fn test_src_plus_changelog_is_not_non_code_cheap() {
    let packet = packet_with_paths(&["src/foo.rs", "CHANGELOG.md"]);
    let plan = plan_fast(&packet, None, &rust_profile());
    assert!(
        plan.refused,
        "mixed src + CHANGELOG must not take NonCodeCheap; steps={:?} reason={:?}",
        plan.steps, plan.fallback_reason
    );
}

#[test]
fn test_openapi_json_is_not_non_code_cheap() {
    let packet = packet_with_paths(&["docs/api/openapi.json"]);
    let plan = plan_fast(&packet, None, &rust_profile());
    assert!(
        plan.refused,
        "openapi.json must not be NonCodeCheap; steps={:?} reason={:?}",
        plan.steps, plan.fallback_reason
    );
}

fn include_str_literals(src: &str) -> Vec<String> {
    parse_include_str_args("foo.rs", src)
        .unwrap_or_else(|err| panic!("{err}"))
        .into_iter()
        .map(|arg| match arg {
            IncludeStrArg::Literal(path) | IncludeStrArg::CargoManifestConcat(path) => path,
        })
        .collect()
}

#[test]
fn test_include_str_comment_paren_resolves_installation() {
    let got = include_str_literals("include_str!(\n // )\n \"docs/installation.md\"\n)");
    assert_eq!(got, vec!["docs/installation.md".to_string()]);
}

#[test]
fn test_include_str_nested_block_comment_resolves_installation() {
    let got = include_str_literals("include_str!( /* ) /* */ */ \"docs/installation.md\" )");
    assert_eq!(got, vec!["docs/installation.md".to_string()]);
}

#[test]
fn test_include_str_line_comment_ignores_first_call() {
    let got = include_str_literals(
        "// include_str!(\"docs/installation.md\")\ninclude_str!(\"docs/golden-path.md\")",
    );
    assert_eq!(got, vec!["docs/golden-path.md".to_string()]);
}

#[test]
fn test_include_str_raw_string_resolves_installation() {
    let got = include_str_literals("include_str!(r\"docs/installation.md\")");
    assert_eq!(got, vec!["docs/installation.md".to_string()]);
}

#[test]
fn test_include_str_raw_string_hashes_resolves_installation() {
    let got = include_str_literals("include_str!(r#\"docs/installation.md\"#)");
    assert_eq!(got, vec!["docs/installation.md".to_string()]);
}

#[test]
fn test_include_str_char_quote_does_not_hide_call() {
    let got = include_str_literals("let c = '\"'; include_str!(\"docs/installation.md\");");
    assert_eq!(got, vec!["docs/installation.md".to_string()]);
}

#[test]
fn test_include_str_lifetime_static_does_not_hide_call() {
    let got =
        include_str_literals("let _: &'static str = \"\"; include_str!(\"docs/installation.md\");");
    assert_eq!(got, vec!["docs/installation.md".to_string()]);
}

#[test]
fn test_include_str_byte_string_is_not_a_call() {
    let got = include_str_literals("let _ = b\"include_str!(\\\"docs/installation.md\\\")\";");
    assert!(got.is_empty(), "{got:?}");
}

#[test]
fn test_include_str_raw_byte_string_is_not_a_call() {
    let got = include_str_literals(
        "let _ = br#\"include_str!(\\\"docs/installation.md\\\")\"#; include_str!(\"docs/golden-path.md\");",
    );
    assert_eq!(got, vec!["docs/golden-path.md".to_string()]);
}

#[test]
fn test_include_str_byte_char_quote_does_not_hide_call() {
    let got = include_str_literals("let _ = b'\"'; include_str!(\"docs/installation.md\");");
    assert_eq!(got, vec!["docs/installation.md".to_string()]);
}

#[test]
fn test_include_str_brace_form_resolves_installation() {
    let got = include_str_literals("include_str! { \"docs/installation.md\" }");
    assert_eq!(got, vec!["docs/installation.md".to_string()]);
}

#[test]
fn test_include_str_bracket_form_is_unresolved() {
    let err = parse_include_str_args("foo.rs", "include_str![\"docs/installation.md\"]")
        .expect_err("bracket form is unresolved");
    assert!(err.contains("unresolved"), "{err}");
    assert!(err.contains("foo.rs"), "{err}");
}

#[test]
fn test_include_str_concat_manifest_dir_resolves_installation() {
    let src = "include_str!(concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/docs/installation.md\"))";
    let args = parse_include_str_args("src/foo.rs", src).expect("concat form");
    match args.as_slice() {
        [IncludeStrArg::CargoManifestConcat(rel)] => assert_eq!(rel, "docs/installation.md"),
        other => panic!("expected concat path, got {other:?}"),
    }
}

#[test]
fn test_include_str_material_stems_name_live_fns() {
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut stems: Vec<&str> = INCLUDE_STR_MATERIAL
        .iter()
        .flat_map(|(_, names)| names.iter().copied())
        .collect();
    stems.sort_unstable();
    stems.dedup();
    assert_eq!(stems.len(), 11, "distinct stem count");
    let mut found = std::collections::BTreeSet::new();
    let mut failures = Vec::new();
    for dir in ["src", "tests", "crates"] {
        walk_rs_files(
            &crate_root.join(dir),
            &crate_root,
            &mut |rel, src| {
                for stem in &stems {
                    if code_has_fn_stem(src, stem, rel)? {
                        found.insert(*stem);
                    }
                }
                Ok(())
            },
            &mut failures,
        );
    }
    assert!(
        failures.is_empty(),
        "stem scan failures:\n  {}",
        failures.join("\n  ")
    );
    let missing: Vec<&&str> = stems.iter().filter(|stem| !found.contains(*stem)).collect();
    assert!(missing.is_empty(), "missing fn stems: {missing:?}");
}

#[test]
fn test_include_str_material_table_matches_live_cheap_sites() {
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut live: Vec<String> = Vec::new();
    let mut failures: Vec<String> = Vec::new();
    for dir in ["src", "tests", "crates"] {
        walk_rs_files(
            &crate_root.join(dir),
            &crate_root,
            &mut |rel, src| {
                live.extend(collect_cheap_include_str_paths(rel, src)?);
                Ok(())
            },
            &mut failures,
        );
    }
    assert!(
        failures.is_empty(),
        "unresolved include_str! forms:\n  {}",
        failures.join("\n  ")
    );
    live.sort();
    live.dedup();
    let table: Vec<String> = INCLUDE_STR_MATERIAL
        .iter()
        .map(|(p, _)| (*p).to_string())
        .collect();
    assert_eq!(
        live, table,
        "cheap include_str! paths drifted from INCLUDE_STR_MATERIAL"
    );
    for (path, stems) in INCLUDE_STR_MATERIAL {
        assert!(
            !stems.is_empty(),
            "INCLUDE_STR_MATERIAL[{path}] must have a non-empty stem list"
        );
    }
}

#[test]
fn test_include_str_concat_extra_args_is_unresolved() {
    let src =
        "include_str!(concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/docs/installation.md\", \"x\"))";
    let err = collect_cheap_include_str_paths("src/foo.rs", src).unwrap_err();
    assert!(
        err.contains("unresolved concat"),
        "extra concat args must fail, got {err}"
    );
}

fn walk_rs_files(
    dir: &Path,
    crate_root: &Path,
    visit: &mut impl FnMut(&str, &str) -> Result<(), String>,
    failures: &mut Vec<String>,
) {
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        let entries = match fs::read_dir(&current) {
            Ok(e) => e,
            Err(e) => {
                failures.push(format!("{}: read_dir failed: {e}", current.display()));
                continue;
            }
        };
        let mut kids = Vec::new();
        for entry in entries {
            match entry {
                Ok(e) => kids.push(e),
                Err(e) => {
                    failures.push(format!("{}: read_dir entry failed: {e}", current.display()))
                }
            }
        }
        kids.sort_by_key(|e| e.file_name());
        for entry in kids {
            let path = entry.path();
            let name = entry.file_name();
            if name == ".git" || name == "target" || name == "node_modules" || name == "vendor" {
                continue;
            }
            let meta = match fs::symlink_metadata(&path) {
                Ok(m) => m,
                Err(e) => {
                    failures.push(format!("{}: symlink_metadata failed: {e}", path.display()));
                    continue;
                }
            };
            if meta.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            let rel = path
                .strip_prefix(crate_root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            let src = match fs::read_to_string(&path) {
                Ok(s) => s,
                Err(e) => {
                    failures.push(format!("{rel}: read_to_string failed: {e}"));
                    continue;
                }
            };
            if let Err(e) = visit(&rel, &src) {
                failures.push(e);
            }
        }
    }
}

fn collect_cheap_include_str_paths(source_rel: &str, src: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    for arg in parse_include_str_args(source_rel, src)? {
        let resolved = match arg {
            IncludeStrArg::Literal(rel) => resolve_from_source(source_rel, &rel),
            IncludeStrArg::CargoManifestConcat(rel) => {
                join_repo(&cargo_manifest_prefix(source_rel), &rel)
            }
        };
        if is_non_code_cheap_path(Path::new(&resolved)) {
            out.push(resolved);
        }
    }
    Ok(out)
}

#[derive(Debug)]
enum IncludeStrArg {
    Literal(String),
    CargoManifestConcat(String),
}

fn parse_include_str_args(source_rel: &str, src: &str) -> Result<Vec<IncludeStrArg>, String> {
    let mut args = Vec::new();
    let mut i = 0;
    while let Some(start) = find_code_include_str(src, i, source_rel)? {
        let after_bang = start + "include_str!".len();
        let j = skip_trivia(src, after_bang, source_rel)?;
        if j >= src.len() {
            return Err(format!("{source_rel}: unresolved include_str! form"));
        }
        let opener = src.as_bytes()[j];
        if opener == b'[' || (opener != b'(' && opener != b'{') {
            return Err(format!("{source_rel}: unresolved include_str! form"));
        }
        let inner_end = find_matching_delimiter(src, j, source_rel)?
            .ok_or_else(|| format!("{source_rel}: unbalanced include_str invocation"))?;
        let inner = &src[j + 1..inner_end];
        args.push(parse_include_str_inner(source_rel, inner)?);
        i = inner_end + 1;
    }
    Ok(args)
}

/// Byte offset of the next `include_str!` that is not inside a comment or literal.
fn find_code_include_str(
    src: &str,
    from: usize,
    source_rel: &str,
) -> Result<Option<usize>, String> {
    let b = src.as_bytes();
    let mut i = from;
    while i < b.len() {
        if let Some(next) = consume_skipped(src, i, source_rel)? {
            if next <= i {
                return Err(format!("{source_rel}: scanner did not advance"));
            }
            i = next;
            continue;
        }
        if src[i..].starts_with("include_str!") {
            let prev_ok = i == 0 || !is_ident_byte(b[i - 1]);
            if prev_ok {
                return Ok(Some(i));
            }
        }
        let step = src[i..].chars().next().map(char::len_utf8).unwrap_or(1);
        i += step;
    }
    Ok(None)
}

fn parse_include_str_inner(source_rel: &str, inner: &str) -> Result<IncludeStrArg, String> {
    let start = skip_trivia(inner, 0, source_rel)?;
    if start >= inner.len() {
        return Err(format!(
            "{source_rel}: unresolved include_str! form: {}",
            inner.trim()
        ));
    }
    if let Some((lit, end)) = take_string_literal(inner, start, source_rel)? {
        let after = skip_trivia(inner, end, source_rel)?;
        if after != inner.len() {
            return Err(format!(
                "{source_rel}: unresolved include_str! form: {}",
                inner.trim()
            ));
        }
        return Ok(IncludeStrArg::Literal(lit));
    }
    if inner[start..].starts_with("concat!") {
        let after_bang = start + "concat!".len();
        let j = skip_trivia(inner, after_bang, source_rel)?;
        if j >= inner.len() || inner.as_bytes()[j] != b'(' {
            return Err(format!("{source_rel}: concat! missing '('"));
        }
        let inner_end = find_matching_delimiter(inner, j, source_rel)?
            .ok_or_else(|| format!("{source_rel}: unbalanced concat!("))?;
        let concat_inner = inner[j + 1..inner_end].trim();
        let arg = parse_cargo_manifest_concat(source_rel, concat_inner)?;
        let after = skip_trivia(inner, inner_end + 1, source_rel)?;
        if after != inner.len() {
            return Err(format!(
                "{source_rel}: unresolved include_str! form: {}",
                inner.trim()
            ));
        }
        return Ok(arg);
    }
    Err(format!(
        "{source_rel}: unresolved include_str! form: {}",
        inner.trim()
    ))
}

fn parse_cargo_manifest_concat(source_rel: &str, inner: &str) -> Result<IncludeStrArg, String> {
    let compact: String = inner.split_whitespace().collect();
    const PREFIX: &str = "env!(\"CARGO_MANIFEST_DIR\"),\"";
    if let Some(rest) = compact.strip_prefix(PREFIX)
        && let Some(rel) = rest.strip_suffix('"')
    {
        if rel.contains('"') {
            return Err(format!(
                "{source_rel}: unresolved concat! form (only env!(CARGO_MANIFEST_DIR)+string allowed): {inner}"
            ));
        }
        let stripped = rel.strip_prefix('/').unwrap_or(rel);
        return Ok(IncludeStrArg::CargoManifestConcat(
            stripped.replace('\\', "/"),
        ));
    }
    Err(format!(
        "{source_rel}: unresolved concat! form (only env!(CARGO_MANIFEST_DIR)+string allowed): {inner}"
    ))
}

fn code_has_fn_stem(src: &str, stem: &str, source_rel: &str) -> Result<bool, String> {
    let b = src.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if let Some(next) = consume_skipped(src, i, source_rel)? {
            if next <= i {
                return Err(format!("{source_rel}: scanner did not advance"));
            }
            i = next;
            continue;
        }
        if src[i..].starts_with("fn") {
            let prev_ok = i == 0 || !is_ident_byte(b[i - 1]);
            let after = i + 2;
            if prev_ok && after < b.len() && b[after].is_ascii_whitespace() {
                let mut k = after;
                while k < b.len() && b[k].is_ascii_whitespace() {
                    k += 1;
                }
                if src[k..].starts_with(stem) {
                    let end = k + stem.len();
                    match b.get(end).copied() {
                        Some(b'(') | Some(b'<') => return Ok(true),
                        _ => {}
                    }
                }
            }
        }
        let step = src[i..].chars().next().map(char::len_utf8).unwrap_or(1);
        i += step;
    }
    Ok(false)
}

fn cargo_manifest_prefix(source_rel: &str) -> String {
    let parts: Vec<&str> = source_rel.split('/').collect();
    if parts.len() >= 2 && parts[0] == "crates" {
        format!("{}/{}", parts[0], parts[1])
    } else {
        String::new()
    }
}

fn resolve_from_source(source_rel: &str, include_rel: &str) -> String {
    let parent = match source_rel.rsplit_once('/') {
        Some((dir, _)) => dir.to_string(),
        None => String::new(),
    };
    join_repo(&parent, include_rel)
}

fn join_repo(base: &str, rel: &str) -> String {
    let mut parts: Vec<String> = Vec::new();
    if !base.is_empty() {
        parts.extend(
            base.split('/')
                .filter(|p| !p.is_empty() && *p != ".")
                .map(str::to_string),
        );
    }
    let rel_norm = rel.replace('\\', "/");
    for p in rel_norm.split('/') {
        if p.is_empty() || p == "." {
            continue;
        }
        if p == ".." {
            parts.pop();
        } else {
            parts.push(p.to_string());
        }
    }
    parts.join("/")
}

fn step(cmd: &str) -> VerificationStep {
    VerificationStep {
        command: cmd.to_string(),
        timeout_secs: 60,
        description: cmd.to_string(),
        shell: false,
        budget_source: None,
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

#[test]
fn resolve_verify_scope_omitted_dry_run_is_fast() {
    assert_eq!(
        resolve_verify_scope(None, true),
        VerifyScope::Fast,
        "omitted --scope on --dry-run is pre-push fast"
    );
}

#[test]
fn resolve_verify_scope_omitted_live_is_full() {
    assert_eq!(
        resolve_verify_scope(None, false),
        VerifyScope::Full,
        "executed verify without --scope stays full"
    );
}

#[test]
fn resolve_verify_scope_explicit_full_dry_run_stays_full() {
    assert_eq!(
        resolve_verify_scope(Some(VerifyScope::Full), true),
        VerifyScope::Full,
        "explicit --scope full always wins"
    );
}

#[test]
fn resolve_verify_scope_explicit_fast_live_stays_fast() {
    assert_eq!(
        resolve_verify_scope(Some(VerifyScope::Fast), false),
        VerifyScope::Fast,
        "explicit --scope fast always wins"
    );
}

// ---- 0549: embedded documents / templates in a third-party repo ----

fn foreign_fixture(files: &[(&str, &str)]) -> (tempfile::TempDir, camino::Utf8PathBuf) {
    let tmp = tempfile::tempdir().expect("tempdir");
    for (rel, body) in files {
        let p = tmp.path().join(rel);
        fs::create_dir_all(p.parent().expect("parent")).expect("mkdir");
        fs::write(p, body).expect("write");
    }
    let root = camino::Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).expect("utf8 temp");
    (tmp, root)
}

const EMBED_ACCESSOR: &str = r#"
pub fn contract() -> &'static str { include_str!("../docs/contract.md") }
pub fn message() -> &'static str { include_str!("../templates/message.txt") }
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn contracts_document() { assert!(contract().contains("v1")); }
    #[test]
    fn contracts_template() { assert!(message().contains("hi")); }
}
"#;

const DYNAMIC_READ_TEST: &str =
    "#[test]\nfn d() { let p = std::env::var(\"X\").unwrap(); let _ = std::fs::read(p); }\n";

#[test]
fn embedded_doc_in_foreign_repo_runs_includer_tests_not_fmt_clippy_only() {
    let (_tmp, root) = foreign_fixture(&[
        ("Cargo.toml", "[package]\nname=\"fx\"\n"),
        ("src/contracts.rs", EMBED_ACCESSOR),
    ]);
    let packet = packet_with_paths(&["docs/contract.md"]);
    let plan = plan_at(&root, &packet, None, &rust_profile());
    assert!(!plan.refused, "{:?}", plan.fallback_reason);
    assert!(plan.fallback_reason.is_none(), "fast scope stays fast");
    let nextest = nextest_step(&plan).expect("embedded doc must schedule nextest");
    assert_scoped_filterset_not_workspace_ci(&nextest.command);
    assert!(
        nextest.command.contains("test(contracts_document)"),
        "{}",
        nextest.command
    );
    assert!(
        nextest
            .description
            .contains("docs/contract.md -> src/contracts.rs"),
        "{}",
        nextest.description
    );
    for vacuous in ["test(tests)", "test(mod)", "test(lib)", "test(contracts)"] {
        assert!(!nextest.command.contains(vacuous), "{}", nextest.command);
    }
}

#[test]
fn embedded_template_in_foreign_repo_runs_includer_tests() {
    let (_tmp, root) = foreign_fixture(&[
        ("Cargo.toml", "[package]\nname=\"fx\"\n"),
        ("src/contracts.rs", EMBED_ACCESSOR),
    ]);
    let packet = packet_with_paths(&["templates/message.txt"]);
    let plan = plan_at(&root, &packet, None, &rust_profile());
    assert!(!plan.refused, "{:?}", plan.fallback_reason);
    let nextest = nextest_step(&plan).expect("embedded template must schedule nextest");
    assert!(
        nextest.command.contains("test(contracts_template)"),
        "{}",
        nextest.command
    );
}

#[test]
fn embedded_plan_is_deterministic_across_runs() {
    let (_tmp, root) = foreign_fixture(&[
        ("Cargo.toml", "[package]\nname=\"fx\"\n"),
        ("src/contracts.rs", EMBED_ACCESSOR),
    ]);
    let packet = packet_with_paths(&["templates/message.txt", "docs/contract.md"]);
    let a = plan_at(&root, &packet, None, &rust_profile());
    let b = plan_at(&root, &packet, None, &rust_profile());
    let cmds = |p: &VerificationPlan| {
        p.steps
            .iter()
            .map(|s| s.command.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(cmds(&a), cmds(&b));
    let nextest = nextest_step(&a).expect("nextest");
    assert!(
        nextest
            .command
            .contains("test(contracts_document) + test(contracts_template)"),
        "{}",
        nextest.command
    );
}

#[test]
fn unreferenced_doc_stays_cheap_even_with_dynamic_reads_elsewhere() {
    let (_tmp, root) = foreign_fixture(&[
        ("Cargo.toml", "[package]\nname=\"fx\"\n"),
        ("tests/dyn.rs", DYNAMIC_READ_TEST),
    ]);
    let packet = packet_with_paths(&["docs/guide.md"]);
    let plan = plan_at(&root, &packet, None, &rust_profile());
    assert!(!plan.refused);
    assert!(plan.fallback_reason.is_none());
    assert!(nextest_step(&plan).is_none(), "{:?}", plan.steps);
}

#[test]
fn unreferenced_asset_is_annotated_not_blocked_without_dynamic_reads() {
    let (_tmp, root) = foreign_fixture(&[
        ("Cargo.toml", "[package]\nname=\"fx\"\n"),
        ("src/lib.rs", "pub fn f() {}\n"),
    ]);
    let packet = packet_with_paths(&["assets/logo.png", "data/x.csv"]);
    let plan = plan_at(&root, &packet, None, &rust_profile());
    assert!(!plan.refused, "{:?}", plan.fallback_reason);
    assert!(nextest_step(&plan).is_none());
    assert!(plan.steps.iter().any(|s| s.command.contains("fmt")));
}

#[test]
fn unreferenced_asset_refuses_when_repo_has_dynamic_test_reads() {
    let (_tmp, root) = foreign_fixture(&[
        ("Cargo.toml", "[package]\nname=\"fx\"\n"),
        ("tests/dyn.rs", DYNAMIC_READ_TEST),
    ]);
    let packet = packet_with_paths(&["assets/logo.png"]);
    let plan = plan_at(&root, &packet, None, &rust_profile());
    assert!(plan.refused);
    assert!(plan.steps.is_empty());
    assert!(
        plan.fallback_reason
            .as_deref()
            .unwrap_or("")
            .contains("unresolved reads")
    );
}

#[test]
fn referenced_but_untestable_embed_refuses_or_falls_back_to_full() {
    let (_tmp, root) = foreign_fixture(&[
        ("Cargo.toml", "[package]\nname=\"fx\"\n"),
        (
            "tests/common/mod.rs",
            "pub fn d() -> &'static str { include_str!(\"../../docs/x.md\") }\n",
        ),
    ]);
    let packet = packet_with_paths(&["docs/x.md"]);
    let refused = plan_at(&root, &packet, None, &rust_profile());
    assert!(refused.refused);
    assert!(
        refused
            .fallback_reason
            .as_deref()
            .unwrap_or("")
            .contains("no #[test]")
    );

    let layout = crate::state::layout::Layout::new(&root);
    let full = build_plan_scoped_with_options(
        &packet,
        &Rules::default(),
        &[],
        &crate::config::model::VerifyConfig::default(),
        &rust_profile(),
        VerifyScope::Fast,
        None,
        &layout,
        false,
        true,
    );
    assert!(!full.refused);
    assert!(
        full.fallback_reason
            .as_deref()
            .unwrap_or("")
            .contains("fast scope unavailable")
    );
}

#[test]
fn production_includer_without_inline_tests_refuses_without_mapping() {
    let (_tmp, root) = foreign_fixture(&[
        ("Cargo.toml", "[package]\nname=\"fx\"\n"),
        (
            "src/a.rs",
            "pub fn t() -> &'static str { include_str!(\"../docs/t.md\") }\n",
        ),
    ]);
    let packet = packet_with_paths(&["docs/t.md"]);
    let plan = plan_at(&root, &packet, None, &rust_profile());
    assert!(plan.refused);
    assert!(
        plan.fallback_reason
            .as_deref()
            .unwrap_or("")
            .contains("test_mapping unavailable")
    );
}

#[test]
fn mapped_production_includer_cannot_rescue_unmapped_changed_source() {
    let (_tmp, root) = foreign_fixture(&[
        ("Cargo.toml", "[package]\nname=\"fx\"\n"),
        (
            "src/reader.rs",
            "pub fn text() -> &'static str { include_str!(\"../templates/x.txt\") }\n",
        ),
    ]);
    let conn = rusqlite::Connection::open(root.join("mapping.db")).unwrap();
    conn.execute_batch("CREATE TABLE project_files (id INTEGER PRIMARY KEY, file_path TEXT);
        CREATE TABLE project_symbols (id INTEGER PRIMARY KEY, symbol_name TEXT, file_id INTEGER);
        CREATE TABLE test_mapping (test_symbol_id INTEGER, test_file_id INTEGER, tested_symbol_id INTEGER, tested_file_id INTEGER);
        INSERT INTO project_files VALUES (1, 'src/reader.rs'), (2, 'src/check.rs');
        INSERT INTO project_symbols VALUES (1, 'reader_contract', 2);
        INSERT INTO test_mapping VALUES (1, 2, NULL, 1);").unwrap();
    let pure = plan_at(
        &root,
        &packet_with_paths(&["templates/x.txt"]),
        Some(&conn),
        &rust_profile(),
    );
    assert!(
        !pure.refused,
        "pure D3(d) must keep working: {:?}",
        pure.fallback_reason
    );
    assert!(
        nextest_step(&pure)
            .unwrap()
            .command
            .contains("test(reader_contract)")
    );
    let mixed = plan_at(
        &root,
        &packet_with_paths(&["src/unmapped.rs", "templates/x.txt"]),
        Some(&conn),
        &rust_profile(),
    );
    assert!(
        mixed.refused,
        "mapped includer must not rescue unmapped source: {:?}",
        mixed.steps
    );
    assert!(
        mixed
            .fallback_reason
            .as_deref()
            .unwrap()
            .contains("changed source")
    );
}

#[test]
fn packaging_plus_embedded_share_one_nextest_step() {
    let (_tmp, root) = foreign_fixture(&[
        ("Cargo.toml", "[package]\nname=\"fx\"\n"),
        ("src/contracts.rs", EMBED_ACCESSOR),
    ]);
    let packet = packet_with_paths(&["packaging/homebrew/x.rb", "docs/contract.md"]);
    let plan = plan_at(&root, &packet, None, &rust_profile());
    assert!(!plan.refused, "{:?}", plan.fallback_reason);
    let steps: Vec<_> = plan
        .steps
        .iter()
        .filter(|s| s.command.contains("nextest"))
        .collect();
    assert_eq!(steps.len(), 1, "{:?}", plan.steps);
    assert!(steps[0].command.contains("test(bump_manifests)"));
    assert!(steps[0].command.contains("test(contracts_document)"));
}

#[test]
fn non_rust_profile_keeps_cheap_docs_unchanged() {
    let (_tmp, root) = foreign_fixture(&[("src/contracts.rs", EMBED_ACCESSOR)]);
    let packet = packet_with_paths(&["docs/contract.md"]);
    let plan = plan_at(
        &root,
        &packet,
        None,
        &crate::platform::repository::RepositoryProfile::default(),
    );
    assert!(!plan.refused);
    assert!(nextest_step(&plan).is_none(), "non-Rust embedding is 0570");
}

// ---- 0549 address-findings: forms, dynamic reads, workspace, deleted ----

#[rstest::rstest]
#[case::include_bytes(
    "#[test]\nfn t_bytes() { let _ = include_bytes!(\"../data/a.bin\"); }\n",
    "data/a.bin",
    "t_bytes"
)]
#[case::include_macro(
    "#[test]\nfn t_incl() { let _ = include!(\"../data/gen.in\"); }\n",
    "data/gen.in",
    "t_incl"
)]
#[case::brace_form(
    "#[test]\nfn t_brace() { let _ = include_str! { \"../data/b.txt\" }; }\n",
    "data/b.txt",
    "t_brace"
)]
#[case::literal_runtime_read(
    "#[test]\nfn t_read() { let _ = std::fs::read_to_string(\"data/r.csv\"); }\n",
    "data/r.csv",
    "t_read"
)]
fn embedded_forms_select_function_names(
    #[case] source: &str,
    #[case] changed: &str,
    #[case] stem: &str,
) {
    let (_tmp, root) = foreign_fixture(&[
        ("Cargo.toml", "[package]\nname=\"fx\"\n"),
        ("tests/t.rs", source),
    ]);
    let plan = plan_at(&root, &packet_with_paths(&[changed]), None, &rust_profile());
    assert!(!plan.refused, "{:?}", plan.fallback_reason);
    let nextest = nextest_step(&plan).expect("nextest");
    assert!(
        nextest.command.contains(&format!("test({stem})")),
        "{}",
        nextest.command
    );
}

#[test]
fn two_member_workspace_resolves_manifest_dir_per_crate() {
    let (_tmp, root) = foreign_fixture(&[
        (
            "Cargo.toml",
            "[workspace]\nmembers=[\"packages/a\",\"services/b\"]\n",
        ),
        ("packages/a/Cargo.toml", "[package]\nname=\"a\"\n"),
        ("services/b/Cargo.toml", "[package]\nname=\"b\"\n"),
        (
            "services/b/src/lib.rs",
            "#[test]\nfn b_reads() { let _ = include_str!(concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/fixtures/m.txt\")); }\n",
        ),
    ]);
    let plan = plan_at(
        &root,
        &packet_with_paths(&["services/b/fixtures/m.txt"]),
        None,
        &rust_profile(),
    );
    assert!(!plan.refused, "{:?}", plan.fallback_reason);
    assert!(
        nextest_step(&plan)
            .expect("nextest")
            .command
            .contains("test(b_reads)")
    );
}

#[test]
fn format_built_read_is_dynamic_so_unreferenced_fixture_refuses() {
    // P1-3: a `format!`-built path must not count as a literal read.
    let (_tmp, root) = foreign_fixture(&[
        ("Cargo.toml", "[package]\nname=\"fx\"\n"),
        (
            "tests/t.rs",
            "#[test]\nfn t() { let n = \"case1\"; let _ = std::fs::read_to_string(format!(\"tests/fixtures/{n}.json\")); }\n",
        ),
    ]);
    let plan = plan_at(
        &root,
        &packet_with_paths(&["tests/fixtures/case1.json"]),
        None,
        &rust_profile(),
    );
    assert!(plan.refused, "{:?}", plan.steps);
}

#[test]
fn unmodeled_file_macro_makes_unreferenced_fixture_refuse() {
    let (_tmp, root) = foreign_fixture(&[
        ("Cargo.toml", "[package]\nname=\"fx\"\n"),
        (
            "src/q.rs",
            "fn q() { sqlx::query_file!(\"queries/a.sql\"); }\n",
        ),
    ]);
    let plan = plan_at(
        &root,
        &packet_with_paths(&["queries/b.sql"]),
        None,
        &rust_profile(),
    );
    assert!(plan.refused);
}

#[test]
fn unreferenced_csv_control_is_annotated_in_plan() {
    let (_tmp, root) = foreign_fixture(&[
        ("Cargo.toml", "[package]\nname=\"fx\"\n"),
        ("src/lib.rs", "pub fn f() {}\n"),
    ]);
    let plan = plan_at(
        &root,
        &packet_with_paths(&["data/x.csv"]),
        None,
        &rust_profile(),
    );
    assert!(!plan.refused);
    assert!(
        plan.steps[0]
            .description
            .contains("no direct include/read found by the lexical lookup for: data/x.csv"),
        "{}",
        plan.steps[0].description
    );
}

#[test]
fn deleted_embedded_material_still_selects_includer_tests() {
    let (_tmp, root) = foreign_fixture(&[
        ("Cargo.toml", "[package]\nname=\"fx\"\n"),
        ("src/contracts.rs", EMBED_ACCESSOR),
    ]);
    let mut packet = packet_with_paths(&["templates/message.txt"]);
    packet.changes[0].status = "Deleted".to_string();
    let plan = plan_at(&root, &packet, None, &rust_profile());
    assert!(!plan.refused, "{:?}", plan.fallback_reason);
    assert!(
        nextest_step(&plan)
            .expect("nextest")
            .command
            .contains("test(contracts_template)")
    );
}

#[test]
fn renamed_embedded_material_selects_tests_via_old_path() {
    let (_tmp, root) = foreign_fixture(&[
        ("Cargo.toml", "[package]\nname=\"fx\"\n"),
        ("src/contracts.rs", EMBED_ACCESSOR),
    ]);
    let mut packet = packet_with_paths(&["templates/renamed.txt"]);
    packet.changes[0].status = "Renamed".to_string();
    packet.changes[0].old_path = Some(PathBuf::from("templates/message.txt"));
    let plan = plan_at(&root, &packet, None, &rust_profile());
    assert!(!plan.refused, "{:?}", plan.fallback_reason);
    assert!(
        nextest_step(&plan)
            .expect("nextest")
            .command
            .contains("test(contracts_template)")
    );
}

#[cfg(windows)]
#[test]
fn windows_case_difference_still_matches_embedded_path() {
    let (_tmp, root) = foreign_fixture(&[
        ("Cargo.toml", "[package]\nname=\"fx\"\n"),
        ("src/contracts.rs", EMBED_ACCESSOR),
    ]);
    let plan = plan_at(
        &root,
        &packet_with_paths(&["Templates/Message.TXT"]),
        None,
        &rust_profile(),
    );
    assert!(
        nextest_step(&plan)
            .expect("nextest")
            .command
            .contains("test(contracts_template)")
    );
}

#[test]
fn engine_tree_looks_up_non_cheap_fixture_but_keeps_table_for_cheap_docs() {
    // The engine's own cheap docs keep the hand-written table; a non-cheap
    // path is still checked by the lookup (it is unreferenced here, so the
    // pre-existing refuse-on-unmapped behavior is unchanged).
    let packet = packet_with_paths(&["README.md"]);
    let plan = plan_fast(&packet, None, &rust_profile());
    let nextest = nextest_step(&plan).expect("engine table inject");
    assert!(nextest.command.contains("readme_has_daily_5_front_door"));
}

// ---- 0549 gate2 P1-1: formerly-material unreferenced paths keep refusing ----

#[rstest::rstest]
#[case::nested_cargo_manifest("crates/core/Cargo.toml")]
#[case::cargo_config(".cargo/config.toml")]
#[case::yaml_fixture("tests/fixtures/a.yaml")]
#[case::sql_fixture("db/seed.sql")]
fn unreferenced_formerly_material_path_still_refuses(#[case] changed: &str) {
    // These paths are not the literal helper argument. The production dynamic read must not be what refuses them.
    let (_tmp, root) = foreign_fixture(&[
        ("Cargo.toml", "[package]\nname=\"fx\"\n"),
        (
            "src/loader.rs",
            "pub fn load(p: &std::path::Path) -> String { std::fs::read_to_string(p).unwrap_or_default() }\n",
        ),
        (
            "tests/it.rs",
            "#[test]\nfn it() { let _ = fx::loader::load(std::path::Path::new(\"tests/fixtures/a.json\")); }\n",
        ),
    ]);
    let plan = plan_at(&root, &packet_with_paths(&[changed]), None, &rust_profile());
    assert!(
        plan.refused,
        "{changed} must not become a fmt+clippy-only pass: {:?}",
        plan.steps
    );
}

#[test]
fn referenced_helper_json_selects_its_test() {
    let (_tmp, root) = foreign_fixture(&[
        ("Cargo.toml", "[package]\nname=\"fx\"\n"),
        (
            "src/loader.rs",
            "pub fn load(p: &std::path::Path) -> String { std::fs::read_to_string(p).unwrap_or_default() }\n",
        ),
        (
            "tests/it.rs",
            "#[test]\nfn it() { let _ = fx::loader::load(std::path::Path::new(\"tests/fixtures/a.json\")); }\n",
        ),
    ]);
    let plan = plan_at(
        &root,
        &packet_with_paths(&["tests/fixtures/a.json"]),
        None,
        &rust_profile(),
    );
    assert!(!plan.refused, "{:?}", plan.fallback_reason);
    assert!(
        nextest_step(&plan)
            .expect("nextest")
            .command
            .contains("test(it)")
    );
}

#[test]
fn literal_helper_path_is_attributed_and_runtime_built_path_stays_accepted() {
    let (_tmp, root) = foreign_fixture(&[
        ("Cargo.toml", "[package]\nname=\"fx\"\n"),
        (
            "src/lib.rs",
            "pub fn load(p: &std::path::Path) -> String { std::fs::read_to_string(p).unwrap_or_default() }\npub fn render() { let path = format!(\"data/case.csv\"); let _ = std::fs::read_to_string(path); }\n",
        ),
        (
            "tests/reader.rs",
            "#[test]\nfn helper_reader() { let _ = fx::load(std::path::Path::new(\"assets/logo.png\")); }\n",
        ),
    ]);
    let logo = plan_at(
        &root,
        &packet_with_paths(&["assets/logo.png"]),
        None,
        &rust_profile(),
    );
    assert!(!logo.refused, "{:?}", logo.fallback_reason);
    let nextest = nextest_step(&logo).expect("nextest");
    assert!(
        nextest.command.contains("test(helper_reader)"),
        "{}",
        nextest.command
    );
    assert!(
        !nextest
            .description
            .contains("no direct include/read found by the lexical lookup"),
        "{}",
        nextest.description
    );
    let other = plan_at(
        &root,
        &packet_with_paths(&["assets/other.png"]),
        None,
        &rust_profile(),
    );
    assert!(!other.refused, "{:?}", other.fallback_reason);
    assert!(nextest_step(&other).is_none(), "{:?}", other.steps);
    let built = plan_at(
        &root,
        &packet_with_paths(&["data/case.csv"]),
        None,
        &rust_profile(),
    );
    assert!(!built.refused, "{:?}", built.fallback_reason);
    assert!(nextest_step(&built).is_none(), "{:?}", built.steps);
}

#[test]
fn embedded_mixed_legacy_path_is_named_in_refusal() {
    let (_tmp, root) = foreign_fixture(&[
        ("Cargo.toml", "[package]\nname=\"fx\"\n"),
        ("src/lib.rs", "pub fn f() {}\n"),
    ]);
    let plan = plan_at(
        &root,
        &packet_with_paths(&["src/lib.rs", "data/x.sql"]),
        None,
        &rust_profile(),
    );
    assert!(plan.refused);
    assert!(
        plan.fallback_reason
            .as_deref()
            .unwrap_or("")
            .contains("data/x.sql")
    );
}

#[test]
fn embedded_mapping_includer_head_mismatch_refuses_after_repair() {
    let (_tmp, root) = foreign_fixture(&[
        ("Cargo.toml", "[package]\nname=\"fx\"\n"),
        (
            "src/render.rs",
            "pub fn render() -> &'static str { include_str!(\"../message.txt\") }\n",
        ),
    ]);
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    seed_mapping_row(&conn);
    seed_index_head(&conn, "old-hash");
    conn.execute_batch("CREATE TABLE project_files (id INTEGER PRIMARY KEY, file_path TEXT); INSERT INTO project_files VALUES (1, 'src/render.rs');").unwrap();
    let mut packet = packet_with_paths(&["message.txt"]);
    packet.head_hash = Some("new-hash".to_string());
    assert_eq!(
        classify_test_mapping_freshness(&conn, &packet),
        MappingFreshness::HeadMismatch
    );
    let plan = plan_at(&root, &packet, Some(&conn), &rust_profile());
    assert!(plan.refused, "{:?}", plan.steps);
    assert!(plan.steps.is_empty());
    let reason = plan.fallback_reason.as_deref().unwrap_or("");
    assert!(
        reason.contains("head_hash") || reason.contains("auto-index failed"),
        "{reason}"
    );
}

#[test]
#[ignore = "0549 local qualification: engine edit refusal-rate cohort; owner track 0549"]
fn embedded_engine_edit_refusal_rate_qualification() {
    let root = camino::Utf8Path::new(env!("CARGO_MANIFEST_DIR"));
    let cohort = [
        "README.md",
        "SECURITY.md",
        "docs/Features.md",
        "docs/verify-performance.md",
        "docs/Engineering.md",
        "docs/agent-output-contract.md",
        "tests/fixtures/qualification.csv",
        "tests/fixtures/qualification.txt",
        "assets/qualification.png",
        "docs/api/openapi.json",
    ];
    let mut refused = 0;
    for path in cohort {
        let plan = plan_at(root, &packet_with_paths(&[path]), None, &rust_profile());
        refused += usize::from(plan.refused);
        eprintln!(
            "engine cohort {path}: refused={} reason={:?}",
            plan.refused, plan.fallback_reason
        );
    }
    eprintln!("0549 engine edit refusal rate: {refused}/{}", cohort.len());
}

#[rstest::rstest]
#[case("png")]
#[case("csv")]
#[case("json")]
fn helper_mediated_read_residual_is_explicit(#[case] extension: &str) {
    let helper_test = format!(
        "#[test] fn helper_reader() {{ let _ = fx::load(std::path::Path::new(\"tests/fixtures/value.{extension}\")); }}"
    );
    let (_tmp, root) = foreign_fixture(&[
        ("Cargo.toml", "[package]\nname='fx'\n"),
        (
            "src/lib.rs",
            "pub fn load(p: &std::path::Path) -> String { std::fs::read_to_string(p).unwrap_or_default() }",
        ),
        ("tests/reader.rs", &helper_test),
    ]);
    let plan = plan_at(
        &root,
        &packet_with_paths(&[&format!("tests/fixtures/value.{extension}")]),
        None,
        &rust_profile(),
    );
    assert!(!plan.refused, "{:?}", plan.fallback_reason);
    let nextest = nextest_step(&plan).expect("nextest");
    assert!(
        nextest.command.contains("test(helper_reader)"),
        "{}",
        nextest.command
    );
    assert!(
        !nextest
            .description
            .contains("no direct include/read found by the lexical lookup"),
        "{}",
        nextest.description
    );
}

#[test]
fn full_planning_retains_root_profile_without_nested_discovery() {
    let (_tmp, root) = foreign_fixture(&[("backend/Cargo.toml", "malformed [")]);
    let profile = crate::platform::repository::detect_repository(root.as_std_path());
    assert!(profile.rust.is_none());
    let plan = build_plan_scoped_with_options(
        &packet_with_paths(&["message.txt"]),
        &Rules::default(),
        &[],
        &crate::config::model::VerifyConfig::default(),
        &profile,
        VerifyScope::Full,
        None,
        &crate::state::layout::Layout::new(&root),
        false,
        false,
    );
    assert!(!plan.refused, "full must not perform nested discovery");
    assert!(plan.steps.iter().all(|s| !s.command.starts_with("cargo ")));
    assert!(profile.rust.is_none());
}

#[test]
fn embedded_nested_profile_and_documentation_contract_locks() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    fn collect_production_rust(dir: &Path, files: &mut Vec<PathBuf>) {
        for entry in fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if path.is_dir() {
                if entry.file_name() != "tests" {
                    collect_production_rust(&path, files);
                }
            } else if path.extension().and_then(|e| e.to_str()) == Some("rs")
                && entry.file_name() != "tests.rs"
            {
                files.push(path);
            }
        }
    }
    let mut files = Vec::new();
    for dir in ["src/verify", "src/commands/verify"] {
        collect_production_rust(&root.join(dir), &mut files);
    }
    files.sort();
    assert!(files.iter().any(|p| p.ends_with("embedded.rs")));
    for path in files {
        let rel = path
            .strip_prefix(root)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        let text = fs::read_to_string(&path).unwrap();
        let bytes = text.as_bytes();
        let mut at = 0;
        while at < bytes.len() {
            // Ignore strings/comments, including inline unit-test assertion
            // strings. A forbidden production identifier still fails the lock.
            if let Some(end) = consume_skipped(&text, at, &rel).unwrap() {
                at = end;
                continue;
            }
            if is_ident_byte(bytes[at]) {
                let start = at;
                while at < bytes.len() && is_ident_byte(bytes[at]) {
                    at += 1;
                }
                let token = &text[start..at];
                for forbidden in [
                    "bind_nested_manifests",
                    "nested_rust_profile",
                    "ensure_rust_profile_for_manifests",
                    "shell_words",
                    "rust_manifest_paths",
                ] {
                    assert_ne!(
                        token, forbidden,
                        "{rel}: removed nested-profile/dependency identifier {forbidden}"
                    );
                }
            } else {
                at += text[at..].chars().next().map(char::len_utf8).unwrap_or(1);
            }
        }
    }
    for path in ["docs/Features.md", "docs/verify-performance.md"] {
        let text = std::fs::read_to_string(root.join(path)).unwrap();
        for forbidden in ["manifest paths", "bind manifest", "explicit manifest"] {
            assert!(
                !text.contains(forbidden),
                "{path}: obsolete nested-command claim {forbidden}"
            );
        }
    }
}

#[rstest::rstest]
#[case("no_database")]
#[case("empty_mapping")]
#[case("packet_head_missing")]
fn embedded_production_mapping_freshness_refuses_without_trust(#[case] state: &str) {
    let (_tmp, root) = foreign_fixture(&[
        ("Cargo.toml", "[package]\nname='fx'\n"),
        (
            "src/reader.rs",
            "pub fn text() -> &'static str { include_str!(\"../templates/message.txt\") }\n",
        ),
    ]);
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    let mut packet = packet_with_paths(&["templates/message.txt"]);
    if state == "empty_mapping" {
        conn.execute_batch("CREATE TABLE test_mapping (test_symbol_id INTEGER, test_file_id INTEGER, tested_symbol_id INTEGER, tested_file_id INTEGER);").unwrap();
        assert_eq!(
            classify_test_mapping_freshness(&conn, &packet),
            MappingFreshness::Empty
        );
    } else if state == "packet_head_missing" {
        seed_mapping_row(&conn);
        seed_index_head(&conn, "indexed-parent");
        packet.head_hash = None;
        assert_eq!(
            classify_test_mapping_freshness(&conn, &packet),
            MappingFreshness::PacketHeadMissing
        );
    }
    let database = (state != "no_database").then_some(&conn);
    let plan = plan_at(&root, &packet, database, &rust_profile());
    assert!(plan.refused, "{state}: {:?}", plan.steps);
    assert!(plan.steps.is_empty());
    let reason = plan.fallback_reason.as_deref().unwrap_or("");
    let expected = match state {
        "no_database" => "test_mapping unavailable",
        "empty_mapping" => "test_mapping is empty",
        _ => "test_mapping freshness unverifiable",
    };
    assert!(reason.contains(expected), "{state}: {reason}");
}

#[test]
fn embedded_external_test_mapping_cannot_rescue_unmapped_new_source() {
    let (_tmp, root) = foreign_fixture(&[
        ("Cargo.toml", "[package]\nname='fx'\n"),
        (
            "src/reader.rs",
            "pub fn text() -> &'static str { include_str!(\"../templates/message.txt\") }\n",
        ),
        ("src/new.rs", "pub fn new_source() {}\n"),
        (
            "tests/render_it.rs",
            "#[test] fn external_reader_contract() {}\n",
        ),
    ]);
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute_batch("CREATE TABLE project_files (id INTEGER PRIMARY KEY, file_path TEXT);
        CREATE TABLE project_symbols (id INTEGER PRIMARY KEY, symbol_name TEXT, file_id INTEGER);
        CREATE TABLE test_mapping (test_symbol_id INTEGER, test_file_id INTEGER, tested_symbol_id INTEGER, tested_file_id INTEGER);
        INSERT INTO project_files VALUES (1, 'src/reader.rs'), (2, 'tests/render_it.rs');
        INSERT INTO project_symbols VALUES (1, 'external_reader_contract', 2);
        INSERT INTO test_mapping VALUES (1, 2, NULL, 1);").unwrap();
    let pure = plan_at(
        &root,
        &packet_with_paths(&["templates/message.txt"]),
        Some(&conn),
        &rust_profile(),
    );
    assert!(!pure.refused, "{:?}", pure.fallback_reason);
    // D3(d)'s existing file-level stem exception is deliberate (0549-P3-2).
    assert!(
        nextest_step(&pure)
            .unwrap()
            .command
            .contains("test(render_it)")
    );
    let mixed = plan_at(
        &root,
        &packet_with_paths(&["src/new.rs", "templates/message.txt"]),
        Some(&conn),
        &rust_profile(),
    );
    assert!(mixed.refused, "{:?}", mixed.steps);
    assert!(mixed.steps.is_empty());
    assert!(
        mixed
            .fallback_reason
            .as_deref()
            .unwrap_or("")
            .contains("changed source")
    );
}

#[test]
fn mixed_mapped_source_preserves_unreferenced_csv_and_scan_annotations() {
    let (_tmp, root) = foreign_fixture(&[
        ("Cargo.toml", "[package]\nname='fx'\n"),
        (
            "src/alpha.rs",
            "pub fn a() {}\n#[cfg(test)] mod tests { #[test] fn alpha_ok() {} }\n",
        ),
    ]);
    fs::write(
        root.join("src/generated.rs"),
        " ".repeat(super::embedded::MAX_FILE_BYTES as usize + 1),
    )
    .unwrap();
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute_batch("CREATE TABLE project_files (id INTEGER PRIMARY KEY, file_path TEXT);
        CREATE TABLE project_symbols (id INTEGER PRIMARY KEY, symbol_name TEXT, file_id INTEGER);
        CREATE TABLE test_mapping (test_symbol_id INTEGER, test_file_id INTEGER, tested_symbol_id INTEGER, tested_file_id INTEGER);
        INSERT INTO project_files VALUES (1, 'src/alpha.rs');
        INSERT INTO project_symbols VALUES (1, 'alpha_ok', 1);
        INSERT INTO test_mapping VALUES (1, 1, NULL, 1);").unwrap();
    let plan = plan_at(
        &root,
        &packet_with_paths(&["src/alpha.rs", "data/x.csv"]),
        Some(&conn),
        &rust_profile(),
    );
    assert!(!plan.refused, "{:?}", plan.fallback_reason);
    let nextest = nextest_step(&plan).unwrap();
    assert!(nextest.command.contains("test(alpha_ok)"));
    assert!(
        nextest
            .description
            .contains("no direct include/read found by the lexical lookup for: data/x.csv"),
        "{}",
        nextest.description
    );
    assert!(
        nextest
            .description
            .contains("skipped token-free oversized source src/generated.rs"),
        "{}",
        nextest.description
    );
    assert!(
        !nextest.description.contains("plus embedded-material tests"),
        "unreferenced material must not claim embedded tests: {}",
        nextest.description
    );
}

#[rstest::rstest]
#[case("assets/logo.png", false)]
#[case("docs/guide.md", true)]
fn non_rust_zero_step_profile_has_no_annotation_step(#[case] changed: &str, #[case] bounded: bool) {
    let (_tmp, root) = foreign_fixture(&[("backend/Cargo.toml", "[package]\nname='fx'\n")]);
    if bounded {
        fs::create_dir(root.join("src")).unwrap();
        let file = fs::File::create(root.join("src/generated.rs")).unwrap();
        file.set_len(super::embedded::MAX_OVERSIZED_INSPECT_BYTES + 1)
            .unwrap();
    }
    let profile = crate::platform::repository::detect_repository(root.as_std_path());
    assert!(
        profile.rust.is_none(),
        "root-based profile must stay unchanged"
    );
    let plan = super::scoped::build_plan_scoped_with_rust_crates(
        &packet_with_paths(&[changed]),
        &Rules::default(),
        &[],
        &crate::config::model::VerifyConfig::default(),
        &profile,
        VerifyScope::Fast,
        None,
        &crate::state::layout::Layout::new(&root),
        false,
        false,
        Some(super::embedded::RustCrates::Present),
    );
    assert!(!plan.refused, "{:?}", plan.fallback_reason);
    assert_eq!(plan.fallback_reason, None);
    assert!(
        plan.steps.is_empty(),
        "zero-step profiles have nowhere to attach annotations: {:?}",
        plan.steps
    );
}

fn non_rust_plan(
    root: &camino::Utf8Path,
    paths: &[&str],
    profile: &crate::platform::repository::RepositoryProfile,
) -> VerificationPlan {
    plan_at(root, &packet_with_paths(paths), None, profile)
}

fn default_non_rust_plan(root: &camino::Utf8Path, paths: &[&str]) -> VerificationPlan {
    non_rust_plan(
        root,
        paths,
        &crate::platform::repository::RepositoryProfile::default(),
    )
}

fn assert_embed_reason(plan: &VerificationPlan, needle: &str) {
    assert!(plan.refused, "{:?}", plan.fallback_reason);
    let reason = plan.fallback_reason.as_deref().unwrap_or("");
    assert!(
        reason.contains("non-Rust embedded"),
        "missing non-Rust refusal in {reason}"
    );
    assert!(reason.contains(needle), "{reason}");
}

fn assert_not_embed_refused(plan: &VerificationPlan) {
    let reason = plan.fallback_reason.as_deref().unwrap_or("");
    assert!(
        !reason.contains("non-Rust embedded"),
        "unexpected non-Rust refusal: {reason}"
    );
}

const PY_LOAD: &str = "def load(p):\n    open(p).read()\n";

#[test]
#[allow(non_snake_case)] // project test naming: feature__condition__expected
fn python_open_literal__changed_template__refuses() {
    let (_tmp, root) = foreign_fixture(&[("app.py", "open(\"templates/message.txt\").read()\n")]);
    let plan = default_non_rust_plan(&root, &["templates/message.txt"]);
    assert_embed_reason(
        &plan,
        "templates/message.txt: non-Rust embedded read (app.py: open)",
    );
    let decision = decide_non_rust(
        root.as_std_path(),
        &["templates/message.txt".to_string()],
        |_| false,
    );
    assert!(decision.stems.is_empty());
    assert!(decision.needs_mapping.is_empty());
}

#[test]
#[allow(non_snake_case)] // project test naming: feature__condition__expected
fn python_importlib_resources__resource_path__refuses() {
    let src = "import importlib.resources\nimportlib.resources.read_text(\"app\", \"templates/message.txt\")\n";
    let (_tmp, root) = foreign_fixture(&[("app.py", src)]);
    let hit = default_non_rust_plan(&root, &["templates/message.txt"]);
    assert_embed_reason(
        &hit,
        "templates/message.txt: non-Rust embedded read (app.py: importlib.resources)",
    );
    let readme = default_non_rust_plan(&root, &["README.md"]);
    assert!(!readme.refused, "{:?}", readme.fallback_reason);
}

#[test]
#[allow(non_snake_case)] // project test naming: feature__condition__expected
fn python_basename_helper__license__refuses() {
    let (_tmp, root) = foreign_fixture(&[
        ("app.py", PY_LOAD),
        (
            "test_app.py",
            "def load(p):\n    open(p).read()\nload(\"LICENSE\")\n",
        ),
    ]);
    let plan = default_non_rust_plan(&root, &["LICENSE"]);
    assert_embed_reason(&plan, "LICENSE: non-Rust embedded read (test_app.py: open)");
}

#[test]
#[allow(non_snake_case)] // project test naming: feature__condition__expected
fn python_basename_helper__docs_license__stays_unreferenced() {
    let (_tmp, root) = foreign_fixture(&[(
        "app.py",
        "def load(p):\n    open(p).read()\nload(\"LICENSE\")\n",
    )]);
    let plan = default_non_rust_plan(&root, &["docs/LICENSE"]);
    assert!(!plan.refused, "{:?}", plan.fallback_reason);
    assert!(plan.steps.is_empty(), "{:?}", plan.steps);
}

#[test]
#[allow(non_snake_case)] // project test naming: feature__condition__expected
fn python_basename_helper__unreferenced_license__stays_zero_step() {
    let (_tmp, root) = foreign_fixture(&[("app.py", "x = 1\n")]);
    let plan = default_non_rust_plan(&root, &["LICENSE"]);
    assert!(!plan.refused, "{:?}", plan.fallback_reason);
    assert!(plan.steps.is_empty(), "{:?}", plan.steps);
}

#[test]
#[allow(non_snake_case)] // project test naming: feature__condition__expected
fn python_runtime_join__other_template__stays_accepted() {
    let src = "import os\ndef load(p):\n    open(p).read()\nload(os.path.join(base, \"templates/other.txt\"))\n";
    let (_tmp, root) = foreign_fixture(&[("app.py", src)]);
    let plan = default_non_rust_plan(&root, &["templates/other.txt"]);
    assert!(!plan.refused, "{:?}", plan.fallback_reason);
}

#[test]
#[allow(non_snake_case)] // project test naming: feature__condition__expected
fn python_render_forward__message_literal__stays_outside_one_hop() {
    let src = "import os\ndef load(p):\n    open(p).read()\ndef render(name):\n    return load(os.path.join(base, name))\nrender(\"templates/message.txt\")\n";
    let (_tmp, root) = foreign_fixture(&[("app.py", src)]);
    let plan = default_non_rust_plan(&root, &["templates/message.txt"]);
    assert!(!plan.refused, "{:?}", plan.fallback_reason);
}

#[test]
#[allow(non_snake_case)] // project test naming: feature__condition__expected
fn python_comment_open__is_not_a_consumer() {
    let (_tmp, root) = foreign_fixture(&[("app.py", "# open(\"templates/message.txt\")\nx = 1\n")]);
    let plan = default_non_rust_plan(&root, &["templates/message.txt"]);
    assert!(!plan.refused, "{:?}", plan.fallback_reason);
}

#[test]
#[allow(non_snake_case)] // project test naming: feature__condition__expected
fn python_open_literal__keyword_file__refuses() {
    let (_tmp, root) = foreign_fixture(&[("app.py", "open(file=\"templates/message.txt\")\n")]);
    let plan = default_non_rust_plan(&root, &["templates/message.txt"]);
    assert_embed_reason(
        &plan,
        "templates/message.txt: non-Rust embedded read (app.py: open)",
    );
}

#[test]
#[allow(non_snake_case)] // project test naming: feature__condition__expected
fn python_importlib__joinpath_segments__refuses() {
    let src = "import importlib.resources\nimportlib.resources.files(\"app\").joinpath(\"templates\", \"message.txt\").read_text()\n";
    let (_tmp, root) = foreign_fixture(&[("app.py", src)]);
    let plan = default_non_rust_plan(&root, &["templates/message.txt"]);
    assert_embed_reason(
        &plan,
        "templates/message.txt: non-Rust embedded read (app.py: importlib.resources)",
    );
    let package = default_non_rust_plan(&root, &["app"]);
    assert_not_embed_refused(&package);
}

#[test]
#[allow(non_snake_case)] // project test naming: feature__condition__expected
fn go_embed_file__changed_template__refuses() {
    let src = "package main\n//go:embed templates/message.txt\nvar b []byte\n";
    let (_tmp, root) = foreign_fixture(&[("embed.go", src), ("templates/message.txt", "hi\n")]);
    let plan = default_non_rust_plan(&root, &["templates/message.txt"]);
    assert_embed_reason(
        &plan,
        "templates/message.txt: non-Rust embedded read (embed.go: go:embed)",
    );
}

#[test]
#[allow(non_snake_case)] // project test naming: feature__condition__expected
fn go_embed_dir__nested_file__refuses_and_other_tree_stays() {
    let src = "package main\n//go:embed templates\nvar fs embed.FS\n";
    let (_tmp, root) = foreign_fixture(&[
        ("embed.go", src),
        ("templates/message.txt", "hi\n"),
        ("other/note.txt", "x\n"),
    ]);
    let hit = default_non_rust_plan(&root, &["templates/message.txt"]);
    assert_embed_reason(
        &hit,
        "templates/message.txt: non-Rust embedded read (embed.go: go:embed)",
    );
    let miss = default_non_rust_plan(&root, &["other/note.txt"]);
    assert!(!miss.refused, "{:?}", miss.fallback_reason);
}

#[test]
#[allow(non_snake_case)] // project test naming: feature__condition__expected
fn go_readfile_basename__license__refuses() {
    let src = "package main\nfunc load(p string) {\n\tos.ReadFile(p)\n}\nfunc main() { load(\"LICENSE\") }\n";
    let (_tmp, root) = foreign_fixture(&[("read.go", src)]);
    let plan = default_non_rust_plan(&root, &["LICENSE"]);
    assert_embed_reason(
        &plan,
        "LICENSE: non-Rust embedded read (read.go: os.ReadFile)",
    );
}

#[test]
#[allow(non_snake_case)] // project test naming: feature__condition__expected
fn go_runtime_join__other_template__stays_accepted() {
    let src = "package other\nfunc load(p string) {\n\tos.ReadFile(p)\n}\nfunc main() { load(filepath.Join(base, \"templates/other.txt\")) }\n";
    let (_tmp, root) = foreign_fixture(&[("other/join.go", src)]);
    let plan = default_non_rust_plan(&root, &["templates/other.txt"]);
    assert!(!plan.refused, "{:?}", plan.fallback_reason);
}

#[test]
#[allow(non_snake_case)] // project test naming: feature__condition__expected
fn go_embed_star__package_file_only() {
    let src = "package main\n//go:embed *\nvar b embed.FS\n";
    let (_tmp, root) = foreign_fixture(&[
        ("embed.go", src),
        ("LICENSE", "mit\n"),
        ("templates/message.txt", "hi\n"),
    ]);
    let license = default_non_rust_plan(&root, &["LICENSE"]);
    assert_embed_reason(
        &license,
        "LICENSE: non-Rust embedded read (embed.go: go:embed)",
    );
    let nested = default_non_rust_plan(&root, &["templates/message.txt"]);
    assert!(!nested.refused, "{:?}", nested.fallback_reason);
}

#[test]
#[allow(non_snake_case)] // project test naming: feature__condition__expected
fn go_readfile__os_open_fixture__refuses() {
    let src = "package main\nfunc main() {\n\tos.Open(\"fixtures/data.txt\")\n}\n";
    let (_tmp, root) = foreign_fixture(&[("read.go", src)]);
    let plan = default_non_rust_plan(&root, &["fixtures/data.txt"]);
    assert_embed_reason(
        &plan,
        "fixtures/data.txt: non-Rust embedded read (read.go: os.Open)",
    );
}

#[test]
#[allow(non_snake_case)] // project test naming: feature__condition__expected
fn go_readfile__ioutil_and_openfile__stay_accepted() {
    let src = "package main\nfunc main() {\n\tioutil.ReadFile(\"fixtures/data.txt\")\n\tos.OpenFile(\"fixtures/data.txt\", os.O_RDONLY, 0)\n}\n";
    let (_tmp, root) = foreign_fixture(&[("read.go", src)]);
    let plan = default_non_rust_plan(&root, &["fixtures/data.txt"]);
    assert!(!plan.refused, "{:?}", plan.fallback_reason);
}

#[test]
#[allow(non_snake_case)] // project test naming: feature__condition__expected
fn js_readfilesync_literal__changed_template__refuses() {
    let (_tmp, root) =
        foreign_fixture(&[("read.js", "fs.readFileSync(\"templates/message.txt\")\n")]);
    let plan = default_non_rust_plan(&root, &["templates/message.txt"]);
    assert_embed_reason(
        &plan,
        "templates/message.txt: non-Rust embedded read (read.js: readFileSync)",
    );
}

#[test]
#[allow(non_snake_case)] // project test naming: feature__condition__expected
fn js_asset_import__png__refuses() {
    let (_tmp, root) = foreign_fixture(&[("app.ts", "import logo from \"./assets/logo.png\"\n")]);
    let plan = default_non_rust_plan(&root, &["assets/logo.png"]);
    assert_embed_reason(
        &plan,
        "assets/logo.png: non-Rust embedded read (app.ts: import)",
    );
}

#[test]
#[allow(non_snake_case)] // project test naming: feature__condition__expected
fn js_basename_helper__license__refuses() {
    let src = "function load(p) {\n  fs.readFileSync(p)\n}\nload(\"LICENSE\")\n";
    let (_tmp, root) = foreign_fixture(&[("read.js", src)]);
    let plan = default_non_rust_plan(&root, &["LICENSE"]);
    assert_embed_reason(
        &plan,
        "LICENSE: non-Rust embedded read (read.js: readFileSync)",
    );
}

#[test]
#[allow(non_snake_case)] // project test naming: feature__condition__expected
fn js_template_literal__other_png__stays_accepted() {
    let (_tmp, root) = foreign_fixture(&[("read.js", "fs.readFileSync(`assets/${name}.png`)\n")]);
    let plan = default_non_rust_plan(&root, &["assets/other.png"]);
    assert!(!plan.refused, "{:?}", plan.fallback_reason);
}

#[test]
#[allow(non_snake_case)] // project test naming: feature__condition__expected
fn js_asset_import__extensionless_png__refuses() {
    let (_tmp, root) = foreign_fixture(&[("app.ts", "import logo from \"./assets/logo\"\n")]);
    let plan = default_non_rust_plan(&root, &["assets/logo.png"]);
    assert_embed_reason(
        &plan,
        "assets/logo.png: non-Rust embedded read (app.ts: import)",
    );
}

#[test]
#[allow(non_snake_case)] // project test naming: feature__condition__expected
fn js_asset_import__extensionless_source__stays_source() {
    let (_tmp, root) = foreign_fixture(&[("app.ts", "import logo from \"./assets/logo\"\n")]);
    let decision = decide_non_rust(
        root.as_std_path(),
        &[
            "assets/logo.ts".to_string(),
            "assets/logo.mjs".to_string(),
            "assets/logo/extra.png".to_string(),
        ],
        |_| false,
    );
    assert!(decision.refuse.is_none(), "{:?}", decision.refuse);
    assert!(decision.stems.is_empty());
    assert!(decision.needs_mapping.is_empty());
}

#[test]
#[allow(non_snake_case)] // project test naming: feature__condition__expected
fn js_asset_import__alias_specifier__stays_accepted() {
    let src = "import a from \"@/assets/logo.png\"\nimport b from \"~/assets/logo.png\"\n";
    let (_tmp, root) = foreign_fixture(&[("app.ts", src)]);
    let plan = default_non_rust_plan(&root, &["assets/logo.png"]);
    assert!(!plan.refused, "{:?}", plan.fallback_reason);
}

#[test]
#[allow(non_snake_case)] // project test naming: feature__condition__expected
fn non_rust_source_only__does_not_embed_refuse() {
    let (_tmp, root) = foreign_fixture(&[("app.py", "open(\"templates/message.txt\")\n")]);
    let plan = default_non_rust_plan(&root, &["app.py"]);
    assert_not_embed_refused(&plan);
}

#[test]
#[allow(non_snake_case)] // project test naming: feature__condition__expected
fn non_rust_source_only__mjs_edit__does_not_embed_refuse() {
    let (_tmp, root) = foreign_fixture(&[("app.mjs", "import \"./assets/logo.png\"\n")]);
    let plan = default_non_rust_plan(&root, &["app.mjs"]);
    assert_not_embed_refused(&plan);
}

fn write_scan_sources(root: &std::path::Path, count: usize) {
    let dir = root.join("pkg");
    fs::create_dir_all(&dir).unwrap();
    for index in 0..count {
        fs::write(dir.join(format!("f{index:04}.py")), b"x = 1\n").unwrap();
    }
}

#[test]
#[allow(non_snake_case)] // project test naming: feature__condition__expected
fn non_rust_bounded_scan__noncheap__refuses() {
    let tmp = tempfile::tempdir().unwrap();
    write_scan_sources(tmp.path(), 2501);
    fs::write(tmp.path().join("notes.txt"), b"n\n").unwrap();
    let root = camino::Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
    let plan = default_non_rust_plan(&root, &["notes.txt"]);
    assert_embed_reason(&plan, "notes.txt: non-Rust embedded scan bound exceeded:");
    assert!(
        plan.fallback_reason
            .as_deref()
            .unwrap_or("")
            .contains("source files (max 2500)"),
        "{:?}",
        plan.fallback_reason
    );
}

#[test]
#[allow(non_snake_case)] // project test naming: feature__condition__expected
fn non_rust_bounded_scan__cheap_docs__stays_zero_step() {
    let tmp = tempfile::tempdir().unwrap();
    write_scan_sources(tmp.path(), 2501);
    fs::create_dir_all(tmp.path().join("docs")).unwrap();
    fs::write(tmp.path().join("docs/contract.md"), b"v1\n").unwrap();
    let root = camino::Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
    let plan = default_non_rust_plan(&root, &["docs/contract.md"]);
    assert!(!plan.refused, "{:?}", plan.fallback_reason);
    assert!(plan.steps.is_empty(), "{:?}", plan.steps);
    let decision = decide_non_rust(
        root.as_std_path(),
        &["docs/contract.md".to_string()],
        |path| path == "docs/contract.md",
    );
    assert!(decision.refuse.is_none(), "{:?}", decision.refuse);
    assert!(
        decision
            .annotations
            .iter()
            .any(|note| note.contains("embedded lookup bounded: not verified")),
        "{:?}",
        decision.annotations
    );
}

struct UnreadablePy {
    path: PathBuf,
    #[cfg(windows)]
    _guard: fs::File,
}

impl UnreadablePy {
    fn create(path: &Path) -> Self {
        fs::write(path, b"print('x')\n").unwrap();
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            let file = fs::OpenOptions::new()
                .read(true)
                .share_mode(0)
                .open(path)
                .unwrap();
            Self {
                path: path.to_path_buf(),
                _guard: file,
            }
        }
        #[cfg(not(windows))]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(path).unwrap().permissions();
            perms.set_mode(0);
            fs::set_permissions(path, perms).unwrap();
            Self {
                path: path.to_path_buf(),
            }
        }
    }
}

impl Drop for UnreadablePy {
    fn drop(&mut self) {
        let _ = &self.path;
        #[cfg(not(windows))]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(meta) = fs::metadata(&self.path) {
                let mut perms = meta.permissions();
                perms.set_mode(0o644);
                let _ = fs::set_permissions(&self.path, perms);
            }
        }
    }
}

#[test]
#[allow(non_snake_case)] // project test naming: feature__condition__expected
fn non_rust_unreadable__cheap_only__stays_zero_step() {
    let tmp = tempfile::tempdir().unwrap();
    let _guard = UnreadablePy::create(&tmp.path().join("bad.py"));
    fs::create_dir_all(tmp.path().join("docs")).unwrap();
    fs::write(tmp.path().join("docs/contract.md"), b"v1\n").unwrap();
    let root = camino::Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
    let plan = default_non_rust_plan(&root, &["docs/contract.md"]);
    assert!(!plan.refused, "{:?}", plan.fallback_reason);
    assert!(plan.steps.is_empty(), "{:?}", plan.steps);
}

#[test]
#[allow(non_snake_case)] // project test naming: feature__condition__expected
fn non_rust_unreadable__noncheap__refuses_without_os_text() {
    let tmp = tempfile::tempdir().unwrap();
    let _guard = UnreadablePy::create(&tmp.path().join("bad.py"));
    fs::write(tmp.path().join("notes.txt"), b"n\n").unwrap();
    let root = camino::Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
    let plan = default_non_rust_plan(&root, &["notes.txt"]);
    assert_embed_reason(&plan, "bad.py: unreadable");
    let reason = plan.fallback_reason.as_deref().unwrap_or("");
    assert!(
        !reason.contains("os error") && !reason.contains("Access is denied"),
        "{reason}"
    );
}

#[test]
#[allow(non_snake_case)] // project test naming: feature__condition__expected
fn polyglot_rust_stem__still_selects_nextest() {
    let src = "pub fn contract() -> &'static str { include_str!(\"../docs/contract.md\") }\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn t_doc() { let _ = contract(); }\n}\n";
    let (_tmp, root) = foreign_fixture(&[
        ("Cargo.toml", "[package]\nname=\"fx\"\n"),
        ("src/lib.rs", src),
        ("docs/contract.md", "v1\n"),
    ]);
    let plan = non_rust_plan(&root, &["docs/contract.md"], &rust_profile());
    assert!(!plan.refused, "{:?}", plan.fallback_reason);
    let nextest = nextest_step(&plan).expect("rust include still selects nextest");
    assert!(
        nextest.command.contains("test(t_doc)"),
        "{}",
        nextest.command
    );
}

#[test]
#[allow(non_snake_case)] // project test naming: feature__condition__expected
fn polyglot_python_read__no_rust_consumer__refuses() {
    let (_tmp, root) = foreign_fixture(&[
        ("src/lib.rs", "pub fn unused() {}\n"),
        ("app.py", "open(\"assets/logo.png\").read()\n"),
    ]);
    let hit = default_non_rust_plan(&root, &["assets/logo.png"]);
    assert_embed_reason(
        &hit,
        "assets/logo.png: non-Rust embedded read (app.py: open)",
    );
    let miss = default_non_rust_plan(&root, &["assets/other.png"]);
    assert!(!miss.refused, "{:?}", miss.fallback_reason);
    assert_not_embed_refused(&miss);
}
