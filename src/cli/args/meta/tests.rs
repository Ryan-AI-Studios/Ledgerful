use crate::cli::args::{
    ChangeContextArgs, Cli, Commands, DoctorArgs, ImpactArgs, LedgerCommands, ScanArgs,
};
use clap::Parser;

fn parse(args: &[&str]) -> Commands {
    let mut full = vec!["ledgerful"];
    full.extend_from_slice(args);
    Cli::try_parse_from(full).unwrap().command
}

#[test]
fn machine_mode_selected_for_json_flags() {
    assert!(parse(&["verify", "--json"]).is_machine_output());
    assert!(parse(&["ledger", "status", "--json"]).is_machine_output());
    assert!(parse(&["scan", "--impact", "--json"]).is_machine_output());
    assert!(parse(&["config", "view", "--json"]).is_machine_output());
    assert!(parse(&["search", "foo", "--json"]).is_machine_output());
    assert!(parse(&["search", "foo", "--json-lines"]).is_machine_output());
    assert!(
        Cli::try_parse_from(["ledgerful", "search", "foo", "--json", "--json-lines"]).is_err(),
        "search --json and --json-lines must conflict"
    );
    assert!(parse(&["index", "--check", "--json"]).is_machine_output());
    assert!(parse(&["timings", "--json"]).is_machine_output());
    assert!(parse(&["hotspots", "--json"]).is_machine_output());
    match parse(&["hotspots", "--include", "tests"]) {
        Commands::Hotspots { args } => {
            assert_eq!(args.include, Some(crate::cli::HotspotIncludeScope::Tests));
        }
        other => panic!("expected hotspots, got {other:?}"),
    }
    assert!(
        Cli::try_parse_from(["ledgerful", "hotspots", "--include", "nope"]).is_err(),
        "--include must only accept tests"
    );
    assert!(parse(&["symbols", "--json"]).is_machine_output());
    assert!(!parse(&["symbols"]).is_machine_output());
    assert_eq!(parse(&["symbols"]).command_name(), "symbols");
    assert!(parse(&["surfaces", "--json"]).is_machine_output());
    assert!(!parse(&["surfaces"]).is_machine_output());
    assert_eq!(parse(&["surfaces"]).command_name(), "surfaces");
    assert!(parse(&["tour", "--json"]).is_machine_output());
    assert!(!parse(&["tour"]).is_machine_output());
    assert_eq!(parse(&["tour"]).command_name(), "surfaces");
    assert!(parse(&["release", "--json"]).is_machine_output());
    assert!(parse(&["release", "pins", "--json"]).is_machine_output());
    assert!(!parse(&["release"]).is_machine_output());
    assert!(!parse(&["release", "pins"]).is_machine_output());
    assert_eq!(parse(&["release"]).command_name(), "release_pins");
    assert_eq!(parse(&["release", "pins"]).command_name(), "release_pins");
    assert!(
        Cli::try_parse_from(["ledgerful", "symbols", "--limit", "5001"]).is_err(),
        "symbols --limit > 5000 must be rejected"
    );
    assert!(
        Cli::try_parse_from(["ledgerful", "symbols", "--limit", "0"]).is_err(),
        "symbols --limit 0 must be rejected"
    );
    #[cfg(feature = "sync")]
    {
        assert!(!parse(&["sync", "setup"]).is_machine_output());
        assert!(parse(&["sync", "setup", "--json"]).is_machine_output());
        assert!(!parse(&["sync", "status"]).is_machine_output());
        assert!(parse(&["sync", "status", "--json"]).is_machine_output());
        assert!(!parse(&["sync", "run", "--once"]).is_machine_output());
        assert_eq!(parse(&["sync", "setup"]).command_name(), "sync_setup");
        assert_eq!(parse(&["sync", "status"]).command_name(), "sync_status");
    }
}

#[test]
fn machine_mode_selected_for_scan_format_json() {
    assert!(parse(&["scan", "--pr", "main...HEAD", "--format", "json"]).is_machine_output());
    assert!(!parse(&["scan", "--pr", "main...HEAD", "--format", "text"]).is_machine_output());
}

