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
