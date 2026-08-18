use crate::cli::args::{
    AskArgs, AuditArgs, ChangeContextArgs, Cli, Commands, DeadCodeArgs, DemoArgs, DoctorArgs,
    ImpactArgs, InitArgs, LedgerCommands, ResetArgs, ScanArgs, SearchTrigramsArgs, SetupArgs,
    StatusArgs, SurfacesArgs, TimingsCliArgs, UpdateArgs, VizArgs, WatchArgs,
};
use miette::{IntoDiagnostic, Result};
use std::env;

mod config;
mod defaults;
mod export;
mod index;
mod ledger;
mod ops;
mod search;
mod verify;

use config::dispatch_config;
use defaults::{
    dispatch_federate, dispatch_gate, dispatch_policy, dispatch_services, federate_or_default,
    gate_or_default, policy_or_default, services_or_default,
};
use export::dispatch_export;
use index::dispatch_index;
use ledger::dispatch_ledger;
#[cfg(feature = "mcp")]
use ops::dispatch_mcp;
#[cfg(feature = "usage-metrics")]
use ops::dispatch_usage;
use ops::{dispatch_intent, dispatch_internal, dispatch_schedule};
use search::dispatch_search;
use verify::dispatch_verify;

pub fn run_with(cli: Cli) -> Result<()> {
    let current_dir = env::current_dir().into_diagnostic()?;

    // Global commands must not mutate the current repository before dispatch.
    // `load_startup_config` migrates legacy state (retired brand dir ->
    // `.ledgerful`) against the *resolved* state path, which would violate the
    // read-only invariant for `ledger status --global` and `timings --global`.
    // For global commands we load only the user-level config; the per-repo
    // global path does not need the current repo's config.
    //
    // Non-global: resolve via `get_layout()` (discover + shared state for linked
    // worktrees). Never use raw cwd as repo root — nested dirs used to create
    // orphan `{subdir}/.ledgerful` trees.
    let config = if is_global_command(&cli.command) {
        load_user_config().unwrap_or_default()
    } else if env::var(crate::commands::helpers::LEDGERFUL_STATE_DIR_ENV).is_ok() {
        // Override present: fail closed on relative/empty/invalid values.
        let layout = crate::commands::helpers::get_layout()?;
        load_startup_config(&layout)?
    } else {
        match crate::commands::helpers::get_layout() {
            Ok(layout) => load_startup_config(&layout)?,
            Err(e) => {
                tracing::debug!("Skipping per-repo startup config (no resolvable worktree): {e}");
                Default::default()
            }
        }
    };

    // Capture the command name for usage metrics before moving `cli.command`
    #[cfg(feature = "usage-metrics")]
    let command_name = cli.command.command_name();

    // Self-timing capture (Track 0043): RAII guard; drop flushes one batch.
    #[cfg(feature = "self-timing")]
    let timed = {
        let name = cli.command.command_name();
        let shape = cli.command.argv_shape();
        crate::observability::self_timing::TimedCommand::start(name, &shape)
    };

    let result = match cli.command {
        Commands::Init(InitArgs { force, enforce }) => {
            crate::commands::init::execute_init(force, enforce)
        }
        Commands::Gate { command } => dispatch_gate(gate_or_default(command)),
        Commands::Policy { command } => dispatch_policy(policy_or_default(command)),
        Commands::Setup(SetupArgs { yes, skip_scan }) => {
            crate::commands::setup::execute_setup(yes, skip_scan)
        }
        Commands::Scan(ScanArgs {
            impact,
            summary,
            json,
            out,
            base_ref,
            pr,
            format,
            blast_depth,
            paths,
            include_governance,
        }) => crate::commands::scan::execute_scan_with_opts(
            impact,
            summary,
            json,
            out,
            base_ref,
            pr,
            format,
            blast_depth,
            paths,
            include_governance,
        ),
        Commands::Impact(ImpactArgs {
            all_parents,
            summary,
            telemetry,
            dead_code,
            json,
            out,
            blast_depth,
            paths,
            include_governance,
        }) => crate::commands::impact::execute_impact_with_opts(
            all_parents,
            summary,
            telemetry,
            dead_code,
            json,
            out,
            blast_depth,
            paths,
            include_governance,
        ),
        Commands::ChangeContext(ChangeContextArgs {
            json,
            detail,
            max_files,
            base_ref,
            blast_depth,
            paths,
            include_governance,
        }) => {
            let opts = crate::commands::change_context::ChangeContextOpts::from_cli(
                Some(detail),
                Some(max_files),
                base_ref,
                blast_depth,
                paths,
                include_governance,
            )?;
            crate::commands::change_context::execute_change_context(opts, json)
        }
        Commands::Index(args) => dispatch_index(args).or_else(handle_schema_error),
        Commands::Search(args) => dispatch_search(current_dir, args).or_else(handle_schema_error),
        Commands::Hotspots { args } => crate::commands::hotspots::execute_hotspots(args),
        Commands::Endpoints(args) => crate::commands::endpoints::execute_endpoints(args),
        Commands::Symbols(args) => crate::commands::symbols::execute_symbols(args),
        Commands::Surfaces(SurfacesArgs { json }) => {
            crate::commands::surfaces::execute_surfaces(json)
        }
        Commands::Federate { command } => dispatch_federate(federate_or_default(command)),
        Commands::Bridge { subcommand } => crate::commands::bridge::execute(subcommand),
        Commands::Export { command } => dispatch_export(command),
        Commands::Services { command } => dispatch_services(services_or_default(command), &config),
        Commands::DataModels(args) => crate::commands::data_models::execute_data_models(args),
        Commands::Ci(args) => crate::commands::deploy::execute_ci(args),
        Commands::Deploy(args) => crate::commands::deploy::execute_deploy(args),
        Commands::Dependencies(args) => crate::commands::dependencies::execute_dependencies(args),
        Commands::Observability(args) => {
            crate::commands::observability::execute_observability(args)
        }
        Commands::Security(args) => crate::commands::security::execute_security(args),
        Commands::Tests(args) => crate::commands::test_mapping::execute_tests_for_entity(args),
        Commands::Ledger { command } => dispatch_ledger(command),
        #[cfg(any(feature = "openapi", feature = "web"))]
        Commands::Openapi => {
            let json = crate::commands::web::api::generate_openapi_json();
            println!("{}", json);
            return Ok(());
        }
        Commands::Verify(args) => {
            let layout = crate::commands::helpers::get_layout()?;
            dispatch_verify(&layout, args, cli.verbose)
        }
        Commands::Ask(AskArgs {
            query,
            semantic,
            limit,
            mode,
            narrative,
            backend,
            auto_index,
            timeout,
            no_kg_fallback,
            auto_scan,
        }) => {
            let query = if query.is_empty() {
                None
            } else {
                Some(query.join(" "))
            };
            crate::commands::ask::execute_ask(crate::commands::ask::ExecuteAskOpts {
                query,
                semantic,
                limit,
                mode,
                narrative,
                backend,
                auto_index,
                timeout_secs: timeout,
                no_kg_fallback,
                auto_scan,
            })
            .or_else(handle_schema_error)
        }
        Commands::Intent { command } => dispatch_intent(command),
        Commands::Reset(ResetArgs {
            remove_config,
            remove_rules,
            include_ledger,
            all,
            yes,
            dry_run,
        }) => crate::commands::reset::execute_reset(
            remove_config,
            remove_rules,
            include_ledger,
            all,
            yes,
            dry_run,
        ),
        Commands::Doctor(DoctorArgs {
            json,
            apply_hook_refresh,
            dry_run,
            full,
        }) => crate::commands::doctor::execute_doctor(
            json,
            apply_hook_refresh,
            dry_run,
            full,
            crate::cli::resolve_quiet(cli.quiet),
        ),
        Commands::Status(StatusArgs { json }) => crate::commands::ledger::execute_ledger_status(
            crate::commands::ledger::LedgerStatusOpts {
                entity_filter: None,
                compact: false,
                exit_code: false,
                verify_signatures: false,
                json,
                all: false,
                strict_observe_signal: false,
            },
        ),
        Commands::Config { command } => dispatch_config(command, cli.verbose),
        Commands::DeadCode(DeadCodeArgs {
            threshold,
            limit,
            auto_index,
            include_traits,
            prune,
            expand,
            explain,
            json,
        }) => crate::commands::dead_code::execute_dead_code(
            threshold,
            limit,
            auto_index,
            include_traits,
            prune,
            expand,
            explain,
            json,
        ),
        Commands::Viz(VizArgs {
            output,
            limit,
            depth,
            entity,
            view,
        }) => {
            let path = output.map(std::path::PathBuf::from);
            crate::commands::viz::execute_viz(path, limit, depth, entity, view)
        }
        Commands::Update(UpdateArgs {
            migrate,
            binary,
            force,
            force_unlock,
            fast,
            dry_run,
            repair_hooks,
        }) => crate::commands::update::execute_update(
            migrate,
            binary,
            force,
            force_unlock,
            fast,
            dry_run,
            repair_hooks,
        ),
        Commands::Watch(WatchArgs {
            interval,
            json,
            no_graph_sync,
        }) => crate::commands::watch::execute_watch(interval, json, no_graph_sync),
        #[cfg(feature = "sync")]
        Commands::Sync { subcommand } => crate::commands::sync::handle(subcommand),
        Commands::SearchTrigrams(SearchTrigramsArgs { trigrams, limit }) => {
            crate::commands::search::execute_search_trigrams(trigrams, limit)
        }
        Commands::Audit(AuditArgs {
            entity,
            pos_entity,
            include_unaudited,
            limit,
            offset,
            json,
        }) => crate::commands::ledger_audit::execute_ledger_audit(
            entity.or(pos_entity),
            include_unaudited,
            limit,
            offset,
            json,
        ),
        #[cfg(feature = "daemon")]
        Commands::Daemon { interval } => crate::commands::daemon::execute_daemon(interval),
        #[cfg(feature = "viz-server")]
        Commands::VizServer {
            port,
            bind,
            open,
            stop,
        } => crate::commands::viz_server::execute_viz_server(port, bind, open, stop),
        #[cfg(feature = "web")]
        Commands::Web { command } => crate::commands::web::execute_web(command),
        Commands::Internal { command } => dispatch_internal(command),
        #[cfg(feature = "usage-metrics")]
        Commands::Usage { command } => dispatch_usage(command),
        #[cfg(feature = "mcp")]
        Commands::Mcp { command } => dispatch_mcp(command),
        Commands::Schedule { subcommand } => dispatch_schedule(subcommand),
        Commands::Demo(DemoArgs {
            keep,
            output,
            force,
        }) => crate::commands::demo::execute_demo(keep, output, force),
        Commands::Timings(TimingsCliArgs {
            global,
            json,
            top,
            days,
            export,
            inner,
            command,
            flame,
            explain,
            prune,
            older_than,
            opt_in,
            opt_out,
        }) => {
            // Opt-in/out write user config only — allow with or without --global.
            if opt_in || opt_out {
                crate::commands::timings::execute_timings(crate::commands::timings::TimingsArgs {
                    global: false,
                    json: false,
                    top: None,
                    days: None,
                    export: None,
                    inner: false,
                    command: None,
                    flame: false,
                    explain: None,
                    prune: false,
                    older_than: None,
                    opt_in,
                    opt_out,
                })
            } else if global {
                let user_config = load_user_config()?;
                crate::state::rollup::execute_timings_global(
                    &user_config.global_rollup,
                    crate::state::rollup::GlobalTimingsArgs {
                        json,
                        top,
                        days,
                        export,
                        inner,
                        command,
                        flame,
                        explain,
                        prune,
                        older_than,
                    },
                )
            } else {
                crate::commands::timings::execute_timings(crate::commands::timings::TimingsArgs {
                    global: false,
                    json,
                    top,
                    days,
                    export,
                    inner,
                    command,
                    flame,
                    explain,
                    prune,
                    older_than,
                    opt_in: false,
                    opt_out: false,
                })
            }
        }
    };

    // Usage metrics counter hook: increment counter and try flush
    // This must never affect the host command's result.
    #[cfg(feature = "usage-metrics")]
    {
        let hook = std::panic::AssertUnwindSafe(|| {
            crate::commands::usage::increment_counter(command_name);
            crate::commands::usage::try_flush();
        });
        if let Err(e) = std::panic::catch_unwind(hook) {
            // Best-effort: debug-level is correct for a panic in a
            // never-fail hook. If a panic happens, the bug is in
            // M7's code; downstream observability (not M7's concern)
            // should be the place to count panics.
            tracing::debug!("Usage metrics hook panicked: {:?}", e);
        }
    }

    // Finish self-timing after the host command completes (exit code 0/1).
    #[cfg(feature = "self-timing")]
    {
        let code = if result.is_ok() { 0 } else { 1 };
        timed.finish(code);
    }

    result
}

