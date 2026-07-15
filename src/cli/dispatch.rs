use crate::cli::args::{
    Cli, Commands, ConfigCommands, ExportCommands, FederateCommands, GateCommands, IntentCommands,
    InternalCommands, LedgerCommands, RegisterCommands, ServiceSubcommands,
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
        Commands::Init { force, enforce } => crate::commands::init::execute_init(force, enforce),
        Commands::Gate { command } => dispatch_gate(command),
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
        Commands::Export { command } => dispatch_export(command),
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
        #[cfg(any(feature = "openapi", feature = "web"))]
        Commands::Openapi => {
            let json = crate::commands::web::api::generate_openapi_json();
            println!("{}", json);
            return Ok(());
        }
        Commands::Verify {
            command,
            tx_id,
            timeout,
            no_predict,
            explain,
            entity,
            health,
            signatures,
            chain,
            against_export,
            dry_run,
            scope,
        } => dispatch_verify(
            &layout,
            command,
            tx_id,
            timeout,
            no_predict,
            explain,
            entity,
            health,
            signatures,
            chain,
            against_export,
            dry_run,
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
        Commands::Demo {
            keep,
            output,
            force,
        } => crate::commands::demo::execute_demo(keep, output, force),
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

/// Check if the current repo has a DEMO_MARKER file, indicating it was
/// created by `ledgerful demo`. If so, exports must self-identify as demo
/// artifacts to prevent synthetic evidence from being mistaken for real.
fn is_demo_repo(layout: &crate::state::layout::Layout) -> bool {
    layout.root.join(".ledgerful").join("DEMO_MARKER").exists()
}

#[cfg(feature = "export")]
fn dispatch_export(command: ExportCommands) -> Result<()> {
    use crate::export::soc2::generate_soc2_export_with_options;
    use crate::state::layout::Layout;
    use owo_colors::OwoColorize;

    match command {
        ExportCommands::Evidence {
            profile,
            out,
            force,
        } => {
            if profile != "soc2" {
                return Err(miette::miette!(
                    "unknown export profile: {profile}; currently only 'soc2' is supported"
                ));
            }

            let root = crate::commands::helpers::get_repo_root()
                .map(|r| r.as_std_path().to_path_buf())
                .unwrap_or_else(|_| {
                    std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
                });
            let root = camino::Utf8PathBuf::from_path_buf(root)
                .map_err(|_| miette::miette!("export root path is not valid UTF-8"))?;
            let layout = Layout::new(root);

            let is_demo = is_demo_repo(&layout);
            let keys_dir = if is_demo {
                Some(
                    layout
                        .root
                        .join(".ledgerful")
                        .join("keys")
                        .as_std_path()
                        .to_path_buf(),
                )
            } else {
                None
            };
            let default_name = if is_demo {
                "ledgerful-DEMO-evidence.zip"
            } else {
                "ledgerful-soc2-evidence.zip"
            };
            let path = out.unwrap_or_else(|| std::path::PathBuf::from(default_name));

            let validated = validate_export_evidence_path(&path, force)?;

            let zip_bytes =
                generate_soc2_export_with_options(&layout, is_demo, keys_dir.as_deref())?;

            std::fs::write(&validated, &zip_bytes).into_diagnostic()?;

            println!(
                "{} SOC2 evidence exported to {}",
                "SUCCESS:".green().bold(),
                validated.display()
            );
            Ok(())
        }
    }
}

#[cfg(not(feature = "export"))]
fn dispatch_export(_command: ExportCommands) -> Result<()> {
    Err(miette::miette!(
        "export feature is not enabled in this build; rebuild with --features export"
    ))
}

/// Validate an export-evidence output path with 0032 path-safety discipline.
///
/// Differences from `validate_export_path` (used for `ledger export-provenance`):
///
/// - The path does **not** have to be inside the repository. Users may export
///   evidence to an absolute path such as `~/Desktop/ledgerful-soc2-evidence.zip`.
/// - If the current directory is inside a git repository, we still refuse to write
///   to `Cargo.toml`, `src/`, or `.ledgerful/state/` inside that repository, and
///   we re-check the canonicalized path after symlink resolution so a repo-local
///   symlink cannot escape into a protected location.
/// - Refuses to overwrite an existing file without `--force`.
fn validate_export_evidence_path(
    path: &std::path::Path,
    force: bool,
) -> miette::Result<std::path::PathBuf> {
    let cwd = std::env::current_dir()
        .map_err(|e| miette::miette!("failed to determine current directory: {e}"))?;
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    };
    let cleaned = path_clean::PathClean::clean(&absolute);

    // Reject directory-only targets and paths with no file component.
    let file_name_os = cleaned.file_name();
    let file_name_valid = file_name_os
        .and_then(|n| n.to_str().map(|s| !s.is_empty()))
        .unwrap_or(false);
    if cleaned.is_dir() || !file_name_valid {
        return Err(miette::miette!(
            "invalid path: no file component; must specify a file, not a directory"
        ));
    }

    // Resolve symlinks/junctions for the safety-boundary comparison.
    let canonical = if cleaned.exists() {
        std::fs::canonicalize(&cleaned)
            .map_err(|e| miette::miette!("failed to resolve path: {e}"))?
    } else {
        match cleaned.parent() {
            Some(parent) => {
                let base = std::fs::canonicalize(parent)
                    .map_err(|e| miette::miette!("failed to resolve parent directory: {e}"))?;
                base.join(file_name_os.unwrap_or_default())
            }
            None => cleaned.clone(),
        }
    };

    let canonical = strip_verbatim_prefix(&canonical);

    // If we can discover a repo root, apply source/state protections to the
    // canonicalized path. This is the post-canonicalization symlink re-check:
    // a path that pointed at `src/` or `.ledgerful/state/` via a symlink/junction
    // will resolve to a canonical path under those directories and be rejected.
    if let Ok(repo_root) = crate::commands::helpers::get_repo_root() {
        let repo_root_std = repo_root.as_std_path();
        let canonical_repo_root = std::fs::canonicalize(repo_root_std)
            .map_err(|e| miette::miette!("failed to resolve repo root: {e}"))?;
        let canonical_repo_root = strip_verbatim_prefix(&canonical_repo_root);

        let cargo_toml_path = strip_verbatim_prefix(&canonical_repo_root.join("Cargo.toml"));
        if canonical == cargo_toml_path {
            return Err(miette::miette!("refusing to write to Cargo.toml"));
        }

        let src_dir = strip_verbatim_prefix(&canonical_repo_root.join("src"));
        if canonical.starts_with(&src_dir) {
            return Err(miette::miette!("refusing to write inside src/"));
        }

        let state_dir =
            strip_verbatim_prefix(&canonical_repo_root.join(".ledgerful").join("state"));
        if canonical.starts_with(&state_dir) {
            return Err(miette::miette!(
                "refusing to write inside .ledgerful/state/"
            ));
        }
    }

    if canonical.exists() && !force {
        return Err(miette::miette!(
            "{} already exists; use --force to overwrite",
            canonical.display()
        ));
    }

    Ok(canonical)
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
        LedgerCommands::Resume { tx_id } => crate::commands::ledger::execute_ledger_resume(tx_id),
        LedgerCommands::ExportProvenance { out_path, force } => {
            dispatch_ledger_export_provenance(out_path, force)
        }
        LedgerCommands::ExportPublic { output, sign, key } => {
            dispatch_ledger_export_public(output, sign, key)
        }
        LedgerCommands::HookRepair { force } => {
            crate::commands::ledger::execute_ledger_hook_repair(force)
        }
    }
}