#[test]
fn machine_mode_selected_for_mcp() {
    #[cfg(feature = "mcp")]
    {
        assert!(parse(&["mcp"]).is_machine_output());
        assert!(parse(&["mcp", "serve"]).is_machine_output());
        assert!(!parse(&["mcp", "install"]).is_machine_output());
        assert!(parse(&["mcp", "install", "--json"]).is_machine_output());
        assert!(!parse(&["mcp", "uninstall"]).is_machine_output());
        assert!(parse(&["mcp", "uninstall", "--json"]).is_machine_output());
        assert!(!parse(&["mcp", "status"]).is_machine_output());
        assert!(parse(&["mcp", "status", "--json"]).is_machine_output());
        assert_eq!(parse(&["mcp"]).command_name(), "mcp");
        assert_eq!(parse(&["mcp", "serve"]).command_name(), "mcp");
        assert_eq!(parse(&["mcp", "install"]).command_name(), "mcp_install");
        assert_eq!(parse(&["mcp", "uninstall"]).command_name(), "mcp_uninstall");
        assert_eq!(parse(&["mcp", "status"]).command_name(), "mcp_status");
    }
}

#[test]
fn human_commands_not_machine() {
    assert!(!parse(&["doctor"]).is_machine_output());
    assert!(!parse(&["verify"]).is_machine_output());
    assert!(!parse(&["verify", "--signatures"]).is_machine_output());
    assert!(!parse(&["ledger", "status"]).is_machine_output());
    assert!(!parse(&["scan", "--impact"]).is_machine_output());
}

#[test]
fn export_head_stdout_is_machine_output() {
    assert!(parse(&["export", "head", "--stdout"]).is_machine_output());
    assert!(parse(&["export", "head", "-o", "-"]).is_machine_output());
    assert!(parse(&["export", "head", "--out", "-"]).is_machine_output());
    assert!(!parse(&["export", "head"]).is_machine_output());
    assert!(!parse(&["export", "head", "-o", "head.json"]).is_machine_output());
    assert!(!parse(&["export", "evidence", "--profile", "soc2"]).is_machine_output());
}

#[test]
fn export_head_argv_shape_stdout_mode() {
    // argv_shape is "export_head|flag1,flag2" with sorted flag names.
    let stdout_shape = parse(&["export", "head", "--stdout"]).argv_shape();
    assert_eq!(stdout_shape, "export_head|stdout");

    let dash_shape = parse(&["export", "head", "-o", "-"]).argv_shape();
    assert_eq!(dash_shape, "export_head|stdout");

    // force is ignored in stdout mode (not recorded).
    let force_stdout = parse(&["export", "head", "--stdout", "--force"]).argv_shape();
    assert_eq!(force_stdout, "export_head|stdout");

    let redundant = parse(&["export", "head", "--stdout", "-o", "-"]).argv_shape();
    assert_eq!(redundant, "export_head|stdout");

    let file_shape = parse(&["export", "head", "-o", "x.json", "--force"]).argv_shape();
    assert_eq!(file_shape, "export_head|force,out");
}

#[test]
fn doctor_json_is_machine_output() {
    assert!(parse(&["doctor", "--json"]).is_machine_output());
    assert!(!parse(&["doctor"]).is_machine_output());
}

#[test]
fn status_json_is_machine_output() {
    assert!(parse(&["status", "--json"]).is_machine_output());
    assert!(!parse(&["status"]).is_machine_output());
    assert_eq!(parse(&["status", "--json"]).command_name(), "status");
}

#[test]
fn dead_code_json_is_machine_output() {
    assert!(parse(&["dead-code", "--json"]).is_machine_output());
    assert!(!parse(&["dead-code"]).is_machine_output());
    assert_eq!(parse(&["dead-code", "--json"]).command_name(), "dead_code");
}

#[test]
fn change_context_json_is_machine_output() {
    assert!(parse(&["change-context", "--json"]).is_machine_output());
    assert!(!parse(&["change-context"]).is_machine_output());
    let cmd = parse(&["change-context", "--json", "--base-ref", "HEAD~1"]);
    assert!(cmd.is_machine_output());
    assert_eq!(cmd.command_name(), "change_context");
}

#[test]
fn session_json_is_machine_output() {
    assert!(parse(&["session", "--json"]).is_machine_output());
    assert!(!parse(&["session"]).is_machine_output());
    assert_eq!(parse(&["session"]).command_name(), "session");
    assert_eq!(parse(&["session", "--json"]).command_name(), "session");
    assert_eq!(parse(&["session", "--json"]).argv_shape(), "session|json");
}