/// Return true for commands that operate across discovered repos and must not
/// write to the current repo before dispatch.
fn is_global_command(cmd: &Commands) -> bool {
    matches!(
        cmd,
        Commands::Ledger {
            command: LedgerCommands::Status { global: true, .. },
        } | Commands::Timings(TimingsCliArgs { global: true, .. })
    )
}

/// Pure warn-message for config parse fallback (DoD-7). Unit-tested so the
/// warn! site stays honest without pulling a tracing-subscriber harness.
fn config_load_fallback_warn_message(
    path: impl std::fmt::Display,
    err: impl std::fmt::Display,
) -> String {
    format!("Failed to parse config at {path}: {err}. Using defaults.")
}

/// Pure warn-message for global/user config parse fallback (mirrors startup).
fn global_config_load_fallback_warn_message(
    path: impl std::fmt::Display,
    err: impl std::fmt::Display,
) -> String {
    format!("Failed to parse global rollup config at {path}: {err}. Using defaults.")
}

pub(super) fn load_startup_config(
    layout: &crate::state::layout::Layout,
) -> Result<crate::config::model::Config> {
    // 0094: migrate + gitignore side-effect live at this seam (not in `state`)
    // so the state crate does not depend on git. On rename we ensure
    // `.ledgerful/` is ignored — finishing an operation the tool started
    // (spec §3.1 / §4 single exception). The stale legacy gitignore line is
    // left in place and only reported by doctor.
    let renamed = layout.migrate_legacy_state_dir()?;
    if renamed {
        match crate::git::ignore::add_to_gitignore(&layout.root, ".ledgerful/") {
            Ok(true) => {
                tracing::info!("Added .ledgerful/ to .gitignore after migrating state directory")
            }
            Ok(false) => {}
            Err(e) => {
                tracing::warn!("Failed to add .ledgerful/ to .gitignore after state migration: {e}")
            }
        }
    }
    // DoD-7: warn-then-default on parse failure (same idiom as load_user_config).
    // Do NOT introduce deny_unknown_fields — old configs must keep loading;
    // unknown keys are reported via doctor / serde_ignored (DoD-8).
    match crate::config::load::load_config(layout) {
        Ok(cfg) => Ok(cfg),
        Err(e) => {
            tracing::warn!(
                "{}",
                config_load_fallback_warn_message(layout.config_file(), &e)
            );
            Ok(Default::default())
        }
    }
}