fn dispatch_ledger_export_provenance(
    out_path: Option<std::path::PathBuf>,
    force: bool,
) -> Result<()> {
    use camino::Utf8PathBuf;

    let Some(path) = out_path else {
        return crate::commands::ledger::execute_ledger_export_provenance(None);
    };

    let clean = validate_export_path(&path, force)?;
    let utf8 = Utf8PathBuf::from_path_buf(clean)
        .map_err(|_| miette::miette!("export path is not valid UTF-8"))?;
    crate::commands::ledger::execute_ledger_export_provenance(Some(utf8.to_string()))
}

fn dispatch_ledger_export_public(
    output: std::path::PathBuf,
    sign: bool,
    key: Option<std::path::PathBuf>,
) -> Result<()> {
    let clean = validate_export_public_path(&output)?;
    let options = crate::ledger::ExportOptions {
        output: &clean,
        sign,
        key: key.as_deref(),
    };
    crate::commands::ledger::execute_ledger_export_public(options)
}

fn validate_export_public_path(path: &std::path::Path) -> miette::Result<std::path::PathBuf> {
    let repo_root = crate::commands::helpers::get_repo_root()
        .map_err(|e| miette::miette!("failed to determine repository root: {e}"))?;

    let absolute = std::env::current_dir()
        .map(|cwd| cwd.join(path))
        .map_err(|e| miette::miette!("failed to determine current directory: {e}"))?;
    let cleaned = path_clean::PathClean::clean(&absolute);

    // For directory output, the target itself must have a directory name component.
    let file_name_os = cleaned.file_name();
    let file_name_valid = file_name_os
        .and_then(|n| n.to_str().map(|s| !s.is_empty()))
        .unwrap_or(false);
    if !file_name_valid {
        return Err(miette::miette!(
            "invalid output directory: no directory name component"
        ));
    }

    let canonical = if cleaned.exists() {
        let meta = std::fs::metadata(&cleaned)
            .map_err(|e| miette::miette!("failed to inspect path: {e}"))?;
        if !meta.is_dir() {
            return Err(miette::miette!(
                "output path exists and is not a directory: {}",
                cleaned.display()
            ));
        }
        std::fs::canonicalize(&cleaned)
            .map_err(|e| miette::miette!("failed to resolve path: {e}"))?
    } else {
        match cleaned.parent() {
            Some(parent) => {
                let base = std::fs::canonicalize(parent)
                    .map_err(|e| miette::miette!("failed to resolve parent directory: {e}"))?;
                base.join(file_name_os.unwrap_or_default())
            }
            None => cleaned.clone(),
        }
    };

    let canonical = strip_verbatim_prefix(&canonical);

    let canonical_repo_root = std::fs::canonicalize(repo_root.as_std_path())
        .map_err(|e| miette::miette!("failed to resolve repo root: {e}"))?;
    let canonical_repo_root = strip_verbatim_prefix(&canonical_repo_root);

    let state_dir = strip_verbatim_prefix(&canonical_repo_root.join(".ledgerful").join("state"));
    if canonical.starts_with(&state_dir) {
        return Err(miette::miette!(
            "refusing to write public ledger bundle inside .ledgerful/state/"
        ));
    }

    let src_dir = strip_verbatim_prefix(&canonical_repo_root.join("src"));
    if canonical.starts_with(&src_dir) {
        return Err(miette::miette!(
            "refusing to write public ledger bundle inside src/"
        ));
    }

    if !canonical.exists() {
        std::fs::create_dir_all(&canonical)
            .map_err(|e| miette::miette!("failed to create output directory: {e}"))?;
    }

    Ok(canonical)
}

