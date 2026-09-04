use crate::cli::args::*;
use crate::commands::bridge::BridgeCommands;
use crate::commands::data_models::DataModelSubcommands;
use crate::commands::observability::ObservabilitySubcommands;
use crate::commands::security::SecuritySubcommands;

impl Commands {
    /// Return a stable, full subcommand path suitable for usage-metrics
    /// counters. Top-level commands return their own name (e.g. `"scan"`,
    /// `"doctor"`); multi-variant groups return `"<group>_<variant>"`
    /// (e.g. `"ledger_start"`, `"usage_show_payload"`).
    ///
    /// The returned string MUST be a valid identifier suffix — lowercase
    /// ASCII letters, digits, and underscores only — and MUST NOT include
    /// any user-supplied values, paths, or arguments. The dispatch hook
    /// stores this as the primary key in the per-repo `usage_counters`
    /// table.
    pub fn command_name(&self) -> &'static str {
        match self {
            Commands::Init(InitArgs { .. }) => "init",
            // Bare gate → show only (Mode { mode: None }); never invent a set path.
            Commands::Gate { command } => match command {
                None | Some(GateCommands::Mode { mode: None }) => "gate_mode_show",
                Some(GateCommands::Mode { mode: Some(_) }) => "gate_mode_set",
            },
            Commands::Policy { command } => match command {
                None | Some(PolicyCommands::Check { .. }) => "policy_check",
            },
            Commands::Release { command, .. } => match command {
                None | Some(ReleaseCommands::Pins) => "release_pins",
            },

            Commands::Setup(SetupArgs { .. }) => "setup",
            Commands::Scan(ScanArgs { .. }) => "scan",
            Commands::Impact(ImpactArgs { .. }) => "impact",
            Commands::ChangeContext(ChangeContextArgs { .. }) => "change_context",
            Commands::Session(SessionArgs { .. }) => "session",
            Commands::Index(IndexArgs { .. }) => "index",
            Commands::Search(SearchCliArgs { .. }) => "search",
            Commands::Hotspots { args } => match &args.command {
                Some(HotspotSubcommands::Trend { .. }) => "hotspots_trend",
                Some(HotspotSubcommands::Explain { .. }) => "hotspots_explain",
                Some(HotspotSubcommands::Budget { .. }) => "hotspots_budget",
                None => "hotspots",
            },
            Commands::Endpoints(_) => "endpoints",
            Commands::Symbols(_) => "symbols",
            Commands::Surfaces(SurfacesArgs { .. }) => "surfaces",
            Commands::Export { command } => match command {
                ExportCommands::Evidence { .. } => "export_evidence",
                ExportCommands::Head { .. } => "export_head",
            },
            Commands::Federate { command } => match command {
                // Bare federate → status (read-only); never default to export (writes).
                None | Some(FederateCommands::Status) => "federate_status",
                Some(FederateCommands::Export { .. }) => "federate_export",
                Some(FederateCommands::Scan) => "federate_scan",
            },
            Commands::Services { command } => match command {
                None | Some(ServiceSubcommands::Diff(_)) => "services_diff",
            },
            Commands::DataModels(args) => match &args.command {
                DataModelSubcommands::List { .. } => "data_models_list",
                DataModelSubcommands::Impact { .. } => "data_models_impact",
            },
            Commands::Ci(_) => "ci",
            Commands::Deploy(_) => "deploy",
            Commands::Dependencies(_) => "dependencies",
            Commands::Observability(args) => match &args.command {
                ObservabilitySubcommands::Coverage { .. } => "observability_coverage",
                ObservabilitySubcommands::Diff { .. } => "observability_diff",
            },
            Commands::Security(args) => match &args.command {
                SecuritySubcommands::Impact { .. } => "security_impact",
                SecuritySubcommands::Boundaries { .. } => "security_boundaries",
            },
            Commands::Tests(_) => "tests",
            Commands::Bridge { subcommand } => match subcommand {
                BridgeCommands::Export { .. } => "bridge_export",
                BridgeCommands::Import { .. } => "bridge_import",
                BridgeCommands::Query { .. } => "bridge_query",
            },
            Commands::Ledger { command } => match command {
                LedgerCommands::Start { .. } => "ledger_start",
                LedgerCommands::Commit { .. } => "ledger_commit",
                LedgerCommands::Rollback { .. } => "ledger_rollback",
                LedgerCommands::Atomic { .. } => "ledger_atomic",
                LedgerCommands::Status { .. } => "ledger_status",
                LedgerCommands::Register { command } => match command {
                    RegisterCommands::Rule { .. } => "ledger_register_rule",
                    RegisterCommands::Validator { .. } => "ledger_register_validator",
                },
                LedgerCommands::Stack { .. } => "ledger_stack",
                LedgerCommands::Adr { command } => match command {
                    AdrSubcommands::Export { .. } => "ledger_adr_export",
                    AdrSubcommands::UpdateStatus { .. } => "ledger_adr_update_status",
                    AdrSubcommands::Link { .. } => "ledger_adr_link",
                    AdrSubcommands::Review { .. } => "ledger_adr_review",
                    AdrSubcommands::List => "ledger_adr_list",
                },
                LedgerCommands::Validator { command } => match command {
                    ValidatorSubcommands::List { .. } => "ledger_validator_list",
                    ValidatorSubcommands::Enable { .. } => "ledger_validator_enable",
                    ValidatorSubcommands::Disable { .. } => "ledger_validator_disable",
                    ValidatorSubcommands::Remove { .. } => "ledger_validator_remove",
                    ValidatorSubcommands::Doctor => "ledger_validator_doctor",
                },
                LedgerCommands::Graph(_) => "ledger_graph",
                LedgerCommands::Search { .. } => "ledger_search",
                LedgerCommands::Reconcile { .. } => "ledger_reconcile",
                LedgerCommands::Adopt { .. } => "ledger_adopt",
                LedgerCommands::Audit { .. } => "ledger_audit",
                LedgerCommands::Note { .. } => "ledger_note",
                LedgerCommands::ReSign { .. } => "ledger_re_sign",
                LedgerCommands::Gc { .. } => "ledger_gc",
                LedgerCommands::Resume { .. } => "ledger_resume",
                LedgerCommands::ExportProvenance { .. } => "ledger_export_provenance",
                LedgerCommands::ExportPublic { .. } => "ledger_export_public",
                LedgerCommands::HookRepair { .. } => "ledger_hook_repair",
                LedgerCommands::RecoverOrphan { .. } => "ledger_recover_orphan",
            },
            Commands::Verify(VerifyArgs { .. }) => "verify",
            Commands::Ask(AskArgs { .. }) => "ask",
            Commands::Intent { command } => match command {
                IntentCommands::Demo => "intent_demo",
            },
            Commands::Reset(ResetArgs { .. }) => "reset",
            Commands::Doctor(DoctorArgs { .. }) => "doctor",
            Commands::Status(StatusArgs { .. }) => "status",
            Commands::Timings(TimingsCliArgs { .. }) => "timings",
            Commands::Config { command } => match command {
                ConfigCommands::Verify { .. } => "config_verify",
                ConfigCommands::View { .. } => "config_view",
                ConfigCommands::Schema { .. } => "config_schema",
                ConfigCommands::Diff { .. } => "config_diff",
                ConfigCommands::Set { .. } => "config_set",
                ConfigCommands::Unset { .. } => "config_unset",
            },
            Commands::DeadCode(DeadCodeArgs { .. }) => "dead_code",
            Commands::Viz(VizArgs { .. }) => "viz",
            Commands::Update(UpdateArgs { .. }) => "update",
            Commands::Watch(WatchArgs { .. }) => "watch",
            #[cfg(feature = "sync")]
            Commands::Sync { subcommand } => match subcommand {
                SyncSubcommands::Init { .. } => "sync_init",
                SyncSubcommands::Pair { .. } => "sync_pair",
                SyncSubcommands::Run { .. } => "sync_run",
                SyncSubcommands::Setup { .. } => "sync_setup",
                SyncSubcommands::Status { .. } => "sync_status",
                SyncSubcommands::Verify { .. } => "sync_verify",
                SyncSubcommands::Cursor { .. } => "sync_cursor",
                SyncSubcommands::Log { .. } => "sync_log",
            },
            Commands::SearchTrigrams(SearchTrigramsArgs { .. }) => "search_trigrams",
            Commands::Audit(AuditArgs { .. }) => "audit",
            Commands::Schedule { subcommand } => match subcommand {
                crate::commands::schedule::ScheduleSubcommands::SetupNightly { .. } => {
                    "schedule_setup_nightly"
                }
                crate::commands::schedule::ScheduleSubcommands::RunNightly => {
                    "schedule_run_nightly"
                }
            },
            #[cfg(feature = "daemon")]
            Commands::Daemon { .. } => "daemon",
            #[cfg(feature = "viz-server")]
            Commands::VizServer { .. } => "viz_server",
            #[cfg(feature = "web")]
            Commands::Web { command } => match command {
                WebCommands::Start(_) => "web_start",
                WebCommands::Stop => "web_stop",
                WebCommands::Status => "web_status",
            },
            Commands::Internal { command } => match command {
                InternalCommands::HookCommitMsg { .. } => "internal_hook_commit_msg",
                InternalCommands::HookPostCommit => "internal_hook_post_commit",
            },
            Commands::Demo(DemoArgs { .. }) => "demo",
            #[cfg(feature = "usage-metrics")]
            Commands::Usage { command } => match command {
                UsageCommands::Enable => "usage_enable",
                UsageCommands::Disable => "usage_disable",
                UsageCommands::Status => "usage_status",
                UsageCommands::ShowPayload => "usage_show_payload",
            },
            #[cfg(feature = "mcp")]
            Commands::Mcp { command } => match command {
                None | Some(McpCommands::Serve) => "mcp",
                Some(McpCommands::Install { .. }) => "mcp_install",
                Some(McpCommands::Uninstall { .. }) => "mcp_uninstall",
                Some(McpCommands::Status { .. }) => "mcp_status",
            },
            #[cfg(any(feature = "openapi", feature = "web"))]
            Commands::Openapi => "openapi",
        }
    }

    /// Canonical workload-shape key for `argv_hash`: subcommand path plus sorted
    /// present flag *names* (no values — paths, tx-ids, queries omitted).
    pub fn argv_shape(&self) -> String {
        let mut flags = self.present_flag_names();
        flags.sort_unstable();
        flags.dedup();
        if flags.is_empty() {
            self.command_name().to_string()
        } else {
            format!("{}|{}", self.command_name(), flags.join(","))
        }
    }
}