pub(super) fn load_user_config() -> Result<crate::config::model::Config> {
    // `user_config_dir()` returns the Ledgerful state directory (e.g. `~/.ledgerful`
    // or the value of `LEDGERFUL_CONFIG_HOME`). `Layout::new` expects the repo-root
    // equivalent — i.e. the parent of the state directory — so that it can append
    // `.ledgerful/config.toml` itself. This assumes `LEDGERFUL_CONFIG_HOME` points
    // to a directory whose parent acts as the user-level repo root.
    let config_dir = crate::state::rollup::user_config_dir()?;
    let Some(parent) = config_dir.parent() else {
        return Err(miette::miette!(
            "user config directory '{}' has no parent",
            config_dir.display()
        ));
    };
    let layout = crate::state::layout::Layout::new(
        camino::Utf8Path::from_path(parent)
            .ok_or_else(|| miette::miette!("user config directory parent is not valid UTF-8"))?,
    );
    match crate::config::load::load_config(&layout) {
        Ok(cfg) => Ok(cfg),
        Err(e) => {
            tracing::warn!(
                "{}",
                global_config_load_fallback_warn_message(layout.config_file(), &e)
            );
            Ok(Default::default())
        }
    }
}

fn handle_schema_error(err: miette::Error) -> Result<()> {
    let is_schema_mismatch = if let Some(state_err) = err.downcast_ref::<crate::state::StateError>()
    {
        matches!(state_err, crate::state::StateError::SchemaMismatch)
    } else {
        false
    };

    if is_schema_mismatch && crate::util::term::is_interactive() {
        use inquire::Confirm;
        if let Ok(true) = Confirm::new("Schema mismatch detected. Run 'update --migrate' now?")
            .with_default(true)
            .prompt()
        {
            crate::commands::update::execute_update(true, false, true, false, false, false, false)?;
            return Ok(());
        }
    }
    Err(err)
}

