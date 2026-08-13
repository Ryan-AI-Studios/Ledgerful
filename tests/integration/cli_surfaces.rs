use crate::common::{DirGuard, git_add_and_commit, setup_git_repo};
use ledgerful::commands::data_models::execute_data_models;
use ledgerful::commands::data_models::{DataModelSubcommands, DataModelsArgs};
use ledgerful::commands::endpoints::execute_endpoints;
use ledgerful::commands::init::execute_init;
use ledgerful::commands::observability::execute_observability;
use ledgerful::commands::observability::{ObservabilityArgs, ObservabilitySubcommands};
use ledgerful::commands::security::execute_security;
use ledgerful::commands::security::{SecurityArgs, SecuritySubcommands};
use ledgerful::commands::services_diff::ServicesDiffArgs;
use ledgerful::commands::services_diff::execute_services_diff;
use ledgerful::config::model::Config;
use std::fs;
use tempfile::tempdir;

#[test]
fn test_endpoints_json() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();

    setup_git_repo(root);
    fs::write(root.join("dummy.txt"), "content").unwrap();
    git_add_and_commit(root, "initial");

    let _guard = DirGuard::new(root);
    execute_init(false, false).unwrap();

    // EndpointsArgs fields are private, so construct via clap::Cli::try_parse_from
    use clap::Parser;
    use ledgerful::cli::{Cli, Commands};
    let cli = Cli::try_parse_from(["ledgerful", "endpoints", "--json"])
        .expect("endpoints --json parsing must succeed");
    match cli.command {
        Commands::Endpoints(args) => {
            let result = execute_endpoints(args);
            assert!(result.is_ok());
        }
        _ => panic!("expected Endpoints command"),
    }
}

/// DoD-7: human `endpoints` table never shows raw JSON array syntax in Auth.
#[test]
fn test_endpoints_human_auth_no_raw_json() {
    use crate::common::run_cli;
    use rusqlite::Connection;

    let tmp = tempdir().unwrap();
    let root = tmp.path();

    setup_git_repo(root);
    fs::write(root.join("dummy.txt"), "content").unwrap();
    git_add_and_commit(root, "initial");

    let _guard = DirGuard::new(root);
    execute_init(false, false).unwrap();

    // Seed an authenticated route row — writer shape is Option<Vec<String>> JSON.
    // Extractor coverage for live Axum auth is separate; this asserts the human renderer.
    let db_path = root.join(".ledgerful").join("state").join("ledger.db");
    let conn = Connection::open(&db_path).expect("open ledger.db");
    // Ensure a project_files row exists for the FK if required; insert minimal route.
    // If schema requires handler_file_id, insert a stub file first.
    let file_id: i64 = conn
        .query_row(
            "SELECT id FROM project_files LIMIT 1",
            [],
            |row| row.get(0),
        )
        .unwrap_or_else(|_| {
            conn.execute(
                "INSERT INTO project_files (file_path, language, content_hash, file_size, last_indexed_at)
                 VALUES ('src/routes.rs', 'Rust', 'abc', 100, datetime('now'))",
                [],
            )
            .ok();
            conn.query_row("SELECT id FROM project_files LIMIT 1", [], |row| row.get(0))
                .unwrap_or(1)
        });

    // Columns may vary by migration; use the known set from api_routes.
    let insert = conn.execute(
        "INSERT INTO api_routes (
            method, path_pattern, handler_symbol_name, handler_file_id,
            framework, route_source, route_confidence, last_indexed_at, auth_requirements
         ) VALUES (
            'GET', '/api/secure', 'handler', ?1,
            'Axum', 'BUILDER', 0.9, datetime('now'), ?2
         )",
        rusqlite::params![file_id, r#"["secured"]"#],
    );
    if insert.is_err() {
        // Fallback: try without optional columns if schema differs.
        conn.execute(
            "INSERT INTO api_routes (
                method, path_pattern, handler_file_id, framework,
                route_source, route_confidence, last_indexed_at, auth_requirements
             ) VALUES (
                'GET', '/api/secure', ?1, 'Axum',
                'BUILDER', 0.9, datetime('now'), ?2
             )",
            rusqlite::params![file_id, r#"["secured"]"#],
        )
        .expect("insert api_routes row");
    }

    let (stdout, stderr, code) = run_cli(root, &["endpoints"]);
    assert_eq!(code, 0, "endpoints must succeed; stderr={stderr}");
    assert!(
        !stdout.contains("[\""),
        "DoD-7: human Auth column must not show raw JSON array; stdout={stdout}"
    );
    // Seeded ["secured"] must render as the human token, not only a header.
    assert!(
        stdout.contains("secured"),
        "DoD-7: seeded auth [\"secured\"] must render as human text; stdout={stdout}"
    );
}