#[test]
fn paths_comma_and_repeated_parse() {
    // Comma form
    let cli = Cli::try_parse_from([
        "ledgerful",
        "change-context",
        "--paths",
        "src/a.rs,src/b.rs",
    ])
    .expect("comma paths");
    match cli.command {
        Commands::ChangeContext(ChangeContextArgs { paths, .. }) => {
            assert_eq!(paths, vec!["src/a.rs", "src/b.rs"]);
        }
        other => panic!("expected ChangeContext, got {other:?}"),
    }
    // Repeated --paths
    let cli = Cli::try_parse_from([
        "ledgerful",
        "change-context",
        "--paths",
        "src/a.rs",
        "--paths",
        "src/b.rs",
    ])
    .expect("repeated paths");
    match cli.command {
        Commands::ChangeContext(ChangeContextArgs { paths, .. }) => {
            assert_eq!(paths, vec!["src/a.rs", "src/b.rs"]);
        }
        other => panic!("expected ChangeContext, got {other:?}"),
    }
    // Impact flags
    let cli = Cli::try_parse_from([
        "ledgerful",
        "impact",
        "--paths",
        "src/foo.rs",
        "--include-governance",
    ])
    .expect("impact paths");
    match cli.command {
        Commands::Impact(ImpactArgs {
            paths,
            include_governance,
            ..
        }) => {
            assert_eq!(paths, vec!["src/foo.rs"]);
            assert!(include_governance);
        }
        other => panic!("expected Impact, got {other:?}"),
    }
}

#[test]
fn quiet_does_not_imply_machine() {
    // Quiet is a separate state; only --json selects machine mode.
    let cli = Cli::try_parse_from(["ledgerful", "--quiet", "verify"]).unwrap();
    assert!(cli.quiet);
    assert!(!cli.command.is_machine_output());
    let cli_json = Cli::try_parse_from(["ledgerful", "--quiet", "verify", "--json"]).unwrap();
    assert!(cli_json.quiet);
    assert!(cli_json.command.is_machine_output());
}

/// 0174-B: doctor --full parses; argv_shape includes full.
#[test]
fn doctor_full_flag_parses_and_argv_shape() {
    let cli = Cli::try_parse_from(["ledgerful", "doctor", "--full"]).expect("parse --full");
    match cli.command {
        Commands::Doctor(DoctorArgs {
            json,
            apply_hook_refresh,
            dry_run,
            full,
            fix,
            yes,
        }) => {
            assert!(!json);
            assert!(!apply_hook_refresh);
            assert!(!dry_run);
            assert!(full);
            assert!(!fix);
            assert!(!yes);
        }
        other => panic!("expected Doctor, got {other:?}"),
    }
    let shape = Cli::try_parse_from(["ledgerful", "doctor", "--full", "--json"])
        .unwrap()
        .command
        .argv_shape();
    assert!(
        shape.contains("full"),
        "argv_shape must include full: {shape}"
    );
    assert!(
        shape.contains("json"),
        "argv_shape must include json: {shape}"
    );
}

/// 0226 clap matrix: `--fix` / `--yes` / `--dry-run` / hook-refresh.
#[test]
fn doctor_fix_clap_matrix() {
    assert!(
        Cli::try_parse_from(["ledgerful", "doctor", "--dry-run"]).is_err(),
        "--dry-run requires --fix or --apply-hook-refresh"
    );
    assert!(
        Cli::try_parse_from(["ledgerful", "doctor", "--fix", "--apply-hook-refresh"]).is_err(),
        "--fix conflicts with --apply-hook-refresh"
    );
    assert!(
        Cli::try_parse_from(["ledgerful", "doctor", "--yes"]).is_err(),
        "--yes requires --fix"
    );
    Cli::try_parse_from(["ledgerful", "doctor", "--fix", "--dry-run"]).expect("--fix --dry-run");
    Cli::try_parse_from(["ledgerful", "doctor", "--json", "--fix", "--dry-run"])
        .expect("--json + --fix --dry-run allowed");
    Cli::try_parse_from(["ledgerful", "doctor", "--apply-hook-refresh", "--dry-run"])
        .expect("--apply-hook-refresh --dry-run");
    let json_refresh =
        Cli::try_parse_from(["ledgerful", "doctor", "--json", "--apply-hook-refresh"]);
    assert!(
        json_refresh.is_ok(),
        "--json + --apply-hook-refresh still parses; execute_doctor rejects"
    );
    let cli = Cli::try_parse_from(["ledgerful", "doctor", "--fix", "--yes"]).expect("--fix --yes");
    match cli.command {
        Commands::Doctor(DoctorArgs {
            fix, yes, dry_run, ..
        }) => {
            assert!(fix);
            assert!(yes);
            assert!(!dry_run);
        }
        other => panic!("expected Doctor, got {other:?}"),
    }
    let shape = Cli::try_parse_from(["ledgerful", "doctor", "--fix", "--dry-run"])
        .unwrap()
        .command
        .argv_shape();
    assert!(shape.contains("fix"), "{shape}");
    assert!(shape.contains("dry-run"), "{shape}");
}