#[cfg(test)]
mod rename_tests {
    use super::*;
    use camino::Utf8Path;
    use std::sync::{Mutex, OnceLock};

    /// Serialize cwd-mutating tests so parallel nextest workers do not race.
    fn cwd_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    #[test]
    fn startup_config_migrates_legacy_state_before_loading() {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8Path::from_path(tmp.path()).unwrap();
        let legacy = root.join(concat!(".change", "guard"));
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::write(
            legacy.join("config.toml"),
            "[verify]\ndefault_timeout_secs = 917\n",
        )
        .unwrap();
        let layout = crate::state::layout::Layout::new(root);

        let config = load_startup_config(&layout).unwrap();

        assert_eq!(config.verify.default_timeout_secs, 917);
        assert!(!legacy.exists());
        assert!(layout.config_file().exists());
    }

    /// DoD-1: Design-shaped fixture — legacy dir + gitignore only legacy path.
    /// After migrate, `.ledgerful/` is present in `.gitignore`.
    #[test]
    fn startup_migration_ensures_ledgerful_gitignore() {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8Path::from_path(tmp.path()).unwrap();
        let legacy = root.join(concat!(".change", "guard"));
        std::fs::create_dir_all(legacy.join("state")).unwrap();
        std::fs::write(legacy.join("state").join("marker"), "x").unwrap();
        // Only the legacy path is ignored (Design shape).
        std::fs::write(
            root.join(".gitignore"),
            format!("target/\n.{}/\n", concat!("change", "guard")),
        )
        .unwrap();

        let layout = crate::state::layout::Layout::new(root);
        let _ = load_startup_config(&layout).unwrap();

        assert!(!legacy.exists());
        assert!(layout.state_dir.exists());
        let gi = std::fs::read_to_string(root.join(".gitignore")).unwrap();
        assert!(
            gi.lines()
                .any(|l| crate::git::ignore::gitignore_patterns_equivalent(l, ".ledgerful/")),
            ".gitignore must contain a .ledgerful/ equivalent after migration; got:\n{gi}"
        );
    }

