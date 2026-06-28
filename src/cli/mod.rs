pub mod args;
pub mod dispatch;

pub use args::*;
pub use dispatch::run_with;

#[cfg(test)]
mod tests {
    use super::*;
    use clap::{CommandFactory, Parser};

    #[test]
    fn command_debug_assert() {
        // clap baseline contract test: ensures no struct/enum definition issues
        Cli::command().debug_assert();
    }

    #[test]
    fn global_help_contains_ledgerful() {
        let mut cmd = Cli::command();
        let mut buf = Vec::new();
        cmd.write_help(&mut buf).unwrap();
        let help = String::from_utf8(buf).unwrap();
        assert!(
            help.contains("Ledgerful"),
            "global help must mention Ledgerful"
        );
        assert!(
            help.contains("scan"),
            "global help must list scan subcommand"
        );
        assert!(
            help.contains("ledger"),
            "global help must list ledger subcommand"
        );
    }

    #[test]
    fn scan_help_is_valid() {
        let result = Cli::try_parse_from(["ledgerful", "scan", "--help"]);
        assert!(
            result.is_err(),
            "--help should trigger clap's special error (success path)"
        );
        let err = result.unwrap_err().to_string();
        assert!(err.contains("scan"), "scan help must mention scan");
        assert!(err.contains("--impact"), "scan help must mention --impact");
    }

    #[test]
    fn ledger_status_help_is_valid() {
        let result = Cli::try_parse_from(["ledgerful", "ledger", "status", "--help"]);
        assert!(
            result.is_err(),
            "--help should trigger clap's special error"
        );
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("status"),
            "ledger status help must mention status"
        );
        assert!(
            err.contains("--compact"),
            "ledger status help must mention --compact"
        );
    }

    #[test]
    fn alias_out_for_viz_output() {
        let cli = Cli::try_parse_from(["ledgerful", "viz", "--out", "output.html"]).unwrap();
        match cli.command {
            Commands::Viz { output, .. } => {
                assert_eq!(output.as_deref(), Some("output.html"));
            }
            _ => panic!("expected Viz command"),
        }
    }

    #[test]
    fn alias_output_dir_for_adr_export() {
        let cli = Cli::try_parse_from([
            "ledgerful",
            "ledger",
            "adr",
            "export",
            "--output-dir",
            "docs/decisions",
        ])
        .unwrap();
        match cli.command {
            Commands::Ledger { command } => match command {
                LedgerCommands::Adr { command } => match command {
                    AdrSubcommands::Export { output, .. } => {
                        assert_eq!(output, "docs/decisions");
                    }
                    _ => panic!("expected Export subcommand"),
                },
                _ => panic!("expected Adr subcommand"),
            },
            _ => panic!("expected Ledger command"),
        }
    }

    #[test]
    fn update_visible_alias_upgrade() {
        let cli = Cli::try_parse_from(["ledgerful", "upgrade", "--dry-run"]).unwrap();
        match cli.command {
            Commands::Update { dry_run, .. } => {
                assert!(
                    dry_run,
                    "upgrade alias must map to Update with dry_run true"
                );
            }
            _ => panic!("expected Update command via upgrade alias"),
        }
    }

    #[test]
    fn verify_alias_dry_run() {
        let cli = Cli::try_parse_from(["ledgerful", "verify", "--dry-run"]).unwrap();
        match cli.command {
            Commands::Verify { dry_run, .. } => {
                assert!(dry_run, "--dry-run must be parsed as dry_run = true");
            }
            _ => panic!("expected Verify command"),
        }
    }

    #[test]
    fn index_help_contains_fast() {
        let result = Cli::try_parse_from(["ledgerful", "index", "--help"]);
        assert!(
            result.is_err(),
            "--help should trigger clap's special error"
        );
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("--fast"),
            "index help must mention --fast flag"
        );
    }

    /// Compile-time/API contract test: prove all key facade exports remain public.
    #[test]
    fn facade_exports_reachable() {
        // If this test compiles, the facade re-exports are intact.
        let _: Cli = Cli {
            command: Commands::Init { force: false },
            verbose: false,
        };
        let _: LedgerCommands = LedgerCommands::Status {
            entity: None,
            compact: false,
            exit_code: false,
            verify_signatures: false,
            json: false,
            all: false,
        };
        let _: ConfigCommands = ConfigCommands::Verify {
            json: false,
            section: None,
            verbose: false,
        };
        let _: FederateCommands = FederateCommands::Status;
        let _: IntentCommands = IntentCommands::Demo;
        let _: InternalCommands = InternalCommands::HookPostCommit;
        let _: ServiceSubcommands =
            ServiceSubcommands::Diff(crate::commands::services_diff::ServicesDiffArgs {
                full: false,
                json: false,
            });
        let _: RegisterCommands = RegisterCommands::Rule {
            term: String::new(),
            category: crate::ledger::types::Category::Refactor,
            reason: String::new(),
        };
    }

    #[test]
    fn data_models_command_parses() {
        let result = Cli::try_parse_from(["ledgerful", "data-models", "--help"]);
        assert!(
            result.is_err(),
            "--help should trigger clap's special error"
        );
        let err = result.unwrap_err().to_string();
        assert!(err.contains("data-models"), "help must mention data-models");
    }

    #[test]
    fn verify_signatures_flag_parses() {
        let cli =
            Cli::try_parse_from(["ledgerful", "ledger", "status", "--verify-signatures"]).unwrap();
        match cli.command {
            Commands::Ledger { command } => match command {
                LedgerCommands::Status {
                    verify_signatures, ..
                } => {
                    assert!(
                        verify_signatures,
                        "--verify-signatures must set verify_signatures = true"
                    );
                }
                _ => panic!("expected Status subcommand"),
            },
            _ => panic!("expected Ledger command"),
        }
    }

    #[test]
    fn force_unlock_flag_parses() {
        let cli = Cli::try_parse_from(["ledgerful", "update", "--force-unlock"]).unwrap();
        match cli.command {
            Commands::Update { force_unlock, .. } => {
                assert!(force_unlock, "--force-unlock must set force_unlock = true");
            }
            _ => panic!("expected Update command"),
        }
    }

    #[test]
    fn no_graph_sync_flag_parses() {
        let cli = Cli::try_parse_from(["ledgerful", "watch", "--no-graph-sync"]).unwrap();
        match cli.command {
            Commands::Watch { no_graph_sync, .. } => {
                assert!(
                    no_graph_sync,
                    "--no-graph-sync must set no_graph_sync = true"
                );
            }
            _ => panic!("expected Watch command"),
        }
    }

    #[test]
    fn internal_hook_commands_parse() {
        let cli = Cli::try_parse_from([
            "ledgerful",
            "internal",
            "hook-commit-msg",
            ".git/COMMIT_EDITMSG",
        ])
        .unwrap();
        match cli.command {
            Commands::Internal { command } => match command {
                InternalCommands::HookCommitMsg { msg_file } => {
                    assert_eq!(msg_file, std::path::PathBuf::from(".git/COMMIT_EDITMSG"));
                }
                _ => panic!("expected HookCommitMsg subcommand"),
            },
            _ => panic!("expected Internal command"),
        }

        let cli = Cli::try_parse_from(["ledgerful", "internal", "hook-post-commit"]).unwrap();
        match cli.command {
            Commands::Internal { command } => match command {
                InternalCommands::HookPostCommit => {}
                _ => panic!("expected HookPostCommit subcommand"),
            },
            _ => panic!("expected Internal command"),
        }
    }
}