#[test]
fn test_data_models_impact_changed() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();

    setup_git_repo(root);
    fs::write(root.join("dummy.txt"), "content").unwrap();
    git_add_and_commit(root, "initial");

    let _guard = DirGuard::new(root);
    execute_init(false, false).unwrap();

    let args = DataModelsArgs {
        command: DataModelSubcommands::Impact {
            changed: true,
            json: false,
        },
    };
    let result = execute_data_models(args);
    assert!(result.is_ok());
}

/// B4 / DoD-2 / DoD-6: clean-tree `data-models impact --changed` must not
/// create or rewrite `.ledgerful/reports/latest-impact.json`.
///
/// Filter CLIs use `collect_changed_files_for_filter` (git status only). If
/// `execute_impact_silent` were reintroduced, `write_impact_report` would
/// clobber a seeded report (or create one when absent).
#[test]
fn test_data_models_impact_changed_does_not_rewrite_latest_impact() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();

    setup_git_repo(root);
    fs::write(root.join("dummy.txt"), "content").unwrap();
    git_add_and_commit(root, "initial");

    let _guard = DirGuard::new(root);
    execute_init(false, false).unwrap();

    let report_path = root
        .join(".ledgerful")
        .join("reports")
        .join("latest-impact.json");
    fs::create_dir_all(report_path.parent().expect("reports parent")).expect("mkdir reports");
    let seed = r#"{"schemaVersion":"v1","headHash":"SEED_MARKER_0146_B4","riskReasons":["seed-do-not-clobber"]}"#;
    fs::write(&report_path, seed).expect("seed latest-impact.json");
    let before = fs::read(&report_path).expect("read seeded report");
    assert!(
        std::str::from_utf8(&before)
            .expect("utf8 seed")
            .contains("SEED_MARKER_0146_B4"),
        "precondition: seed marker present"
    );

    // Clean tree (no dirty files after commit + init side-effects may remain
    // uncommitted). Re-commit so git status is empty for a true clean filter path.
    git_add_and_commit(root, "post-init clean");

    let args = DataModelsArgs {
        command: DataModelSubcommands::Impact {
            changed: true,
            json: true,
        },
    };
    let result = execute_data_models(args);
    assert!(
        result.is_ok(),
        "data-models impact --changed must succeed on clean tree: {result:?}"
    );

    assert!(
        report_path.is_file(),
        "seeded latest-impact.json must still exist after filter CLI"
    );
    let after = fs::read(&report_path).expect("read report after filter CLI");
    assert_eq!(
        before, after,
        "data-models impact --changed must not rewrite latest-impact.json \
         (filter path must not call execute_impact_silent / write_impact_report)"
    );

    // Absence case: remove report and re-run — must not create it.
    fs::remove_file(&report_path).expect("remove seeded report");
    assert!(!report_path.exists(), "precondition: report absent");

    let args_again = DataModelsArgs {
        command: DataModelSubcommands::Impact {
            changed: true,
            json: true,
        },
    };
    let result_again = execute_data_models(args_again);
    assert!(
        result_again.is_ok(),
        "second clean-tree run must succeed: {result_again:?}"
    );
    assert!(
        !report_path.exists(),
        "data-models impact --changed must not create latest-impact.json when absent"
    );
}

#[test]
fn test_observability_coverage_json() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();

    setup_git_repo(root);
    fs::write(root.join("dummy.txt"), "content").unwrap();
    git_add_and_commit(root, "initial");

    let _guard = DirGuard::new(root);
    execute_init(false, false).unwrap();

    let args = ObservabilityArgs {
        command: ObservabilitySubcommands::Coverage { json: true },
    };
    let result = execute_observability(args);
    assert!(result.is_ok());
}