fn validate_export_path(path: &std::path::Path, force: bool) -> miette::Result<std::path::PathBuf> {
    let repo_root = crate::commands::helpers::get_repo_root()
        .map_err(|e| miette::miette!("failed to determine repository root: {e}"))?;

    let absolute = std::env::current_dir()
        .map(|cwd| cwd.join(path))
        .map_err(|e| miette::miette!("failed to determine current directory: {e}"))?;
    let cleaned = path_clean::PathClean::clean(&absolute);

    // Reject paths that escape the repository root (e.g. "../foo.json").
    let repo_root_std = repo_root.as_std_path();
    if cleaned != repo_root_std && !cleaned.starts_with(repo_root_std) {
        return Err(miette::miette!(
            "export path must be inside the repository ({})",
            repo_root_std.display()
        ));
    }

    // Reject directory-only targets (e.g. "src/..") and paths with no file component.
    let file_name_os = cleaned.file_name();
    let file_name_valid = file_name_os
        .and_then(|n| n.to_str().map(|s| !s.is_empty()))
        .unwrap_or(false);
    if cleaned.is_dir() || !file_name_valid {
        return Err(miette::miette!(
            "invalid path: no file component; must specify a file, not a directory"
        ));
    }

    // Resolve symlinks/junctions for the safety-boundary comparison.
    let canonical = if cleaned.exists() {
        std::fs::canonicalize(&cleaned)
            .map_err(|e| miette::miette!("failed to resolve path: {e}"))?
    } else {
        match cleaned.parent() {
            Some(parent) => {
                let base = std::fs::canonicalize(parent)
                    .map_err(|e| miette::miette!("failed to resolve parent directory: {e}"))?;
                base.join(file_name_os.unwrap_or_default())
            }
            None => cleaned.clone(),
        }
    };

    let canonical = strip_verbatim_prefix(&canonical);

    // Re-check the repository boundary after canonicalization so a repo-local
    // symlink/junction cannot resolve outside the repository.
    let canonical_repo_root = std::fs::canonicalize(repo_root_std)
        .map_err(|e| miette::miette!("failed to resolve repo root: {e}"))?;
    let canonical_repo_root = strip_verbatim_prefix(&canonical_repo_root);
    if canonical != canonical_repo_root && !canonical.starts_with(&canonical_repo_root) {
        return Err(miette::miette!(
            "export path resolves outside the repository after symlink resolution"
        ));
    }

    let cargo_toml_path = strip_verbatim_prefix(&canonical_repo_root.join("Cargo.toml"));
    if canonical == cargo_toml_path {
        return Err(miette::miette!("refusing to write to Cargo.toml"));
    }

    let src_dir = strip_verbatim_prefix(&canonical_repo_root.join("src"));
    if canonical.starts_with(&src_dir) {
        return Err(miette::miette!("refusing to write inside src/"));
    }

    let state_dir = strip_verbatim_prefix(&canonical_repo_root.join(".ledgerful").join("state"));
    if canonical.starts_with(&state_dir) {
        return Err(miette::miette!(
            "refusing to write inside .ledgerful/state/"
        ));
    }

    if canonical.exists() && !force {
        return Err(miette::miette!(
            "{} already exists; use --force to overwrite",
            canonical.display()
        ));
    }

    Ok(canonical)
}

