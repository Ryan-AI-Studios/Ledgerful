pub mod args;
pub mod category_parser;
pub mod dispatch;

pub use args::*;
pub use category_parser::{CATEGORY_LONG_HELP, CategoryValueParser};
pub use dispatch::run_with;

/// `true` when `--quiet`/`-q` or `LEDGERFUL_QUIET=1` (or `true`) is set.
///
/// Shared by logging (`main`) and doctor human progressive disclosure (0174-G)
/// so the env var is not logging-only.
pub fn resolve_quiet(cli_quiet: bool) -> bool {
    if cli_quiet {
        return true;
    }
    std::env::var("LEDGERFUL_QUIET")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

#[cfg(test)]
mod resolve_quiet_tests {
    use super::resolve_quiet;

    #[test]
    fn resolve_quiet_cli_flag_wins() {
        assert!(resolve_quiet(true));
    }

    #[test]
    fn resolve_quiet_without_flag_respects_env_absence() {
        // When LEDGERFUL_QUIET is unset, cli_quiet=false → false.
        // Do not force-unset env (may be set by agent harness); only assert
        // flag path and that false is returned when env is empty-ish.
        let from_env = std::env::var("LEDGERFUL_QUIET").ok();
        if from_env
            .as_ref()
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false)
        {
            assert!(resolve_quiet(false));
        } else {
            assert!(!resolve_quiet(false));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::{CommandFactory, Parser};

    /// Run clap Command-tree work on a 32 MiB stack thread.
    /// Windows PE default stack overflows on `Cli::command().debug_assert()`
    /// after 0043 expanded Timings + argv_shape matching (independent of
    /// RUST_MIN_STACK, which does not always resize the main thread).
    fn on_large_stack<F, R>(f: F) -> R
    where
        F: FnOnce() -> R + Send + 'static,
        R: Send + 'static,
    {
        std::thread::Builder::new()
            .stack_size(32 * 1024 * 1024)
            .name("clap-large-stack".into())
            .spawn(f)
            .expect("spawn clap test thread")
            .join()
            .expect("clap test thread panicked")
    }

    #[test]
    fn command_debug_assert() {
        // clap baseline contract test: ensures no struct/enum definition issues
        on_large_stack(|| {
            Cli::command().debug_assert();
        });
    }

    #[test]
    fn global_help_contains_ledgerful() {
        on_large_stack(|| {
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
        });
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
    fn alias_config_show_parses_as_view() {
        let cli = Cli::try_parse_from(["ledgerful", "config", "show"]).unwrap();
        match cli.command {
            Commands::Config { command } => match command {
                ConfigCommands::View { .. } => {}
                other => panic!("expected ConfigCommands::View, got {other:?}"),
            },
            _ => panic!("expected Config command"),
        }
    }

    #[test]
    fn primary_config_view_still_parses() {
        let cli = Cli::try_parse_from(["ledgerful", "config", "view"]).unwrap();
        match cli.command {
            Commands::Config { command } => match command {
                ConfigCommands::View { .. } => {}
                other => panic!("expected ConfigCommands::View, got {other:?}"),
            },
            _ => panic!("expected Config command"),
        }
    }

    #[test]
    fn alias_policy_evaluate_parses_as_check() {
        let cli = Cli::try_parse_from(["ledgerful", "policy", "evaluate"]).unwrap();
        match cli.command {
            Commands::Policy { command } => {
                let PolicyCommands::Check { .. } = command;
            }
            _ => panic!("expected Policy command"),
        }
    }

    #[test]
    fn primary_policy_check_still_parses() {
        let cli = Cli::try_parse_from(["ledgerful", "policy", "check"]).unwrap();
        match cli.command {
            Commands::Policy { command } => {
                let PolicyCommands::Check { .. } = command;
            }
            _ => panic!("expected Policy command"),
        }
    }

    #[test]
    fn alias_gate_status_parses_as_mode_not_top_level_status() {
        let cli = Cli::try_parse_from(["ledgerful", "gate", "status"]).unwrap();
        match cli.command {
            Commands::Gate { command } => {
                let GateCommands::Mode { mode, .. } = command;
                assert!(mode.is_none(), "bare gate status should not set mode");
            }
            Commands::Status { .. } => {
                panic!("gate status must not parse as top-level Status")
            }
            _ => panic!("expected Gate command"),
        }
    }

    #[test]
    fn primary_gate_mode_still_parses() {
        let cli = Cli::try_parse_from(["ledgerful", "gate", "mode"]).unwrap();
        match cli.command {
            Commands::Gate { command } => {
                let GateCommands::Mode { .. } = command;
            }
            _ => panic!("expected Gate command"),
        }
    }

    #[test]
    fn alias_ledger_list_parses_as_status() {
        let cli = Cli::try_parse_from(["ledgerful", "ledger", "list"]).unwrap();
        match cli.command {
            Commands::Ledger { command } => match command {
                LedgerCommands::Status { .. } => {}
                other => panic!("expected LedgerCommands::Status, got {other:?}"),
            },
            _ => panic!("expected Ledger command"),
        }
    }

    #[test]
    fn primary_ledger_status_still_parses() {
        let cli = Cli::try_parse_from(["ledgerful", "ledger", "status"]).unwrap();
        match cli.command {
            Commands::Ledger { command } => match command {
                LedgerCommands::Status { .. } => {}
                other => panic!("expected LedgerCommands::Status, got {other:?}"),
            },
            _ => panic!("expected Ledger command"),
        }
    }

    #[test]
    fn alias_ledger_history_parses_as_search() {
        let cli = Cli::try_parse_from(["ledgerful", "ledger", "history", "q"]).unwrap();
        match cli.command {
            Commands::Ledger { command } => match command {
                LedgerCommands::Search { query, .. } => {
                    assert_eq!(query, "q");
                }
                other => panic!("expected LedgerCommands::Search, got {other:?}"),
            },
            _ => panic!("expected Ledger command"),
        }
    }

    #[test]
    fn primary_ledger_search_still_parses() {
        let cli = Cli::try_parse_from(["ledgerful", "ledger", "search", "q"]).unwrap();
        match cli.command {
            Commands::Ledger { command } => match command {
                LedgerCommands::Search { query, .. } => {
                    assert_eq!(query, "q");
                }
                other => panic!("expected LedgerCommands::Search, got {other:?}"),
            },
            _ => panic!("expected Ledger command"),
        }
    }

    #[test]
    fn ask_multi_word_unquoted_joins_query() {
        let cli =
            Cli::try_parse_from(["ledgerful", "ask", "what", "is", "change-context"]).unwrap();
        match cli.command {
            Commands::Ask {
                query, semantic, ..
            } => {
                assert_eq!(query, vec!["what", "is", "change-context"]);
                assert_eq!(query.join(" "), "what is change-context");
                assert!(!semantic);
            }
            _ => panic!("expected Ask command"),
        }
    }

    #[test]
    fn ask_empty_query_vec() {
        let cli = Cli::try_parse_from(["ledgerful", "ask"]).unwrap();
        match cli.command {
            Commands::Ask { query, .. } => {
                assert!(query.is_empty(), "bare ask must leave query empty");
                let joined = if query.is_empty() {
                    None
                } else {
                    Some(query.join(" "))
                };
                assert_eq!(joined, None);
            }
            _ => panic!("expected Ask command"),
        }
    }

    #[test]
    fn ask_semantic_flags_first() {
        let cli =
            Cli::try_parse_from(["ledgerful", "ask", "--semantic", "what", "is", "X"]).unwrap();
        match cli.command {
            Commands::Ask {
                query, semantic, ..
            } => {
                assert!(semantic, "flags-first must set semantic=true");
                assert_eq!(query, vec!["what", "is", "X"]);
                assert!(!query.iter().any(|w| w == "--semantic"));
            }
            _ => panic!("expected Ask command"),
        }
    }

    #[test]
    fn ask_semantic_flags_after_swallowed_as_query() {
        let cli =
            Cli::try_parse_from(["ledgerful", "ask", "what", "is", "X", "--semantic"]).unwrap();
        match cli.command {
            Commands::Ask {
                query, semantic, ..
            } => {
                assert!(
                    !semantic,
                    "post-query --semantic must be swallowed as query text"
                );
                assert_eq!(query, vec!["what", "is", "X", "--semantic"]);
            }
            _ => panic!("expected Ask command"),
        }
    }

    /// 0158 M7: argv fingerprint marks `timeout` only when the flag is present
    /// (not via a hardcoded default of 15).
    #[test]
    fn ask_timeout_fingerprint_only_when_flag_present() {
        let without = Cli::try_parse_from(["ledgerful", "ask", "hello"]).unwrap();
        match &without.command {
            Commands::Ask { timeout, .. } => {
                assert_eq!(*timeout, None, "omitted --timeout must be None");
            }
            _ => panic!("expected Ask"),
        }
        assert!(
            !without.command.argv_shape().contains("timeout"),
            "omitted timeout must not appear in argv shape: {}",
            without.command.argv_shape()
        );

        let with = Cli::try_parse_from(["ledgerful", "ask", "--timeout", "30", "hello"]).unwrap();
        match &with.command {
            Commands::Ask { timeout, .. } => {
                assert_eq!(*timeout, Some(30));
            }
            _ => panic!("expected Ask"),
        }
        assert!(
            with.command.argv_shape().contains("timeout"),
            "explicit --timeout must appear in argv shape: {}",
            with.command.argv_shape()
        );
    }

    #[test]
    fn update_check_alias_with_binary_sets_dry_run() {
        let cli = Cli::try_parse_from(["ledgerful", "update", "--check", "--binary"]).unwrap();
        match cli.command {
            Commands::Update {
                dry_run,
                binary,
                migrate,
                repair_hooks,
                ..
            } => {
                assert!(dry_run, "--check must alias to dry_run");
                assert!(binary);
                assert!(!migrate);
                assert!(!repair_hooks);
            }
            _ => panic!("expected Update command"),
        }
    }

    #[test]
    fn update_check_alone_sets_dry_run_no_action() {
        let cli = Cli::try_parse_from(["ledgerful", "update", "--check"]).unwrap();
        match cli.command {
            Commands::Update {
                dry_run,
                binary,
                migrate,
                repair_hooks,
                ..
            } => {
                assert!(dry_run, "--check alone must set dry_run");
                assert!(!binary);
                assert!(!migrate);
                assert!(!repair_hooks);
            }
            _ => panic!("expected Update command"),
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
            command: Commands::Init {
                force: false,
                enforce: false,
            },
            verbose: false,
            quiet: false,
        };
        let _: LedgerCommands = LedgerCommands::Status {
            entity: None,
            compact: false,
            exit_code: false,
            strict_observe_signal: false,
            verify_signatures: false,
            json: false,
            all: false,
            global: false,
            repo: None,
            reindex: false,
            opt_out: false,
            opt_in: false,
        };
        let _: ConfigCommands = ConfigCommands::Verify {
            json: false,
            section: None,
            verbose: false,
        };
        let _: FederateCommands = FederateCommands::Status;
        let _: ExportCommands = ExportCommands::Evidence {
            profile: "soc2".to_string(),
            out: None,
            force: false,
            control: Vec::new(),
        };
        let _: ExportCommands = ExportCommands::Head {
            out: None,
            force: false,
        };
        let _: Commands = Commands::Demo {
            keep: false,
            output: None,
            force: false,
        };
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
