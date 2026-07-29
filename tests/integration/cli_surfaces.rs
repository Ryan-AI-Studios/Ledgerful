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