#[test]
fn re_sign_all_flag_parses() {
    let cli = Cli::try_parse_from(["ledgerful", "ledger", "re-sign", "--all", "--dry-run"])
        .expect("--all must parse");
    match cli.command {
        Commands::Ledger {
            command:
                LedgerCommands::ReSign {
                    all,
                    all_invalid,
                    tx,
                    dry_run,
                    yes,
                },
        } => {
            assert!(all);
            assert!(!all_invalid);
            assert!(tx.is_none());
            assert!(dry_run);
            assert!(!yes);
        }
        other => panic!("expected ReSign, got {other:?}"),
    }
}

#[test]
fn re_sign_all_conflicts_with_all_invalid_and_tx() {
    assert!(
        Cli::try_parse_from(["ledgerful", "ledger", "re-sign", "--all", "--all-invalid",]).is_err(),
        "--all must conflict with --all-invalid"
    );
    assert!(
        Cli::try_parse_from(["ledgerful", "ledger", "re-sign", "--all", "--tx", "abc"]).is_err(),
        "--all must conflict with --tx"
    );
    assert!(
        Cli::try_parse_from([
            "ledgerful",
            "ledger",
            "re-sign",
            "--all-invalid",
            "--tx",
            "abc",
        ])
        .is_err(),
        "--all-invalid must conflict with --tx"
    );
}

#[test]
fn scan_mode_docs_requires_impact_and_rejects_unknown() {
    use crate::cli::args::ScanImpactMode;
    let err = Cli::try_parse_from(["ledgerful", "scan", "--mode", "fast"])
        .expect_err("--mode fast must be clap-rejected")
        .to_string();
    assert!(
        err.contains("invalid value") || err.contains("possible values") || err.contains("fast"),
        "expected ValueEnum reject, got {err}"
    );

    let parsed = Cli::try_parse_from(["ledgerful", "scan", "--impact", "--mode", "docs"])
        .expect("scan --impact --mode docs");
    match parsed.command {
        Commands::Scan(ScanArgs {
            impact, mode, full, ..
        }) => {
            assert!(impact);
            assert!(matches!(mode, Some(ScanImpactMode::Docs)));
            assert!(!full);
        }
        other => panic!("expected Scan, got {other:?}"),
    }

    let with_full =
        Cli::try_parse_from(["ledgerful", "scan", "--impact", "--mode", "docs", "--full"])
            .expect("scan --impact --mode docs --full");
    match with_full.command {
        Commands::Scan(ScanArgs { full, mode, .. }) => {
            assert!(full);
            assert!(matches!(mode, Some(ScanImpactMode::Docs)));
        }
        other => panic!("expected Scan, got {other:?}"),
    }

    // `--mode docs` without `--impact` parses; execute rejects with miette
    // before gitScan (see validate_mode_requires_impact).
    let no_impact = Cli::try_parse_from(["ledgerful", "scan", "--mode", "docs"])
        .expect("parse succeeds; execute requires --impact");
    match no_impact.command {
        Commands::Scan(ScanArgs { impact, mode, .. }) => {
            assert!(!impact);
            assert!(matches!(mode, Some(ScanImpactMode::Docs)));
        }
        other => panic!("expected Scan, got {other:?}"),
    }
    assert_eq!(
        parse(&["scan", "--impact", "--mode", "docs"]).argv_shape(),
        "scan|impact,mode"
    );
}
