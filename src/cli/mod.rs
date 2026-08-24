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
            Commands::Viz(VizArgs { output, .. }) => {
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
            Commands::Update(UpdateArgs { dry_run, .. }) => {
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
                let Some(PolicyCommands::Check { .. }) = command else {
                    panic!("expected Some(PolicyCommands::Check), got {command:?}");
                };
            }
            _ => panic!("expected Policy command"),
        }
    }

    #[test]
    fn primary_policy_check_still_parses() {
        let cli = Cli::try_parse_from(["ledgerful", "policy", "check"]).unwrap();
        match cli.command {
            Commands::Policy { command } => {
                let Some(PolicyCommands::Check { .. }) = command else {
                    panic!("expected Some(PolicyCommands::Check), got {command:?}");
                };
            }
            _ => panic!("expected Policy command"),
        }
    }

    #[test]
    fn alias_gate_status_parses_as_mode_not_top_level_status() {
        let cli = Cli::try_parse_from(["ledgerful", "gate", "status"]).unwrap();
        match cli.command {
            Commands::Gate { command } => {
                let Some(GateCommands::Mode { mode, .. }) = command else {
                    panic!("expected Some(GateCommands::Mode), got {command:?}");
                };
                assert!(mode.is_none(), "bare gate status should not set mode");
            }
            Commands::Status(StatusArgs { .. }) => {
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
                let Some(GateCommands::Mode { .. }) = command else {
                    panic!("expected Some(GateCommands::Mode), got {command:?}");
                };
            }
            _ => panic!("expected Gate command"),
        }
    }

    // --- 0179: bare parent default subcommands (safe read-only) ---

    #[test]
    fn bare_parent_defaults_parse_ok() {
        for parent in [
            "dependencies",
            "policy",
            "gate",
            "ci",
            "deploy",
            "federate",
            "services",
            "release",
        ] {
            let cli = Cli::try_parse_from(["ledgerful", parent]).unwrap_or_else(|e| {
                panic!("bare `{parent}` must parse without missing-subcommand: {e}")
            });
            // Sanity: parsed command group matches parent name prefix.
            let name = cli.command.command_name();
            assert!(
                name.starts_with(parent) || name == parent || name.contains(parent),
                "bare `{parent}` command_name={name}"
            );
        }
        #[cfg(feature = "mcp")]
        {
            let cli = Cli::try_parse_from(["ledgerful", "mcp"]).unwrap_or_else(|e| {
                panic!("bare `mcp` must parse without missing-subcommand: {e}")
            });
            let name = cli.command.command_name();
            assert!(
                name.starts_with("mcp") || name == "mcp" || name.contains("mcp"),
                "bare `mcp` command_name={name}"
            );
        }
    }

    #[test]
    fn bare_gate_defaults_to_mode_show_never_set() {
        let cli = Cli::try_parse_from(["ledgerful", "gate"]).expect("bare gate parses");
        match cli.command {
            Commands::Gate { command: None } => {
                // Outer None is the bare default; resolve path uses Mode { mode: None }.
            }
            Commands::Gate {
                command: Some(GateCommands::Mode { mode: Some(_) }),
            } => panic!("bare gate must never parse as mode-set"),
            other => panic!("expected Gate {{ command: None }}, got {other:?}"),
        }
        assert_eq!(
            Cli::try_parse_from(["ledgerful", "gate"])
                .expect("parse")
                .command
                .command_name(),
            "gate_mode_show"
        );
        // Explicit set still works.
        let set = Cli::try_parse_from(["ledgerful", "gate", "mode", "enforce"]).expect("set");
        assert_eq!(set.command.command_name(), "gate_mode_set");
    }

    #[test]
    fn bare_federate_defaults_to_status_not_export() {
        let cli = Cli::try_parse_from(["ledgerful", "federate"]).expect("bare federate");
        assert_eq!(cli.command.command_name(), "federate_status");
        match &cli.command {
            Commands::Federate { command: None } => {}
            Commands::Federate {
                command: Some(FederateCommands::Export { .. }),
            } => panic!("bare federate must not default to Export (writes)"),
            other => panic!("expected Federate {{ command: None }}, got {other:?}"),
        }
        // Explicit export still available.
        let exp = Cli::try_parse_from(["ledgerful", "federate", "export"]).expect("export");
        assert_eq!(exp.command.command_name(), "federate_export");
    }

    #[test]
    fn bare_dependencies_defaults_to_list_flags() {
        use crate::commands::dependencies::DependencySubcommands;
        let cli = Cli::try_parse_from(["ledgerful", "dependencies"]).expect("bare deps");
        match cli.command {
            Commands::Dependencies(args) => {
                assert!(args.command.is_none(), "bare dependencies is Option None");
                match args.command_or_default() {
                    DependencySubcommands::List {
                        json: false,
                        verbose: false,
                        all: false,
                    } => {}
                    other => panic!("expected List defaults, got {other:?}"),
                }
            }
            other => panic!("expected Dependencies, got {other:?}"),
        }
    }

    #[test]
    fn bare_policy_ci_deploy_command_names() {
        assert_eq!(
            Cli::try_parse_from(["ledgerful", "policy"])
                .expect("policy")
                .command
                .command_name(),
            "policy_check"
        );
        assert_eq!(
            Cli::try_parse_from(["ledgerful", "ci"])
                .expect("ci")
                .command
                .command_name(),
            "ci"
        );
        assert_eq!(
            Cli::try_parse_from(["ledgerful", "deploy"])
                .expect("deploy")
                .command
                .command_name(),
            "deploy"
        );
    }

    #[test]
    fn parent_flag_passthrough_still_fails_b5() {
        // B5: no parent-level flag passthrough — agents use explicit subcommand.
        assert!(
            Cli::try_parse_from(["ledgerful", "dependencies", "--json"]).is_err(),
            "dependencies --json must remain a clap error (use `dependencies list --json`)"
        );
    }

    #[test]
    fn bare_services_soft_default_to_diff() {
        let cli = Cli::try_parse_from(["ledgerful", "services"]).expect("bare services");
        match cli.command {
            Commands::Services { command: None } => {}
            other => panic!("expected Services {{ command: None }}, got {other:?}"),
        }
        assert_eq!(cli.command.command_name(), "services_diff");
    }

    #[test]
    fn clap_release_defaults_to_pins() {
        let cli = Cli::try_parse_from(["ledgerful", "release"]).expect("bare release parses");
        match &cli.command {
            Commands::Release {
                json: false,
                command: None,
            } => {}
            other => panic!("expected Release {{ json: false, command: None }}, got {other:?}"),
        }
        assert_eq!(cli.command.command_name(), "release_pins");
        assert!(!cli.command.is_machine_output());
    }

    #[test]
    fn clap_release_json_on_parent_and_subcommand() {
        let parent = Cli::try_parse_from(["ledgerful", "release", "--json"])
            .expect("release --json must parse");
        match &parent.command {
            Commands::Release {
                json: true,
                command: None,
            } => {}
            other => panic!("release --json: {other:?}"),
        }
        assert!(parent.command.is_machine_output());

        let sub = Cli::try_parse_from(["ledgerful", "release", "pins", "--json"])
            .expect("release pins --json must parse");
        match &sub.command {
            Commands::Release {
                json: true,
                command: Some(ReleaseCommands::Pins),
            } => {}
            other => panic!("release pins --json: {other:?}"),
        }
        assert!(sub.command.is_machine_output());
    }

    #[test]
    fn argv_shape_release_json_eq_release_pins_json() {
        let parent = Cli::try_parse_from(["ledgerful", "release", "--json"]).expect("parent json");
        let sub =
            Cli::try_parse_from(["ledgerful", "release", "pins", "--json"]).expect("sub json");
        assert_eq!(parent.command.argv_shape(), "release_pins|json");
        assert_eq!(parent.command.argv_shape(), sub.command.argv_shape());
    }

    #[test]
    fn clap_release_pins_help_mentions_json() {
        let result = Cli::try_parse_from(["ledgerful", "release", "--help"]);
        assert!(
            result.is_err(),
            "--help should trigger clap's special error"
        );
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("pins"),
            "release help must mention pins: {err}"
        );
        assert!(
            err.contains("--json"),
            "release help must mention --json: {err}"
        );
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
                LedgerCommands::Search {
                    query,
                    include_rollback,
                    ..
                } => {
                    assert_eq!(query, "q");
                    assert!(
                        !include_rollback,
                        "omitted --include-rollback must default false"
                    );
                }
                other => panic!("expected LedgerCommands::Search, got {other:?}"),
            },
            _ => panic!("expected Ledger command"),
        }

        let cli =
            Cli::try_parse_from(["ledgerful", "ledger", "history", "q", "--include-rollback"])
                .unwrap();
        match cli.command {
            Commands::Ledger { command } => match command {
                LedgerCommands::Search {
                    query,
                    include_rollback,
                    ..
                } => {
                    assert_eq!(query, "q");
                    assert!(include_rollback);
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
                LedgerCommands::Search {
                    query,
                    include_rollback,
                    ..
                } => {
                    assert_eq!(query, "q");
                    assert!(
                        !include_rollback,
                        "omitted --include-rollback must default false"
                    );
                }
                other => panic!("expected LedgerCommands::Search, got {other:?}"),
            },
            _ => panic!("expected Ledger command"),
        }

        let cli = Cli::try_parse_from(["ledgerful", "ledger", "search", "q", "--include-rollback"])
            .unwrap();
        match cli.command {
            Commands::Ledger { command } => match command {
                LedgerCommands::Search {
                    query,
                    include_rollback,
                    ..
                } => {
                    assert_eq!(query, "q");
                    assert!(include_rollback);
                }
                other => panic!("expected LedgerCommands::Search, got {other:?}"),
            },
            _ => panic!("expected Ledger command"),
        }
    }

    #[test]
    fn search_multi_token_unquoted_joins_query() {
        let cli = Cli::try_parse_from(["ledgerful", "search", "foo", "bar"]).unwrap();
        match cli.command {
            Commands::Search(SearchCliArgs { query, json, .. }) => {
                assert_eq!(query, vec!["foo", "bar"]);
                assert_eq!(query.join(" "), "foo bar");
                assert!(!json);
            }
            _ => panic!("expected Search command"),
        }
    }

    #[test]
    fn search_json_flag_before_tokens() {
        let cli = Cli::try_parse_from(["ledgerful", "search", "--json", "foo", "bar"]).unwrap();
        match cli.command {
            Commands::Search(SearchCliArgs { query, json, .. }) => {
                assert!(json);
                assert_eq!(query, vec!["foo", "bar"]);
                assert_eq!(query.join(" "), "foo bar");
            }
            _ => panic!("expected Search command"),
        }
    }

    #[test]
    fn search_json_flag_after_tokens() {
        let cli = Cli::try_parse_from(["ledgerful", "search", "foo", "bar", "--json"]).unwrap();
        match cli.command {
            Commands::Search(SearchCliArgs { query, json, .. }) => {
                assert!(json);
                assert_eq!(query, vec!["foo", "bar"]);
                assert_eq!(query.join(" "), "foo bar");
            }
            _ => panic!("expected Search command"),
        }
    }

    #[test]
    fn search_json_flag_between_tokens() {
        let cli = Cli::try_parse_from(["ledgerful", "search", "foo", "--json", "bar"]).unwrap();
        match cli.command {
            Commands::Search(SearchCliArgs { query, json, .. }) => {
                assert!(json);
                assert_eq!(query, vec!["foo", "bar"]);
                assert_eq!(query.join(" "), "foo bar");
            }
            _ => panic!("expected Search command"),
        }
    }

    #[test]
    fn search_limit_flag_between_tokens() {
        let cli =
            Cli::try_parse_from(["ledgerful", "search", "foo", "--limit", "5", "bar"]).unwrap();
        match cli.command {
            Commands::Search(SearchCliArgs {
                query, limit, json, ..
            }) => {
                assert_eq!(query, vec!["foo", "bar"]);
                assert_eq!(query.join(" "), "foo bar");
                assert_eq!(limit, 5);
                assert!(!json);
            }
            _ => panic!("expected Search command"),
        }
    }

    #[test]
    fn search_quoted_two_word_single_token() {
        let cli = Cli::try_parse_from(["ledgerful", "search", "--json", "foo bar"]).unwrap();
        match cli.command {
            Commands::Search(SearchCliArgs { query, json, .. }) => {
                assert!(json);
                assert_eq!(query, vec!["foo bar"]);
                assert_eq!(query.join(" "), "foo bar");
            }
            _ => panic!("expected Search command"),
        }
    }

    #[test]
    fn search_missing_query_is_required() {
        let err = Cli::try_parse_from(["ledgerful", "search"]).unwrap_err();
        assert_eq!(
            err.kind(),
            clap::error::ErrorKind::MissingRequiredArgument,
            "empty search must fail closed via required=true: {err}"
        );
        let rendered = err.to_string();
        assert!(
            rendered.contains("<QUERY>") || rendered.contains("QUERY"),
            "missing query usage should name QUERY: {rendered}"
        );
    }

    #[test]
    fn search_unknown_flag_is_error() {
        let err = Cli::try_parse_from(["ledgerful", "search", "foo", "--nope"]).unwrap_err();
        assert_eq!(
            err.kind(),
            clap::error::ErrorKind::UnknownArgument,
            "unknown flag must stay fail-closed: {err}"
        );
    }

    #[test]
    fn search_hyphen_leading_without_separator_is_error() {
        let err = Cli::try_parse_from(["ledgerful", "search", "--not-a-search-flag"]).unwrap_err();
        assert_eq!(
            err.kind(),
            clap::error::ErrorKind::UnknownArgument,
            "hyphen-leading token without -- must not become query text: {err}"
        );
    }

    #[test]
    fn search_end_of_options_keeps_hyphen_token() {
        let cli = Cli::try_parse_from(["ledgerful", "search", "--", "--json"]).unwrap();
        match cli.command {
            Commands::Search(SearchCliArgs { query, json, .. }) => {
                assert_eq!(query, vec!["--json"]);
                assert_eq!(query.join(" "), "--json");
                assert!(!json, "token after -- must not select json mode");
            }
            _ => panic!("expected Search command"),
        }
    }

    #[test]
    fn search_identifier_token_stays_out_of_argv_shape() {
        let cli = Cli::try_parse_from(["ledgerful", "search", "verify_step_key"]).unwrap();
        match &cli.command {
            Commands::Search(SearchCliArgs { query, .. }) => {
                assert_eq!(query, &vec!["verify_step_key".to_string()]);
            }
            _ => panic!("expected Search command"),
        }
        assert_eq!(
            cli.command.argv_shape(),
            "search",
            "query text must not enter present_flag_names: {}",
            cli.command.argv_shape()
        );
    }

    #[test]
    fn ask_multi_word_unquoted_joins_query() {
        let cli =
            Cli::try_parse_from(["ledgerful", "ask", "what", "is", "change-context"]).unwrap();
        match cli.command {
            Commands::Ask(AskArgs {
                query, semantic, ..
            }) => {
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
            Commands::Ask(AskArgs { query, .. }) => {
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
            Commands::Ask(AskArgs {
                query, semantic, ..
            }) => {
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
            Commands::Ask(AskArgs {
                query, semantic, ..
            }) => {
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
            Commands::Ask(AskArgs { timeout, .. }) => {
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
            Commands::Ask(AskArgs { timeout, .. }) => {
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
            Commands::Update(UpdateArgs {
                dry_run,
                binary,
                migrate,
                repair_hooks,
                ..
            }) => {
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
            Commands::Update(UpdateArgs {
                dry_run,
                binary,
                migrate,
                repair_hooks,
                ..
            }) => {
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
            Commands::Verify(VerifyArgs { dry_run, .. }) => {
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

    #[test]
    fn directory_flag_last_wins() {
        let cli = Cli::try_parse_from(["ledgerful", "-C", "first", "-C", "second", "status"])
            .expect("repeated -C should parse");
        assert_eq!(
            cli.directory.as_deref(),
            Some("second"),
            "clap Option last-wins; do not stack relative -C like git"
        );
    }

    #[test]
    fn directory_long_form_parses() {
        let cli = Cli::try_parse_from(["ledgerful", "--directory", "C:\\dev\\web", "status"])
            .expect("--directory should parse");
        assert_eq!(cli.directory.as_deref(), Some("C:\\dev\\web"));
    }

    #[test]
    fn top_level_status_compact_parses() {
        let cli = Cli::try_parse_from(["ledgerful", "status", "--compact"]).unwrap();
        match cli.command {
            Commands::Status(StatusArgs { json, compact }) => {
                assert!(compact);
                assert!(!json);
            }
            other => panic!("expected Status, got {other:?}"),
        }
    }

    #[test]
    fn top_level_status_help_lists_compact_not_global() {
        let result = Cli::try_parse_from(["ledgerful", "status", "--help"]);
        assert!(
            result.is_err(),
            "--help should trigger clap's special error"
        );
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("--compact"),
            "status help must mention --compact; got {err}"
        );
        assert!(
            err.contains("--json"),
            "status help must mention --json; got {err}"
        );
        assert!(
            !err.contains("--global"),
            "top-level status is not --global; got {err}"
        );
        assert!(
            !err.contains("--all"),
            "top-level status is not --all; got {err}"
        );
    }

    /// Compile-time/API contract test: prove all key facade exports remain public.
    #[test]
    fn facade_exports_reachable() {
        // If this test compiles, the facade re-exports are intact.
        let _: Cli = Cli {
            command: Commands::Init(InitArgs {
                force: false,
                enforce: false,
            }),
            verbose: false,
            quiet: false,
            directory: None,
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
            stdout: false,
        };
        let _: Commands = Commands::Demo(DemoArgs {
            keep: false,
            output: None,
            force: false,
        });
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
            Commands::Update(UpdateArgs { force_unlock, .. }) => {
                assert!(force_unlock, "--force-unlock must set force_unlock = true");
            }
            _ => panic!("expected Update command"),
        }
    }

    #[test]
    fn no_graph_sync_flag_parses() {
        let cli = Cli::try_parse_from(["ledgerful", "watch", "--no-graph-sync"]).unwrap();
        match cli.command {
            Commands::Watch(WatchArgs { no_graph_sync, .. }) => {
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