#[test]
fn test_security_boundaries_human() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();

    setup_git_repo(root);
    fs::write(root.join("dummy.txt"), "content").unwrap();
    git_add_and_commit(root, "initial");

    let _guard = DirGuard::new(root);
    execute_init(false, false).unwrap();

    let args = SecurityArgs {
        command: SecuritySubcommands::Boundaries { json: false },
    };
    let result = execute_security(args);
    assert!(result.is_ok());
}

#[test]
fn test_services_diff() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();

    setup_git_repo(root);
    fs::write(root.join("dummy.txt"), "content").unwrap();
    git_add_and_commit(root, "initial");

    let _guard = DirGuard::new(root);
    execute_init(false, false).unwrap();

    let args = ServicesDiffArgs {
        full: false,
        json: false,
    };
    let config = Config::default();
    let result = execute_services_diff(args, &config);
    assert!(result.is_ok());
}

// ---------------------------------------------------------------------------
// 0185 — `ledgerful surfaces` / `tour` inventory
// ---------------------------------------------------------------------------

fn seed_data_models_ready(root: &std::path::Path) {
    use camino::Utf8Path;
    use ledgerful::state::layout::Layout;
    use ledgerful::state::storage::StorageManager;

    let root_utf8 = Utf8Path::from_path(root).expect("utf8 root");
    let layout = Layout::new(root_utf8);
    layout.ensure_state_dir().unwrap();
    let storage =
        StorageManager::init(layout.state_subdir().join("ledger.db").as_std_path()).unwrap();
    let conn = storage.get_connection();
    conn.execute(
        "INSERT INTO project_files (id, file_path, last_indexed_at)
         VALUES (1, 'src/models.rs', '2026-01-01T00:00:00Z')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO data_models (model_name, model_file_id, language, model_kind, confidence, last_indexed_at)
         VALUES ('Invoice', 1, 'Rust', 'STRUCT', 0.9, '2026-01-01T00:00:00Z')",
        [],
    )
    .unwrap();
    storage.shutdown().unwrap();
}

fn setup_surfaces_mint_repo() -> tempfile::TempDir {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    fs::write(root.join("dummy.txt"), "content").unwrap();
    git_add_and_commit(root, "initial");
    let _guard = DirGuard::new(root);
    execute_init(false, false).unwrap();
    seed_data_models_ready(root);
    tmp
}

#[test]
fn surfaces_json_default_config_is_two_gated_three_empty_one_ready() {
    use crate::common::run_cli;

    let tmp = setup_surfaces_mint_repo();
    let root = tmp.path();

    let (stdout, stderr, code) = run_cli(root, &["surfaces", "--json"]);
    assert_eq!(code, 0, "surfaces --json must succeed; stderr={stderr}");
    assert!(
        !stdout.contains("gated ·"),
        "JSON must not include human summary; stdout={stdout}"
    );
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).expect("pure JSON envelope");
    assert_eq!(v["schemaVersion"], 1);
    assert_eq!(v["kind"], "surfaces");
    assert_eq!(v["coverageEnabled"], false);
    assert_eq!(v["counts"]["ready"], 1);
    assert_eq!(v["counts"]["empty"], 3);
    assert_eq!(v["counts"]["gated"], 2);
    let ids: Vec<&str> = v["surfaces"]
        .as_array()
        .expect("surfaces array")
        .iter()
        .map(|s| s["id"].as_str().expect("id"))
        .collect();
    assert_eq!(
        ids,
        [
            "services",
            "deploy",
            "security",
            "observability",
            "schema",
            "data-models"
        ]
    );
    assert_eq!(v["surfaces"][0]["status"], "gated");
    assert_eq!(v["surfaces"][1]["status"], "gated");
    assert_eq!(v["surfaces"][2]["status"], "empty");
    assert_eq!(v["surfaces"][3]["status"], "empty");
    assert_eq!(v["surfaces"][4]["status"], "empty");
    assert_eq!(v["surfaces"][5]["status"], "ready");
    assert_eq!(v["surfaces"][0]["gate"], "coverage.global");
    assert_eq!(v["surfaces"][5]["gate"], "none");
}

#[test]
fn tour_alias_matches_surfaces_json() {
    use crate::common::run_cli;

    let tmp = setup_surfaces_mint_repo();
    let root = tmp.path();
    let (surfaces, _, c1) = run_cli(root, &["surfaces", "--json"]);
    let (tour, _, c2) = run_cli(root, &["tour", "--json"]);
    assert_eq!(c1, 0);
    assert_eq!(c2, 0);
    assert_eq!(surfaces, tour);
}

