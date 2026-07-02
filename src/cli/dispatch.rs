use crate::cli::args::{
    Cli, Commands, ConfigCommands, FederateCommands, IntentCommands, InternalCommands,
    LedgerCommands, RegisterCommands, ServiceSubcommands,
};
use crate::commands::search::SearchArgs;
use miette::{IntoDiagnostic, Result};
use std::env;

pub fn run_with(cli: Cli) -> Result<()> {
    let current_dir = env::current_dir().into_diagnostic()?;
    let layout = crate::state::layout::Layout::new(current_dir.to_string_lossy().as_ref());
    let config = load_startup_config(&layout)?;

    // Capture the command name for usage metrics before moving `cli.command`
    #[cfg(feature = "usage-metrics")]
    let command_name = cli.command.command_name();

    let result = match cli.command {
        Commands::Init { force } => crate::commands::init::execute_init(force),
        Commands::Setup { yes, skip_scan } => crate::commands::setup::execute_setup(yes, skip_scan),
        Commands::Scan {
            impact,
            summary,
            json,
            out,
            base_ref,
        } => crate::commands::scan::execute_scan(impact, summary, json, out, base_ref),
        Commands::Impact {
            all_parents,
            summary,
            telemetry,
            dead_code,
            json,
            out,
        } => crate::commands::impact::execute_impact(
            all_parents,
            summary,
            telemetry,
            dead_code,
            json,
            out,
        ),
        Commands::Index {
            incremental,
            full,
            analyze_graph,
            docs,
            contracts,
            semantic,
            scip,
            auto_scip,
            export_docs,
            doc_type,
            check,
            json,
            strict,
            concurrency,
            semantic_dry_run,
            fast,
            repair_metadata,
            dry_run,
            yes,
        } => dispatch_index(
            incremental,
            full,
            analyze_graph,
            docs,
            contracts,
            semantic,
            scip.clone(),
            auto_scip,
            export_docs,
            doc_type.clone(),
            check,
            json,
            strict,
            concurrency,
            semantic_dry_run.clone(),
            fast,
            repair_metadata,
            dry_run,
            yes,
        )
        .or_else(handle_schema_error),
        Commands::Search {
            query,
            regex,
            semantic,
            limit,
            index,
            json,
            auto_index,
            hybrid,
        } => dispatch_search(
            current_dir,
            query,
            regex,
            semantic,
            limit,
            index,
            json,
            auto_index,
            hybrid,
        )
        .or_else(handle_schema_error),
        Commands::Hotspots { args } => crate::commands::hotspots::execute_hotspots(args),
        Commands::Endpoints(args) => crate::commands::endpoints::execute_endpoints(args),
        Commands::Federate { command } => dispatch_federate(command),
        Commands::Bridge { subcommand } => crate::commands::bridge::execute(subcommand),
        Commands::Services { command } => dispatch_services(command, &config),
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
        Commands::Verify {
            command,
            timeout,
            no_predict,
            explain,
            entity,
            health,
            signatures,
            dry_run,
            scope,
        } => dispatch_verify(
            &layout, command, timeout, no_predict, explain, entity, health, signatures, dry_run,
            scope,
        ),
        Commands::Ask {
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
        } => crate::commands::ask::execute_ask(
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
        )
        .or_else(handle_schema_error),
        Commands::Intent { command } => dispatch_intent(command),
        Commands::Reset {
            remove_config,
            remove_rules,
            include_ledger,
            all,
            yes,
            dry_run,
        } => crate::commands::reset::execute_reset(
            remove_config,
            remove_rules,
            include_ledger,
            all,
            yes,
            dry_run,
        ),
        Commands::Doctor => crate::commands::doctor::execute_doctor(),
        Commands::Status => {
            crate::commands::ledger::execute_ledger_status(None, false, false, false, false, false)
        }
        Commands::Config { command } => dispatch_config(command, cli.verbose),
        Commands::DeadCode {
            threshold,
            limit,
            auto_index,
            include_traits,
            prune,
            expand,
            explain,
        } => crate::commands::dead_code::execute_dead_code(
            threshold,
            limit,
            auto_index,
            include_traits,
            prune,
            expand,
            explain,
        ),
        Commands::Viz {
            output,
            limit,
            depth,
            entity,
            view,
        } => {
            let path = output.map(std::path::PathBuf::from);
            crate::commands::viz::execute_viz(path, limit, depth, entity, view)
        }
        Commands::Update {
            migrate,
            binary,
            force,
            force_unlock,
            fast,
            dry_run,
            repair_hooks,
        } => crate::commands::update::execute_update(
            migrate,
            binary,
            force,
            force_unlock,
            fast,
            dry_run,
            repair_hooks,
        ),
        Commands::Watch {
            interval,
            json,
            no_graph_sync,
        } => crate::commands::watch::execute_watch(interval, json, no_graph_sync),
        #[cfg(feature = "sync")]
        Commands::Sync { subcommand } => crate::commands::sync::handle(subcommand),
        Commands::SearchTrigrams { trigrams, limit } => {
            crate::commands::search::execute_search_trigrams(trigrams, limit)
        }
        Commands::Audit {
            entity,
            pos_entity,
            include_unaudited,
            limit,
            offset,
            json,
        } => crate::commands::ledger_audit::execute_ledger_audit(
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
        Commands::Mcp => crate::commands::mcp::execute_mcp_server(),
        Commands::Schedule { subcommand } => dispatch_schedule(subcommand),
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

    result
}

fn load_startup_config(
    layout: &crate::state::layout::Layout,
) -> Result<crate::config::model::Config> {
    layout.migrate_legacy_state_dir()?;
    Ok(crate::config::load::load_config(layout).unwrap_or_default())
}

// ---------------------------------------------------------------------------
// Command-group dispatch helpers
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn dispatch_index(
    incremental: bool,
    full: bool,
    analyze_graph: bool,
    docs: bool,
    contracts: bool,
    semantic: bool,
    scip: Option<std::path::PathBuf>,
    auto_scip: bool,
    export_docs: bool,
    doc_type: Option<String>,
    check: bool,
    json: bool,
    strict: bool,
    concurrency: Option<usize>,
    semantic_dry_run: Option<Option<std::path::PathBuf>>,
    fast: bool,
    repair_metadata: bool,
    dry_run: bool,
    yes: bool,
) -> Result<()> {
    crate::commands::index::execute_index(crate::commands::index::IndexArgs {
        incremental: incremental && !full,
        check,
        strict,
        json,
        analyze_graph,
        docs,
        contracts,
        semantic,
        scip,
        auto_scip,
        export_docs,
        doc_type,
        concurrency,
        semantic_dry_run,
        fast,
        repair_metadata,
        dry_run,
        yes,
    })
}

#[allow(clippy::too_many_arguments)]
fn dispatch_search(
    current_dir: std::path::PathBuf,
    query: String,
    regex: bool,
    semantic: bool,
    limit: usize,
    index: bool,
    json: bool,
    auto_index: bool,
    hybrid: bool,
) -> Result<()> {
    let project_id = current_dir
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "unknown".to_string());
    crate::commands::search::execute_search(SearchArgs {
        query,
        regex,
        semantic,
        limit,
        index,
        json,
        auto_index,
        project_id,
        hybrid,
    })
}

fn dispatch_federate(command: FederateCommands) -> Result<()> {
    match command {
        FederateCommands::Export { dry_run, out } => {
            crate::commands::federate::execute_federate_export(dry_run, out)
        }
        FederateCommands::Scan => crate::commands::federate::execute_federate_scan(),
        FederateCommands::Status => crate::commands::federate::execute_federate_status(),
    }
}

fn dispatch_services(
    command: ServiceSubcommands,
    config: &crate::config::model::Config,
) -> Result<()> {
    match command {
        ServiceSubcommands::Diff(args) => {
            crate::commands::services_diff::execute_services_diff(args, config)
        }
    }
}

fn dispatch_ledger(command: LedgerCommands) -> Result<()> {
    match command {
        LedgerCommands::Start {
            entity,
            category,
            message,
        } => crate::commands::ledger::execute_ledger_start(entity, &category.to_string(), &message),
        LedgerCommands::Commit {
            tx_id,
            summary,
            reason,
            breaking,
            force,
            with_git,
            git_message,
            no_signoff,
            dry_run,
        } => crate::commands::ledger::execute_ledger_commit(
            tx_id,
            &summary,
            &reason,
            breaking,
            force,
            crate::commands::ledger::LedgerCommitGitOptions {
                with_git,
                git_message,
                signoff: !no_signoff,
                dry_run,
            },
        ),
        LedgerCommands::Rollback { tx_id, reason } => {
            crate::commands::ledger::execute_ledger_rollback(tx_id, reason)
        }
        LedgerCommands::Atomic {
            entity,
            category,
            summary,
            reason,
            force,
        } => crate::commands::ledger::execute_ledger_atomic(
            &entity,
            &category.to_string(),
            &summary,
            &reason,
            force,
        ),
        LedgerCommands::Status {
            all,
            entity,
            compact,
            exit_code,
            verify_signatures,
            json,
        } => crate::commands::ledger::execute_ledger_status(
            entity,
            compact,
            exit_code,
            verify_signatures,
            json,
            all,
        ),
        LedgerCommands::Register { command } => match command {
            RegisterCommands::Rule {
                term,
                category,
                reason,
            } => crate::commands::ledger::execute_ledger_register_rule(
                &term,
                &category.to_string(),
                &reason,
            ),
            RegisterCommands::Validator {
                name,
                command,
                category,
                timeout,
            } => crate::commands::ledger::execute_ledger_register_validator(
                &name, &command, &category, timeout,
            ),
        },
        LedgerCommands::Stack { category } => {
            crate::commands::ledger_stack::execute_ledger_stack(category.map(|c| c.to_string()))
        }
        LedgerCommands::Adr { command } => crate::commands::ledger_adr::execute_ledger_adr(command),
        LedgerCommands::Validator { command } => {
            crate::commands::ledger_register::execute_validator_lifecycle(command)
        }
        LedgerCommands::Graph(args) => crate::commands::ledger_graph::execute_ledger_graph(args),
        LedgerCommands::Search {
            query,
            category,
            days,
            breaking,
            limit,
            offset,
            json,
        } => crate::commands::ledger_search::execute_ledger_search(
            query, category, days, breaking, limit, offset, json,
        ),
        LedgerCommands::Reconcile {
            tx_id,
            pattern,
            all,
            reason,
        } => crate::commands::ledger::execute_ledger_reconcile(tx_id, pattern, all, reason),
        LedgerCommands::Adopt {
            pattern,
            all,
            category,
            summary,
            reason,
        } => crate::commands::ledger::execute_ledger_adopt(
            pattern,
            all,
            &category.to_string(),
            &summary,
            &reason,
        ),
        LedgerCommands::Audit {
            entity,
            pos_entity,
            include_unaudited,
            limit,
            offset,
            json,
        } => crate::commands::ledger_audit::execute_ledger_audit(
            entity.or(pos_entity),
            include_unaudited,
            limit,
            offset,
            json,
        ),
        LedgerCommands::Note {
            entity,
            note,
            message,
        } => crate::commands::ledger::execute_ledger_note(&entity, note, message),
        LedgerCommands::ReSign {
            tx,
            all_invalid,
            dry_run,
            yes,
        } => crate::commands::ledger_re_sign::execute_ledger_re_sign(tx, all_invalid, dry_run, yes),
        LedgerCommands::Gc {
            stale,
            orphans,
            ttl_hours,
            force,
            dry_run,
        } => crate::commands::ledger::execute_ledger_gc(stale, orphans, ttl_hours, force, dry_run),
    }
}

#[allow(clippy::too_many_arguments)]
fn dispatch_verify(
    layout: &crate::state::layout::Layout,
    command: Option<String>,
    timeout: u64,
    no_predict: bool,
    explain: bool,
    entity: Option<String>,
    health: bool,
    signatures: bool,
    dry_run: bool,
    scope: crate::verify::plan::VerifyScope,
) -> Result<()> {
    if signatures {
        crate::commands::verify::verify_ledger_signatures(layout)
    } else {
        crate::commands::verify::execute_verify(
            command, timeout, no_predict, explain, entity, health, dry_run, scope,
        )
    }
}

fn dispatch_intent(command: IntentCommands) -> Result<()> {
    match command {
        IntentCommands::Demo => crate::commands::intent::execute_intent_demo(),
    }
}

fn dispatch_config(command: ConfigCommands, global_verbose: bool) -> Result<()> {
    match command {
        ConfigCommands::Verify {
            json,
            section,
            verbose,
        } => crate::commands::config::execute_config_verify(json, section.as_deref(), verbose),
        ConfigCommands::View { json, section, key } => {
            crate::commands::config::execute_config_view(json, section, key)
        }
        ConfigCommands::Schema { json } => crate::commands::config::execute_config_schema(json),
        // TA19: for `config diff` the global `-v` flag is intercepted to
        // control the internal-env-var filter only; tracing is handled by
        // RUST_LOG and is suppressed for this command in main.rs.
        ConfigCommands::Diff {
            json,
            show_internal,
        } => crate::commands::config::execute_config_diff(json, show_internal || global_verbose),
        ConfigCommands::Set { key_value } => {
            crate::commands::config::execute_config_set(&key_value)
        }
        ConfigCommands::Unset { key } => crate::commands::config::execute_config_unset(&key),
    }
}

fn dispatch_internal(command: InternalCommands) -> Result<()> {
    match command {
        InternalCommands::HookCommitMsg { msg_file } => {
            crate::commands::hook_commit_msg::execute_hook_commit_msg(&msg_file)
        }
        InternalCommands::HookPostCommit => {
            crate::commands::hook_post_commit::execute_hook_post_commit()
        }
    }
}

#[cfg(feature = "usage-metrics")]
fn dispatch_usage(command: crate::cli::args::UsageCommands) -> Result<()> {
    match command {
        crate::cli::args::UsageCommands::Enable => crate::commands::usage::execute_usage_enable(),
        crate::cli::args::UsageCommands::Disable => crate::commands::usage::execute_usage_disable(),
        crate::cli::args::UsageCommands::Status => crate::commands::usage::execute_usage_status(),
        crate::cli::args::UsageCommands::ShowPayload => {
            crate::commands::usage::execute_usage_show_payload()
        }
    }
}

fn dispatch_schedule(subcommand: crate::commands::schedule::ScheduleSubcommands) -> Result<()> {
    match subcommand {
        crate::commands::schedule::ScheduleSubcommands::SetupNightly { dry_run, uninstall } => {
            crate::commands::schedule::execute_setup_nightly(dry_run, uninstall)
        }
        crate::commands::schedule::ScheduleSubcommands::RunNightly => {
            crate::commands::schedule::execute_run_nightly()
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
}