    /// DoD-1 / R3: repo already ignoring `.ledgerful/` gets no .gitignore write.
    #[test]
    fn startup_migration_no_gitignore_write_when_already_present() {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8Path::from_path(tmp.path()).unwrap();
        let legacy = root.join(concat!(".change", "guard"));
        std::fs::create_dir_all(&legacy).unwrap();
        let original_gi = "target/\n.ledgerful/\n";
        std::fs::write(root.join(".gitignore"), original_gi).unwrap();

        let layout = crate::state::layout::Layout::new(root);
        let _ = load_startup_config(&layout).unwrap();

        let after = std::fs::read_to_string(root.join(".gitignore")).unwrap();
        assert_eq!(after, original_gi);
    }

    /// DoD-7: malformed config produces a warning (via tracing) and defaults.
    #[test]
    fn startup_config_malformed_uses_defaults_without_panic() {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8Path::from_path(tmp.path()).unwrap();
        let layout = crate::state::layout::Layout::new(root);
        layout.ensure_state_dir().unwrap();
        std::fs::write(layout.config_file(), "this is not = valid toml {{").unwrap();

        let config = load_startup_config(&layout).unwrap();
        // Defaults: strict is false.
        assert!(!config.core.strict);
    }

    /// DoD-7: fallback warn message names path and "Using defaults".
    #[test]
    fn config_load_fallback_warn_message_includes_path_and_defaults() {
        let msg = config_load_fallback_warn_message(
            "/tmp/repo/.ledgerful/config.toml",
            "TOML parse error at line 1",
        );
        assert!(
            msg.contains("/tmp/repo/.ledgerful/config.toml"),
            "must include path: {msg}"
        );
        assert!(
            msg.contains("Using defaults"),
            "must say Using defaults: {msg}"
        );
        assert!(
            msg.contains("TOML parse error"),
            "must include error fragment: {msg}"
        );
        assert!(
            msg.contains("Failed to parse config"),
            "must identify parse failure: {msg}"
        );
    }

    /// DoD-10: global command path must not rename the legacy state directory.
    /// Invokes the real `run_with` dispatch path (not the rollup builder alone).
    #[test]
    fn global_command_does_not_rename_legacy_state_dir() {
        let _guard = cwd_lock().lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8Path::from_path(tmp.path()).unwrap();
        let legacy = root.join(concat!(".change", "guard"));
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::write(legacy.join("marker"), "preserve").unwrap();

        let original = std::env::current_dir().unwrap();
        std::env::set_current_dir(tmp.path()).unwrap();

        let cli = Cli {
            verbose: false,
            quiet: false,
            command: Commands::Timings(TimingsCliArgs {
                global: true,
                json: true,
                top: None,
                days: None,
                export: None,
                inner: false,
                command: None,
                flame: false,
                explain: None,
                prune: false,
                older_than: None,
                opt_in: false,
                opt_out: false,
            }),
        };
        // May fail for unrelated reasons (no timing data / user config); rename
        // must still not have occurred.
        let _ = run_with(cli);

        std::env::set_current_dir(original).unwrap();

        assert!(
            legacy.exists(),
            "global command must not rename legacy state directory"
        );
        assert!(
            !root.join(".ledgerful").exists()
                || std::fs::read_to_string(legacy.join("marker")).unwrap() == "preserve",
            "legacy marker must remain under the legacy path"
        );
        assert_eq!(
            std::fs::read_to_string(legacy.join("marker")).unwrap(),
            "preserve"
        );
    }
}