/// Strip the Windows "\\?\" verbatim prefix so that canonical paths compare
/// naturally with user-provided paths. On non-Windows platforms this is a no-op.
fn strip_verbatim_prefix(path: &std::path::Path) -> std::path::PathBuf {
    #[cfg(windows)]
    {
        use std::path::Component;
        let mut components = path.components();
        if let Some(Component::Prefix(prefix)) = components.next()
            && let Some(disk) = prefix
                .as_os_str()
                .to_str()
                .and_then(|s| s.strip_prefix(r"\\?\"))
        {
            let rest = components.as_path();
            return std::path::Path::new(disk).join(rest);
        }
    }
    path.to_path_buf()
}

#[cfg(test)]
mod export_path_tests {
    use super::*;
    use camino::Utf8Path;
    use tempfile::tempdir;

    struct CwdGuard {
        original: std::path::PathBuf,
    }

    impl CwdGuard {
        fn enter(path: &std::path::Path) -> Self {
            let original = std::env::current_dir().unwrap();
            std::env::set_current_dir(path).unwrap();
            CwdGuard { original }
        }
    }

    impl Drop for CwdGuard {
        fn drop(&mut self) {
            let _ = std::env::set_current_dir(&self.original);
        }
    }

    fn temp_repo() -> (tempfile::TempDir, camino::Utf8PathBuf) {
        let tmp = tempdir().unwrap();
        let root = Utf8Path::from_path(tmp.path()).unwrap().to_path_buf();
        std::process::Command::new("git")
            .args(["init"])
            .current_dir(&root)
            .output()
            .expect("git init failed");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join(".ledgerful").join("state")).unwrap();
        std::fs::File::create(root.join("Cargo.toml")).unwrap();
        (tmp, root)
    }

    fn temp_repo_root_only() -> (tempfile::TempDir, camino::Utf8PathBuf) {
        let tmp = tempdir().unwrap();
        let root = Utf8Path::from_path(tmp.path()).unwrap().to_path_buf();
        std::process::Command::new("git")
            .args(["init"])
            .current_dir(&root)
            .output()
            .expect("git init failed");
        // No src/ and no .ledgerful/state/ so outside-repo paths have something
        // to compare against but do not accidentally hit forbidden dirs.
        (tmp, root)
    }

    #[serial_test::serial(cwd)]
    #[test]
    fn validate_export_path_accepts_valid_relative_file() {
        let (_tmp, root) = temp_repo();
        let _guard = CwdGuard::enter(root.as_std_path());

        let path = validate_export_path(std::path::Path::new("out.json"), false).unwrap();
        // Canonicalize both sides — on Windows CI the temp dir may resolve
        // to an 8.3 short name (e.g. RUNNER~1) via canonicalize, while the
        // original path uses the long name (runneradmin).
        let expected = std::fs::canonicalize(root.as_std_path().join("out.json"))
            .unwrap_or_else(|_| root.as_std_path().join("out.json"));
        let canonical_path = std::fs::canonicalize(&path).unwrap_or(path);
        assert_eq!(canonical_path, expected);
    }

    #[serial_test::serial(cwd)]
    #[test]
    fn validate_export_path_accepts_existing_file_with_force() {
        let (_tmp, root) = temp_repo();
        let _guard = CwdGuard::enter(root.as_std_path());
        std::fs::File::create(root.join("existing.json")).unwrap();

        let path = validate_export_path(std::path::Path::new("existing.json"), true).unwrap();
        // Same 8.3 short-name workaround as above.
        let expected = std::fs::canonicalize(root.as_std_path().join("existing.json"))
            .unwrap_or_else(|_| root.as_std_path().join("existing.json"));
        let canonical_path = std::fs::canonicalize(&path).unwrap_or(path);
        assert_eq!(canonical_path, expected);
    }

    #[serial_test::serial(cwd)]
    #[test]
    fn validate_export_path_refuses_existing_file_without_force() {
        let (_tmp, root) = temp_repo();
        let _guard = CwdGuard::enter(root.as_std_path());
        std::fs::File::create(root.join("existing.json")).unwrap();

        let err = validate_export_path(std::path::Path::new("existing.json"), false)
            .unwrap_err()
            .to_string();

        assert!(err.contains("already exists"));
    }

    #[serial_test::serial(cwd)]
    #[test]
    fn validate_export_path_refuses_src_directory() {
        let (_tmp, root) = temp_repo();
        let _guard = CwdGuard::enter(root.as_std_path());

        let err = validate_export_path(std::path::Path::new("src/foo.json"), false)
            .unwrap_err()
            .to_string();

        assert!(err.contains("inside src/"));
    }

    #[serial_test::serial(cwd)]
    #[test]
    fn validate_export_path_refuses_cargo_toml() {
        let (_tmp, root) = temp_repo();
        let _guard = CwdGuard::enter(root.as_std_path());

        let err = validate_export_path(std::path::Path::new("Cargo.toml"), false)
            .unwrap_err()
            .to_string();

        assert!(err.contains("Cargo.toml"));
    }

    #[serial_test::serial(cwd)]
    #[test]
    fn validate_export_path_refuses_state_directory() {
        let (_tmp, root) = temp_repo();
        let _guard = CwdGuard::enter(root.as_std_path());

        let err = validate_export_path(std::path::Path::new(".ledgerful/state/foo.json"), false)
            .unwrap_err()
            .to_string();

        assert!(err.contains("inside .ledgerful/state/"));
    }

    #[serial_test::serial(cwd)]
    #[test]
    fn validate_export_path_refuses_dotdot_traversal() {
        let (_tmp, root) = temp_repo();
        let _guard = CwdGuard::enter(root.as_std_path());

        let err = validate_export_path(std::path::Path::new("../src/foo.json"), false)
            .unwrap_err()
            .to_string();

        assert!(err.contains("inside the repository") || err.contains("inside src/"));
    }

    #[cfg(windows)]
    #[serial_test::serial(cwd)]
    #[test]
    fn validate_export_path_refuses_symlink_resolving_outside_repo() {
        let (_tmp, root) = temp_repo();
        let _guard = CwdGuard::enter(root.as_std_path());

        // Create a directory outside the repo and a symlink/junction inside the
        // repo that points to it. Windows requires elevated privileges for
        // directory symlinks, so we fall back to a junction when symlinking fails.
        let outside = root.as_std_path().join("../outside");
        std::fs::create_dir_all(&outside).unwrap();
        let outside_abs = std::fs::canonicalize(&outside).unwrap();
        let link_path = root.as_std_path().join("link_to_outside");

        let symlink_ok = std::os::windows::fs::symlink_dir(&outside_abs, &link_path);
        if symlink_ok.is_err() {
            let _ = std::fs::remove_dir_all(&link_path);
            // Junction fallback requires Windows-specific APIs; skip if unavailable.
            if std::process::Command::new("cmd")
                .args([
                    "/c",
                    "mklink",
                    "/J",
                    link_path.to_str().unwrap_or_default(),
                    outside_abs.to_str().unwrap_or_default(),
                ])
                .output()
                .map(|out| !out.status.success())
                .unwrap_or(true)
            {
                // Symlinks/junctions unavailable in this environment; skip.
                return;
            }
        }

        let err = validate_export_path(std::path::Path::new("link_to_outside/escaped.json"), false)
            .unwrap_err()
            .to_string();

        assert!(
            err.contains("resolves outside the repository") || err.contains("inside src/"),
            "unexpected error: {err}"
        );
    }

    #[cfg(not(windows))]
    #[serial_test::serial(cwd)]
    #[test]
    fn validate_export_path_refuses_symlink_resolving_outside_repo() {
        let (_tmp, root) = temp_repo();
        let _guard = CwdGuard::enter(root.as_std_path());

        let outside = root.as_std_path().join("../outside");
        std::fs::create_dir_all(&outside).unwrap();
        let outside_abs = std::fs::canonicalize(&outside).unwrap();
        let link_path = root.as_std_path().join("link_to_outside");

        if std::os::unix::fs::symlink(&outside_abs, &link_path).is_err() {
            return;
        }

        let err = validate_export_path(std::path::Path::new("link_to_outside/escaped.json"), false)
            .unwrap_err()
            .to_string();

        assert!(
            err.contains("resolves outside the repository") || err.contains("inside src/"),
            "unexpected error: {err}"
        );
    }

    #[serial_test::serial(cwd)]
    #[test]
    fn validate_export_path_refuses_src_dotdot() {
        let (_tmp, root) = temp_repo();
        let _guard = CwdGuard::enter(root.as_std_path());

        let err = validate_export_path(std::path::Path::new("src/.."), false)
            .unwrap_err()
            .to_string();

        assert!(err.contains("no file component") || err.contains("directory"));
    }

    #[serial_test::serial(cwd)]
    #[test]
    fn validate_export_path_refuses_absolute_path_in_src() {
        let (_tmp, root) = temp_repo();
        let _guard = CwdGuard::enter(root.as_std_path());

        let err = validate_export_path(&root.as_std_path().join("src/foo.json"), false)
            .unwrap_err()
            .to_string();

        assert!(err.contains("inside src/"));
    }

    #[serial_test::serial(cwd)]
    #[test]
    fn validate_export_path_rejects_directory_target() {
        let (_tmp, root) = temp_repo();
        let _guard = CwdGuard::enter(root.as_std_path());
        std::fs::create_dir_all(root.join("mydir")).unwrap();

        let err = validate_export_path(std::path::Path::new("mydir"), false)
            .unwrap_err()
            .to_string();

        assert!(
            err.contains("already exists") || err.contains("directory"),
            "unexpected error: {err}"
        );
    }

    #[serial_test::serial(cwd)]
    #[test]
    fn validate_export_public_path_accepts_outside_repo() {
        let (_tmp, root) = temp_repo();
        let _guard = CwdGuard::enter(root.as_std_path());

        let outside = root.as_std_path().join("..").join("public-output");
        let path = validate_export_public_path(outside.as_path()).unwrap();
        assert!(path.exists(), "output directory should be created");
    }

    #[serial_test::serial(cwd)]
    #[test]
    fn validate_export_public_path_accepts_existing_outside_directory() {
        let (_tmp, root) = temp_repo();
        let _guard = CwdGuard::enter(root.as_std_path());

        let outside = root.as_std_path().join("..").join("existing-output");
        std::fs::create_dir_all(&outside).unwrap();
        let path = validate_export_public_path(outside.as_path()).unwrap();
        assert_eq!(
            path,
            strip_verbatim_prefix(&std::fs::canonicalize(&outside).unwrap())
        );
    }

    #[serial_test::serial(cwd)]
    #[test]
    fn validate_export_public_path_refuses_existing_file() {
        let (_tmp, root) = temp_repo_root_only();
        let _guard = CwdGuard::enter(root.as_std_path());

        let target = root.as_std_path().join("not-a-dir.json");
        std::fs::File::create(&target).unwrap();

        let err = validate_export_public_path(target.as_path())
            .unwrap_err()
            .to_string();

        assert!(err.contains("not a directory"), "unexpected error: {err}");
    }

    #[serial_test::serial(cwd)]
    #[test]
    fn validate_export_public_path_refuses_src_directory() {
        let (_tmp, root) = temp_repo();
        let _guard = CwdGuard::enter(root.as_std_path());

        let err = validate_export_public_path(std::path::Path::new("src/bundle"))
            .unwrap_err()
            .to_string();

        assert!(err.contains("inside src/"), "unexpected error: {err}");
    }

    #[serial_test::serial(cwd)]
    #[test]
    fn validate_export_public_path_refuses_state_directory() {
        let (_tmp, root) = temp_repo();
        let _guard = CwdGuard::enter(root.as_std_path());

        let err = validate_export_public_path(std::path::Path::new(".ledgerful/state/bundle"))
            .unwrap_err()
            .to_string();

        assert!(
            err.contains("inside .ledgerful/state/"),
            "unexpected error: {err}"
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn dispatch_verify(
    layout: &crate::state::layout::Layout,
    command: Option<String>,
    tx_id: Option<String>,
    timeout: u64,
    no_predict: bool,
    explain: bool,
    entity: Option<String>,
    health: bool,
    signatures: bool,
    chain: bool,
    against_export: Option<std::path::PathBuf>,
    dry_run: bool,
    scope: crate::verify::plan::VerifyScope,
) -> Result<()> {
    if signatures || chain || against_export.is_some() {
        crate::commands::verify::verify_ledger_signatures_with_options(
            layout,
            signatures,
            chain,
            against_export.as_deref(),
        )
    } else {
        crate::commands::verify::execute_verify(
            command, tx_id, timeout, no_predict, explain, entity, health, dry_run, scope,
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

fn dispatch_gate(command: GateCommands) -> Result<()> {
    match command {
        GateCommands::Mode { mode } => {
            let layout = crate::commands::helpers::get_layout()?;
            if let Some(mode) = mode {
                let mode = mode.to_lowercase();
                if !crate::config::model::GateConfig::valid_modes().contains(&mode.as_str()) {
                    return Err(miette::miette!(
                        "invalid gate mode '{}'; valid modes are: observe, enforce",
                        mode
                    ));
                }
                let config = crate::config::load::load_config(&layout).unwrap_or_default();
                let old_mode = config.gate.mode.clone();
                if old_mode == mode {
                    println!("Gate mode is already: {}", mode);
                    return Ok(());
                }
                crate::commands::gate::write_mode_transition_entry(&layout, &old_mode, &mode)?;
                crate::commands::config::execute_config_set_in(
                    &layout,
                    &format!("gate.mode={}", mode),
                )?;
                println!("Gate mode changed: {} → {}", old_mode, mode);
            } else {
                let config = crate::config::load::load_config(&layout).unwrap_or_default();
                println!("Gate mode: {}", config.gate.mode);
            }
            Ok(())
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
