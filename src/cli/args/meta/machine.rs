use crate::cli::args::*;
use crate::commands::bridge::BridgeCommands;
use crate::commands::data_models::DataModelSubcommands;
use crate::commands::observability::ObservabilitySubcommands;
use crate::commands::security::SecuritySubcommands;

impl Commands {
    /// Whether this invocation is machine-facing and must keep stdout free of
    /// human `cli_summary` product lines (track 0093 machine mode).
    ///
    /// Selected by any `--json` flag, `scan --format json`, or `mcp`.
    /// **No wildcard arm** — a new subcommand or `--json` flag must decide
    /// explicitly at compile time rather than silently defaulting to human.
    pub fn is_machine_output(&self) -> bool {
        match self {
            Commands::Init(InitArgs { .. }) => false,
            Commands::Gate { .. } => false,
            Commands::Policy { command } => match command {
                // Policy uses `--format json` (not `--json`); treat as machine.
                // Bare parent defaults to check with format=None (text).
                None => false,
                Some(PolicyCommands::Check { format, .. }) => format
                    .as_deref()
                    .is_some_and(|f| f.eq_ignore_ascii_case("json")),
            },
            // Parent `--json` (global) even when `command` is None (T18).
            Commands::Release { json, .. } => *json,
            Commands::Setup(SetupArgs { .. }) => false,
            Commands::Scan(ScanArgs { json, format, .. }) => {
                *json
                    || format
                        .as_deref()
                        .is_some_and(|f| f.eq_ignore_ascii_case("json"))
            }
            Commands::Impact(ImpactArgs { json, .. }) => *json,
            Commands::ChangeContext(ChangeContextArgs { json, .. }) => *json,
            Commands::Session(SessionArgs { json }) => *json,
            Commands::Index(IndexArgs { json, .. }) => *json,
            Commands::Search(SearchCliArgs {
                json, json_lines, ..
            }) => *json || *json_lines,
            Commands::Hotspots { args } => {
                args.json
                    || match &args.command {
                        Some(HotspotSubcommands::Trend { json, .. }) => *json,
                        Some(HotspotSubcommands::Budget { json }) => *json,
                        Some(HotspotSubcommands::Explain { .. }) => false,
                        None => false,
                    }
            }
            Commands::Endpoints(args) => args.wants_json(),
            Commands::Symbols(args) => args.wants_json(),
            Commands::Surfaces(SurfacesArgs { json }) => *json,
            Commands::Export { command } => match command {
                // 0182: pure stdout ChainHead JSON must stay free of SUCCESS banners.
                ExportCommands::Head { out, stdout, .. } => {
                    *stdout || out.as_ref().is_some_and(|p| p.as_os_str() == "-")
                }
                // Evidence remains file-only zip; never machine/stdout product body.
                ExportCommands::Evidence { .. } => false,
            },
            Commands::Federate { .. } => false,
            Commands::Services { command } => match command {
                None => false,
                Some(ServiceSubcommands::Diff(args)) => args.json,
            },
            Commands::DataModels(args) => match &args.command {
                DataModelSubcommands::List { json, .. } => *json,
                DataModelSubcommands::Impact { json, .. } => *json,
            },
            Commands::Ci(args) => match &args.command {
                None => false,
                Some(crate::commands::deploy::CiSubcommands::Diff { json }) => *json,
            },
            Commands::Deploy(args) => match &args.command {
                None => false,
                Some(crate::commands::deploy::DeploySubcommands::Impact { json, .. }) => *json,
            },
            Commands::Dependencies(args) => match &args.command {
                None => false,
                Some(crate::commands::dependencies::DependencySubcommands::List {
                    json, ..
                }) => *json,
                Some(crate::commands::dependencies::DependencySubcommands::Audit {
                    json, ..
                }) => *json,
            },
            Commands::Observability(args) => match &args.command {
                ObservabilitySubcommands::Coverage { json } => *json,
                ObservabilitySubcommands::Diff { json } => *json,
            },
            Commands::Security(args) => match &args.command {
                SecuritySubcommands::Impact { json, .. } => *json,
                SecuritySubcommands::Boundaries { json } => *json,
            },
            Commands::Tests(args) => args.json,
            Commands::Bridge { subcommand } => match subcommand {
                BridgeCommands::Export { json, .. } => *json,
                BridgeCommands::Import { .. } => false,
                BridgeCommands::Query { .. } => false,
            },
            Commands::Ledger { command } => match command {
                LedgerCommands::Start { .. } => false,
                LedgerCommands::Commit { .. } => false,
                LedgerCommands::Rollback { .. } => false,
                LedgerCommands::Atomic { .. } => false,
                LedgerCommands::Status { json, .. } => *json,
                LedgerCommands::Register { .. } => false,
                LedgerCommands::Stack { .. } => false,
                LedgerCommands::Adr { .. } => false,
                LedgerCommands::Validator { command } => match command {
                    ValidatorSubcommands::List { json } => *json,
                    ValidatorSubcommands::Enable { .. } => false,
                    ValidatorSubcommands::Disable { .. } => false,
                    ValidatorSubcommands::Remove { .. } => false,
                    ValidatorSubcommands::Doctor => false,
                },
                LedgerCommands::Graph(args) => args.json,
                LedgerCommands::Search { json, .. } => *json,
                LedgerCommands::Reconcile { .. } => false,
                LedgerCommands::Adopt { .. } => false,
                LedgerCommands::Audit { json, .. } => *json,
                LedgerCommands::Note { .. } => false,
                LedgerCommands::ReSign { .. } => false,
                LedgerCommands::Gc { .. } => false,
                LedgerCommands::Resume { .. } => false,
                LedgerCommands::ExportProvenance { .. } => false,
                LedgerCommands::ExportPublic { .. } => false,
                LedgerCommands::HookRepair { .. } => false,
                LedgerCommands::RecoverOrphan { .. } => false,
            },
            Commands::Verify(VerifyArgs { json, .. }) => *json,
            Commands::Ask(AskArgs { .. }) => false,
            Commands::Intent { .. } => false,
            Commands::Reset(ResetArgs { .. }) => false,
            Commands::Doctor(DoctorArgs { json, .. }) => *json,
            Commands::Status(StatusArgs { json, .. }) => *json,
            Commands::Timings(TimingsCliArgs { json, .. }) => *json,
            Commands::Config { command } => match command {
                ConfigCommands::Verify { json, .. } => *json,
                ConfigCommands::View { json, .. } => *json,
                ConfigCommands::Schema { json } => *json,
                ConfigCommands::Diff { json, .. } => *json,
                ConfigCommands::Set { .. } => false,
                ConfigCommands::Unset { .. } => false,
            },
            Commands::DeadCode(DeadCodeArgs { json, .. }) => *json,
            Commands::Viz(VizArgs { .. }) => false,
            Commands::Update(UpdateArgs { .. }) => false,
            Commands::Watch(WatchArgs { json, .. }) => *json,
            #[cfg(feature = "sync")]
            Commands::Sync { subcommand } => match subcommand {
                SyncSubcommands::Setup { json, .. } => *json,
                SyncSubcommands::Status { json } => *json,
                _ => false,
            },
            Commands::SearchTrigrams(SearchTrigramsArgs { .. }) => false,
            Commands::Audit(AuditArgs { json, .. }) => *json,
            Commands::Schedule { .. } => false,
            #[cfg(feature = "daemon")]
            Commands::Daemon { .. } => false,
            #[cfg(feature = "viz-server")]
            Commands::VizServer { .. } => false,
            #[cfg(feature = "web")]
            Commands::Web { .. } => false,
            Commands::Internal { .. } => false,
            Commands::Demo(DemoArgs { .. }) => false,
            #[cfg(feature = "usage-metrics")]
            Commands::Usage { .. } => false,
            #[cfg(feature = "mcp")]
            Commands::Mcp { command } => match command {
                None | Some(McpCommands::Serve) => true,
                Some(McpCommands::Install { json, .. })
                | Some(McpCommands::Uninstall { json, .. })
                | Some(McpCommands::Status { json }) => *json,
            },
            #[cfg(any(feature = "openapi", feature = "web"))]
            Commands::Openapi => true,
        }
    }
}