#[test]
fn surfaces_human_table_and_summary() {
    use crate::common::run_cli;

    let tmp = setup_surfaces_mint_repo();
    let root = tmp.path();
    let (stdout, stderr, code) = run_cli(root, &["surfaces"]);
    assert_eq!(code, 0, "surfaces must succeed; stderr={stderr}");
    assert!(
        stdout.contains("Services"),
        "human table must list Services; stdout={stdout}"
    );
    assert!(
        stdout.contains("gated"),
        "human table must show gated; stdout={stdout}"
    );
    assert!(
        stdout.contains("2 gated · 3 empty · 1 ready"),
        "summary counts; stdout={stdout}"
    );
    assert!(
        !stdout.contains("schemaVersion"),
        "human path must not dump JSON envelope; stdout={stdout}"
    );
}

#[test]
fn surfaces_is_read_only() {
    use crate::common::run_cli;

    let tmp = setup_surfaces_mint_repo();
    let root = tmp.path();
    let config_path = root.join(".ledgerful").join("config.toml");
    let before = fs::read(&config_path).expect("read config");
    let policies = root.join("policies");
    assert!(!policies.exists(), "precondition: no policies/");
    let (stdout, stderr, code) = run_cli(root, &["surfaces", "--json"]);
    assert_eq!(code, 0, "stderr={stderr} stdout={stdout}");
    let after = fs::read(&config_path).expect("reread config");
    assert_eq!(before, after, "surfaces must not write config");
    assert!(!policies.exists(), "surfaces must not create policies/");
    assert!(
        !root.join(".env.example").exists(),
        "surfaces must not create .env.example"
    );
}

#[test]
fn doctor_json_includes_surfaces_gated_without_blocking_publish() {
    use crate::common::run_cli;

    let tmp = setup_surfaces_mint_repo();
    let root = tmp.path();
    let (stdout, stderr, code) = run_cli(root, &["doctor", "--json"]);
    assert_eq!(code, 0, "doctor --json; stderr={stderr}");
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).expect("doctor JSON");
    assert_eq!(v["readyForPublish"], true);
    let findings = v["findings"].as_array().expect("findings");
    let gated = findings
        .iter()
        .find(|f| f["code"] == "surfaces-gated")
        .expect("surfaces-gated finding");
    assert_eq!(gated["severity"], "info");
    assert_eq!(gated["category"], "optional");
    assert_eq!(gated["remediation"], "ledgerful surfaces");
    assert!(
        gated["message"]
            .as_str()
            .expect("msg")
            .contains("services, deploy"),
        "ids in §3.2 order: {}",
        gated["message"]
    );
}

#[test]
fn surfaces_malformed_config_does_not_emit_default_inventory() {
    use crate::common::run_cli;

    let tmp = setup_surfaces_mint_repo();
    let root = tmp.path();
    let config_path = root.join(".ledgerful").join("config.toml");
    fs::write(&config_path, "this is not = valid toml [[[").expect("corrupt config");
    let (stdout, stderr, code) = run_cli(root, &["surfaces", "--json"]);
    assert_ne!(
        code, 0,
        "malformed config must fail closed; stdout={stdout} stderr={stderr}"
    );
    assert!(
        !stdout.contains("\"kind\": \"surfaces\""),
        "must not emit a default-gated inventory on config error; stdout={stdout}"
    );
}

#[test]
fn doctor_default_human_collapses_surfaces_gated() {
    use crate::common::run_cli;

    let tmp = setup_surfaces_mint_repo();
    let root = tmp.path();
    let (stdout, stderr, code) = run_cli(root, &["doctor"]);
    assert_eq!(code, 0, "doctor; stderr={stderr}");
    assert!(
        !stdout.contains("surfaces-gated"),
        "default human must collapse hygiene; stdout={stdout}"
    );
    let (full, _, c2) = run_cli(root, &["doctor", "--full"]);
    assert_eq!(c2, 0);
    assert!(
        full.contains("surfaces-gated") || full.contains("gated by coverage"),
        "doctor --full must show the finding; stdout={full}"
    );
}
