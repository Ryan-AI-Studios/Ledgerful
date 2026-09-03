use super::*;
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
}

impl Commands {
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

    /// Long flag names that are present (values stripped). Used only for hashing.
    fn present_flag_names(&self) -> Vec<&'static str> {
        let mut f = Vec::new();
        match self {
            Commands::Init(InitArgs { force, enforce }) => {
                if *force {
                    f.push("force");
                }
                if *enforce {
                    f.push("enforce");
                }
            }
            Commands::ChangeContext(ChangeContextArgs {
                json,
                detail,
                max_files,
                base_ref,
                blast_depth,
                paths,
                include_governance,
            }) => {
                if *json {
                    f.push("json");
                }
                if detail != "minimal" {
                    f.push("detail");
                }
                if *max_files != 20 {
                    f.push("max_files");
                }
                if base_ref.is_some() {
                    f.push("base_ref");
                }
                if blast_depth.is_some() {
                    f.push("blast_depth");
                }
                if !paths.is_empty() {
                    f.push("paths");
                }
                if *include_governance {
                    f.push("include_governance");
                }
            }
            Commands::Session(SessionArgs { json }) => {
                if *json {
                    f.push("json");
                }
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
                mode,
                full,
            }) => {
                if *impact {
                    f.push("impact");
                }
                if *summary {
                    f.push("summary");
                }
                if *json {
                    f.push("json");
                }
                if out.is_some() {
                    f.push("out");
                }
                if base_ref.is_some() {
                    f.push("base_ref");
                }
                if pr.is_some() {
                    f.push("pr");
                }
                if format.is_some() {
                    f.push("format");
                }
                if blast_depth.is_some() {
                    f.push("blast_depth");
                }
                if !paths.is_empty() {
                    f.push("paths");
                }
                if *include_governance {
                    f.push("include_governance");
                }
                if mode.is_some() {
                    f.push("mode");
                }
                if *full {
                    f.push("full");
                }
            }
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
            }) => {
                if *all_parents {
                    f.push("all_parents");
                }
                if *summary {
                    f.push("summary");
                }
                if *telemetry {
                    f.push("telemetry");
                }
                if *dead_code {
                    f.push("dead_code");
                }
                if *json {
                    f.push("json");
                }
                if out.is_some() {
                    f.push("out");
                }
                if blast_depth.is_some() {
                    f.push("blast_depth");
                }
                if !paths.is_empty() {
                    f.push("paths");
                }
                if *include_governance {
                    f.push("include_governance");
                }
            }
            Commands::Index(IndexArgs {
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
            }) => {
                if *incremental {
                    f.push("incremental");
                }
                if *full {
                    f.push("full");
                }
                if *analyze_graph {
                    f.push("analyze_graph");
                }
                if *docs {
                    f.push("docs");
                }
                if *contracts {
                    f.push("contracts");
                }
                if *semantic {
                    f.push("semantic");
                }
                if scip.is_some() {
                    f.push("scip");
                }
                if *auto_scip {
                    f.push("auto_scip");
                }
                if *export_docs {
                    f.push("export_docs");
                }
                if doc_type.is_some() {
                    f.push("doc_type");
                }
                if *check {
                    f.push("check");
                }
                if *json {
                    f.push("json");
                }
                if *strict {
                    f.push("strict");
                }
                if concurrency.is_some() {
                    f.push("concurrency");
                }
                if semantic_dry_run.is_some() {
                    f.push("semantic_dry_run");
                }
                if *fast {
                    f.push("fast");
                }
                if *repair_metadata {
                    f.push("repair_metadata");
                }
                if *dry_run {
                    f.push("dry_run");
                }
                if *yes {
                    f.push("yes");
                }
            }
            Commands::Verify(VerifyArgs {
                command,
                tx_id,
                no_predict,
                explain,
                entity,
                health,
                signatures,
                chain,
                against_export,
                exact,
                strict_signatures,
                dry_run,
                auto_index,
                allow_full_fallback,
                json,
                // `scope` always present (default full) — include name only when not default
                // would leak value; we record the flag name always when user would care.
                // Values are stripped: record "scope" unconditionally so fast/full group
                // separately only if we include the enum discriminant without user paths.
                // Spec: flag *names* only. Recording "scope" for every verify run is fine
                // and keeps path/tx_id out of the hash.
                scope: _,
                timeout: _,
            }) => {
                if command.is_some() {
                    f.push("command");
                }
                if tx_id.is_some() {
                    f.push("tx_id");
                }
                if *no_predict {
                    f.push("no_predict");
                }
                if *explain {
                    f.push("explain");
                }
                if entity.is_some() {
                    f.push("entity");
                }
                if *health {
                    f.push("health");
                }
                if *signatures {
                    f.push("signatures");
                }
                if *chain {
                    f.push("chain");
                }
                if against_export.is_some() {
                    f.push("against_export");
                }
                if *exact {
                    f.push("exact");
                }
                if *strict_signatures {
                    f.push("strict_signatures");
                }
                if *dry_run {
                    f.push("dry_run");
                }
                // Always note scope was part of the shape (defaulted or not).
                f.push("scope");
                if *auto_index {
                    f.push("auto_index");
                }
                if *allow_full_fallback {
                    f.push("allow_full_fallback");
                }
                if *json {
                    f.push("json");
                }
            }
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
                if *global {
                    f.push("global");
                }
                if *json {
                    f.push("json");
                }
                if top.is_some() {
                    f.push("top");
                }
                if days.is_some() {
                    f.push("days");
                }
                if export.is_some() {
                    f.push("export");
                }
                if *inner {
                    f.push("inner");
                }
                if command.is_some() {
                    f.push("command");
                }
                if *flame {
                    f.push("flame");
                }
                if explain.is_some() {
                    f.push("explain");
                }
                if *prune {
                    f.push("prune");
                }
                if older_than.is_some() {
                    f.push("older_than");
                }
                if *opt_in {
                    f.push("opt_in");
                }
                if *opt_out {
                    f.push("opt_out");
                }
            }
            Commands::Search(SearchCliArgs {
                query: _,
                regex,
                semantic,
                limit,
                index,
                json,
                json_lines,
                auto_index,
                hybrid,
            }) => {
                // Values (query text) never enter the shape — flag names only.
                if *regex {
                    f.push("regex");
                }
                if *semantic {
                    f.push("semantic");
                }
                // `limit` always has a clap default; record when non-default so
                // workload-affecting overrides group separately.
                if *limit != 10 {
                    f.push("limit");
                }
                if *index {
                    f.push("index");
                }
                if *json {
                    f.push("json");
                }
                if *json_lines {
                    f.push("json_lines");
                }
                if *auto_index {
                    f.push("auto_index");
                }
                if *hybrid {
                    f.push("hybrid");
                }
            }
            Commands::Hotspots { args } => {
                if args.limit.is_some() {
                    f.push("limit");
                }
                if args.commits.is_some() {
                    f.push("commits");
                }
                if args.days.is_some() {
                    f.push("days");
                }
                if args.since.is_some() {
                    f.push("since");
                }
                if args.json {
                    f.push("json");
                }
                if args.auto_index {
                    f.push("auto_index");
                }
                if args.all_parents {
                    f.push("all_parents");
                }
                if args.centrality {
                    f.push("centrality");
                }
                if args.entity.is_some() {
                    f.push("entity");
                }
                if args.semantic {
                    f.push("semantic");
                }
                if args.snapshot {
                    f.push("snapshot");
                }
                if args.include.is_some() {
                    f.push("include");
                }
                match &args.command {
                    Some(HotspotSubcommands::Trend {
                        entity,
                        days: _,
                        limit: _,
                        all,
                        json,
                        bootstrap,
                        samples,
                        force,
                    }) => {
                        f.push("trend");
                        if entity.is_some() {
                            f.push("entity");
                        }
                        // days always present with default on Trend — always part of shape
                        f.push("days");
                        // limit always present with default on Trend (summary cap)
                        f.push("limit");
                        if *all {
                            f.push("all");
                        }
                        if *json {
                            f.push("json");
                        }
                        if *bootstrap {
                            f.push("bootstrap");
                        }
                        if samples.is_some() {
                            f.push("samples");
                        }
                        if *force {
                            f.push("force");
                        }
                    }
                    Some(HotspotSubcommands::Explain { .. }) => {
                        f.push("explain");
                    }
                    Some(HotspotSubcommands::Budget { json }) => {
                        f.push("budget");
                        if *json {
                            f.push("json");
                        }
                    }
                    None => {}
                }
            }
            Commands::Endpoints(args) => {
                f.extend(args.present_flag_names());
            }
            Commands::Symbols(args) => {
                f.extend(args.present_flag_names());
            }
            Commands::Surfaces(SurfacesArgs { json }) => {
                if *json {
                    f.push("json");
                }
            }
            Commands::Ask(AskArgs {
                query: _,
                semantic,
                limit,
                mode: _,
                narrative,
                backend,
                auto_index,
                timeout,
                no_kg_fallback,
                auto_scan,
            }) => {
                if *semantic {
                    f.push("semantic");
                }
                if *limit != 10 {
                    f.push("limit");
                }
                // mode always present (default analyze) — include name for shape.
                f.push("mode");
                if *narrative {
                    f.push("narrative");
                }
                if backend.is_some() {
                    f.push("backend");
                }
                if *auto_index {
                    f.push("auto_index");
                }
                if timeout.is_some() {
                    f.push("timeout");
                }
                if *no_kg_fallback {
                    f.push("no_kg_fallback");
                }
                if *auto_scan {
                    f.push("auto_scan");
                }
            }
            Commands::Config { command } => match command {
                ConfigCommands::Verify {
                    json,
                    section,
                    verbose,
                } => {
                    if *json {
                        f.push("json");
                    }
                    if section.is_some() {
                        f.push("section");
                    }
                    if *verbose {
                        f.push("verbose");
                    }
                }
                ConfigCommands::View { json, section, key } => {
                    if *json {
                        f.push("json");
                    }
                    if section.is_some() {
                        f.push("section");
                    }
                    if key.is_some() {
                        f.push("key");
                    }
                }
                ConfigCommands::Schema { json } => {
                    if *json {
                        f.push("json");
                    }
                }
                ConfigCommands::Diff {
                    json,
                    show_internal,
                } => {
                    if *json {
                        f.push("json");
                    }
                    if *show_internal {
                        f.push("show_internal");
                    }
                }
                ConfigCommands::Set { .. } | ConfigCommands::Unset { .. } => {}
            },
            Commands::DeadCode(DeadCodeArgs {
                threshold,
                limit,
                auto_index,
                include_traits,
                prune,
                expand,
                explain,
                json,
            }) => {
                if (*threshold - 0.75).abs() > f64::EPSILON {
                    f.push("threshold");
                }
                if *limit != 50 {
                    f.push("limit");
                }
                if *auto_index {
                    f.push("auto_index");
                }
                if *include_traits {
                    f.push("include_traits");
                }
                if *prune {
                    f.push("prune");
                }
                if *expand {
                    f.push("expand");
                }
                if explain.is_some() {
                    f.push("explain");
                }
                if *json {
                    f.push("json");
                }
            }
            Commands::Ledger { command } => match command {
                LedgerCommands::Start { force, .. } => {
                    // category/message/entity are values — names only via presence of required flags.
                    f.push("category");
                    f.push("message");
                    if *force {
                        f.push("force");
                    }
                }
                LedgerCommands::Commit {
                    tx_id,
                    summary: _,
                    reason: _,
                    breaking,
                    force,
                    with_git,
                    git_message,
                    no_signoff,
                    dry_run,
                } => {
                    if tx_id.is_some() {
                        f.push("tx_id");
                    }
                    f.push("summary");
                    f.push("reason");
                    if *breaking {
                        f.push("breaking");
                    }
                    if *force {
                        f.push("force");
                    }
                    if *with_git {
                        f.push("with_git");
                    }
                    if git_message.is_some() {
                        f.push("git_message");
                    }
                    if *no_signoff {
                        f.push("no_signoff");
                    }
                    if *dry_run {
                        f.push("dry_run");
                    }
                }
                LedgerCommands::Status {
                    all,
                    entity,
                    compact,
                    exit_code,
                    strict_observe_signal,
                    verify_signatures,
                    json,
                    global,
                    repo,
                    reindex,
                    opt_out,
                    opt_in,
                } => {
                    if *all {
                        f.push("all");
                    }
                    if entity.is_some() {
                        f.push("entity");
                    }
                    if *compact {
                        f.push("compact");
                    }
                    if *exit_code {
                        f.push("exit_code");
                    }
                    if *strict_observe_signal {
                        f.push("strict_observe_signal");
                    }
                    if *verify_signatures {
                        f.push("verify_signatures");
                    }
                    if *json {
                        f.push("json");
                    }
                    if *global {
                        f.push("global");
                    }
                    if repo.is_some() {
                        f.push("repo");
                    }
                    if *reindex {
                        f.push("reindex");
                    }
                    if *opt_out {
                        f.push("opt_out");
                    }
                    if *opt_in {
                        f.push("opt_in");
                    }
                }
                LedgerCommands::Search {
                    query: _,
                    category,
                    days,
                    breaking,
                    limit,
                    offset,
                    json,
                    include_rollback,
                } => {
                    if category.is_some() {
                        f.push("category");
                    }
                    if days.is_some() {
                        f.push("days");
                    }
                    if *breaking {
                        f.push("breaking");
                    }
                    if *limit != 10 {
                        f.push("limit");
                    }
                    if *offset != 0 {
                        f.push("offset");
                    }
                    if *json {
                        f.push("json");
                    }
                    if *include_rollback {
                        f.push("include_rollback");
                    }
                }
                LedgerCommands::Rollback { .. } => {
                    f.push("reason");
                }
                LedgerCommands::Atomic { force, .. } => {
                    f.push("category");
                    f.push("summary");
                    f.push("reason");
                    if *force {
                        f.push("force");
                    }
                }
                LedgerCommands::Reconcile {
                    tx_id,
                    pattern,
                    all,
                    reason,
                } => {
                    if tx_id.is_some() {
                        f.push("tx_id");
                    }
                    if pattern.is_some() {
                        f.push("pattern");
                    }
                    if *all {
                        f.push("all");
                    }
                    if reason.is_some() {
                        f.push("reason");
                    }
                }
                LedgerCommands::Adopt {
                    pattern,
                    all,
                    category: _,
                    summary: _,
                    reason: _,
                } => {
                    if pattern.is_some() {
                        f.push("pattern");
                    }
                    if *all {
                        f.push("all");
                    }
                    f.push("category");
                    f.push("summary");
                    f.push("reason");
                }
                LedgerCommands::Gc {
                    stale,
                    orphans,
                    ttl_hours,
                    force,
                    dry_run,
                } => {
                    if *stale {
                        f.push("stale");
                    }
                    if *orphans {
                        f.push("orphans");
                    }
                    if *ttl_hours != 72 {
                        f.push("ttl_hours");
                    }
                    if *force {
                        f.push("force");
                    }
                    if *dry_run {
                        f.push("dry_run");
                    }
                }
                LedgerCommands::Register { command } => match command {
                    RegisterCommands::Rule { .. } => {
                        f.push("category");
                        f.push("reason");
                    }
                    RegisterCommands::Validator { timeout, .. } => {
                        f.push("command");
                        f.push("category");
                        if *timeout != 30 {
                            f.push("timeout");
                        }
                    }
                },
                // Optional category is positional (not a long flag).
                LedgerCommands::Stack { .. } => {}
                LedgerCommands::Adr { command } => match command {
                    AdrSubcommands::Export { output: _, days } => {
                        // output always has a default path — record presence of days only
                        // when set; output path is a value and never enters the hash, but
                        // the flag name is always part of export shape.
                        f.push("output");
                        if days.is_some() {
                            f.push("days");
                        }
                    }
                    AdrSubcommands::UpdateStatus { .. } => {}
                    AdrSubcommands::Link { .. } => {
                        f.push("supersedes");
                    }
                    AdrSubcommands::Review { message, .. } => {
                        if message.is_some() {
                            f.push("message");
                        }
                    }
                    AdrSubcommands::List => {}
                },
                LedgerCommands::Validator { command } => match command {
                    ValidatorSubcommands::List { json } => {
                        if *json {
                            f.push("json");
                        }
                    }
                    ValidatorSubcommands::Enable { .. }
                    | ValidatorSubcommands::Disable { .. }
                    | ValidatorSubcommands::Remove { .. }
                    | ValidatorSubcommands::Doctor => {}
                },
                LedgerCommands::Graph(args) => {
                    if args.json {
                        f.push("json");
                    }
                }
                LedgerCommands::Audit {
                    entity,
                    pos_entity: _,
                    include_unaudited,
                    limit,
                    offset,
                    json,
                } => {
                    if entity.is_some() {
                        f.push("entity");
                    }
                    if *include_unaudited {
                        f.push("include_unaudited");
                    }
                    if *limit != 10 {
                        f.push("limit");
                    }
                    if *offset != 0 {
                        f.push("offset");
                    }
                    if *json {
                        f.push("json");
                    }
                }
                LedgerCommands::Note { message, .. } => {
                    if message.is_some() {
                        f.push("message");
                    }
                }
                LedgerCommands::ReSign {
                    tx,
                    all_invalid,
                    all,
                    dry_run,
                    yes,
                } => {
                    if tx.is_some() {
                        f.push("tx");
                    }
                    if *all_invalid {
                        f.push("all_invalid");
                    }
                    if *all {
                        f.push("all");
                    }
                    if *dry_run {
                        f.push("dry_run");
                    }
                    if *yes {
                        f.push("yes");
                    }
                }
                LedgerCommands::Resume { tx_id } => {
                    if tx_id.is_some() {
                        f.push("tx");
                    }
                }
                LedgerCommands::ExportProvenance { out_path, force } => {
                    if out_path.is_some() {
                        f.push("out_path");
                    }
                    if *force {
                        f.push("force");
                    }
                }
                LedgerCommands::ExportPublic { sign, key, .. } => {
                    // output is required — values stripped; record flag name only.
                    f.push("output");
                    if *sign {
                        f.push("sign");
                    }
                    if key.is_some() {
                        f.push("key");
                    }
                }
                LedgerCommands::HookRepair { force } => {
                    if *force {
                        f.push("force");
                    }
                }
                LedgerCommands::RecoverOrphan {
                    promote,
                    abandon,
                    reason,
                } => {
                    if *promote {
                        f.push("promote");
                    }
                    if *abandon {
                        f.push("abandon");
                    }
                    if reason.is_some() {
                        f.push("reason");
                    }
                }
            },
            Commands::Setup(SetupArgs { yes, skip_scan }) => {
                if *yes {
                    f.push("yes");
                }
                if *skip_scan {
                    f.push("skip_scan");
                }
            }
            Commands::Policy { command } => match command {
                None => {}
                Some(PolicyCommands::Check {
                    pr,
                    fail_on,
                    policy,
                    format,
                }) => {
                    if pr.is_some() {
                        f.push("pr");
                    }
                    if fail_on.is_some() {
                        f.push("fail_on");
                    }
                    if policy.is_some() {
                        f.push("policy");
                    }
                    if format.is_some() {
                        f.push("format");
                    }
                }
            },
            // Parent `json` for both `None` and `Some(Pins)` so T18 argv_shape matches.
            Commands::Release { json, command: _ } => {
                if *json {
                    f.push("json");
                }
            }
            Commands::Audit(AuditArgs {
                entity,
                pos_entity: _,
                include_unaudited,
                limit,
                offset,
                json,
            }) => {
                if entity.is_some() {
                    f.push("entity");
                }
                if *include_unaudited {
                    f.push("include_unaudited");
                }
                if *limit != 10 {
                    f.push("limit");
                }
                if *offset != 0 {
                    f.push("offset");
                }
                if *json {
                    f.push("json");
                }
            }
            Commands::Doctor(DoctorArgs {
                json,
                apply_hook_refresh,
                dry_run,
                full,
                fix,
                yes,
            }) => {
                if *json {
                    f.push("json");
                }
                if *apply_hook_refresh {
                    f.push("apply-hook-refresh");
                }
                if *fix {
                    f.push("fix");
                }
                if *yes {
                    f.push("yes");
                }
                if *dry_run {
                    f.push("dry-run");
                }
                if *full {
                    f.push("full");
                }
            }
            Commands::Status(StatusArgs { json, compact }) => {
                if *json {
                    f.push("json");
                }
                if *compact {
                    f.push("compact");
                }
            }
            Commands::Gate { command } => match command {
                // `mode` is a positional optional value, not a long flag.
                None | Some(GateCommands::Mode { .. }) => {}
            },
            Commands::Viz(VizArgs {
                output,
                limit,
                depth,
                entity,
                view,
            }) => {
                if output.is_some() {
                    f.push("output");
                }
                if *limit != 1000 {
                    f.push("limit");
                }
                if *depth != 2 {
                    f.push("depth");
                }
                if entity.is_some() {
                    f.push("entity");
                }
                if view != "graph" {
                    f.push("view");
                }
            }
            Commands::Update(UpdateArgs {
                migrate,
                binary,
                force,
                force_unlock,
                fast,
                dry_run,
                repair_hooks,
            }) => {
                if *migrate {
                    f.push("migrate");
                }
                if *binary {
                    f.push("binary");
                }
                if *force {
                    f.push("force");
                }
                if *force_unlock {
                    f.push("force_unlock");
                }
                if *fast {
                    f.push("fast");
                }
                if *dry_run {
                    f.push("dry_run");
                }
                if *repair_hooks {
                    f.push("repair_hooks");
                }
            }
            Commands::Watch(WatchArgs {
                interval,
                json,
                no_graph_sync,
            }) => {
                if *interval != 0 {
                    f.push("interval");
                }
                if *json {
                    f.push("json");
                }
                if *no_graph_sync {
                    f.push("no_graph_sync");
                }
            }
            Commands::Reset(ResetArgs {
                remove_config,
                remove_rules,
                include_ledger,
                all,
                yes,
                dry_run,
            }) => {
                if *remove_config {
                    f.push("remove_config");
                }
                if *remove_rules {
                    f.push("remove_rules");
                }
                if *include_ledger {
                    f.push("include_ledger");
                }
                if *all {
                    f.push("all");
                }
                if *yes {
                    f.push("yes");
                }
                if *dry_run {
                    f.push("dry_run");
                }
            }
            Commands::Export { command } => match command {
                ExportCommands::Evidence {
                    profile: _,
                    out,
                    force,
                    control,
                } => {
                    // profile always present with default — include name for shape.
                    f.push("profile");
                    if out.is_some() {
                        f.push("out");
                    }
                    if *force {
                        f.push("force");
                    }
                    if !control.is_empty() {
                        f.push("control");
                    }
                }
                ExportCommands::Head { out, force, stdout } => {
                    let out_is_dash = out.as_ref().is_some_and(|p| p.as_os_str() == "-");
                    let stdout_mode = *stdout || out_is_dash;
                    if stdout_mode {
                        f.push("stdout");
                    }
                    // Record real path outs only (not the `-` stdout sentinel).
                    if out.is_some() && !out_is_dash {
                        f.push("out");
                    }
                    if !stdout_mode && *force {
                        f.push("force");
                    }
                }
            },
            Commands::Federate { command } => match command {
                None | Some(FederateCommands::Scan) | Some(FederateCommands::Status) => {}
                Some(FederateCommands::Export { dry_run, out }) => {
                    if *dry_run {
                        f.push("dry_run");
                    }
                    if out.is_some() {
                        f.push("out");
                    }
                }
            },
            Commands::Services { command } => match command {
                None => {}
                Some(ServiceSubcommands::Diff(args)) => {
                    if args.full {
                        f.push("full");
                    }
                    if args.json {
                        f.push("json");
                    }
                }
            },
            Commands::DataModels(args) => match &args.command {
                DataModelSubcommands::List {
                    all,
                    min_confidence,
                    json,
                } => {
                    if *all {
                        f.push("all");
                    }
                    if (*min_confidence - 0.5).abs() > f64::EPSILON {
                        f.push("min_confidence");
                    }
                    if *json {
                        f.push("json");
                    }
                }
                DataModelSubcommands::Impact { changed, json } => {
                    if *changed {
                        f.push("changed");
                    }
                    if *json {
                        f.push("json");
                    }
                }
            },
            Commands::Ci(args) => match &args.command {
                None => {}
                Some(crate::commands::deploy::CiSubcommands::Diff { json: true }) => {
                    f.push("json");
                }
                Some(crate::commands::deploy::CiSubcommands::Diff { json: false }) => {}
            },
            Commands::Deploy(args) => match &args.command {
                None => {}
                Some(crate::commands::deploy::DeploySubcommands::Impact { changed, json }) => {
                    if *changed {
                        f.push("changed");
                    }
                    if *json {
                        f.push("json");
                    }
                }
            },
            Commands::Dependencies(args) => match &args.command {
                None => {}
                Some(crate::commands::dependencies::DependencySubcommands::List {
                    json,
                    verbose,
                    all,
                }) => {
                    if *json {
                        f.push("json");
                    }
                    if *verbose {
                        f.push("verbose");
                    }
                    if *all {
                        f.push("all");
                    }
                }
                Some(crate::commands::dependencies::DependencySubcommands::Audit {
                    json, ..
                }) => {
                    // input path is a value — never hashed; only flag name.
                    f.push("input");
                    if *json {
                        f.push("json");
                    }
                }
            },
            Commands::Observability(args) => match &args.command {
                ObservabilitySubcommands::Coverage { json }
                | ObservabilitySubcommands::Diff { json } => {
                    if *json {
                        f.push("json");
                    }
                }
            },
            Commands::Security(args) => match &args.command {
                SecuritySubcommands::Impact { changed, json } => {
                    if *changed {
                        f.push("changed");
                    }
                    if *json {
                        f.push("json");
                    }
                }
                SecuritySubcommands::Boundaries { json } => {
                    if *json {
                        f.push("json");
                    }
                }
            },
            Commands::Tests(args) => {
                if args.entity.is_some() {
                    f.push("entity");
                }
                if args.json {
                    f.push("json");
                }
            }
            Commands::Bridge { subcommand } => match subcommand {
                BridgeCommands::Export {
                    out,
                    stdout,
                    pretty,
                    hotspots,
                    ledger,
                    scope,
                    madr,
                    json,
                } => {
                    if out.is_some() {
                        f.push("out");
                    }
                    if *stdout {
                        f.push("stdout");
                    }
                    if *pretty {
                        f.push("pretty");
                    }
                    if *hotspots {
                        f.push("hotspots");
                    }
                    if *ledger {
                        f.push("ledger");
                    }
                    if scope.is_some() {
                        f.push("scope");
                    }
                    if *madr {
                        f.push("madr");
                    }
                    if *json {
                        f.push("json");
                    }
                }
                BridgeCommands::Import { .. } => {
                    f.push("input");
                }
                BridgeCommands::Query { .. } => {}
            },
            Commands::Intent { command } => match command {
                IntentCommands::Demo => {}
            },
            Commands::Schedule { subcommand } => match subcommand {
                crate::commands::schedule::ScheduleSubcommands::SetupNightly {
                    dry_run,
                    uninstall,
                } => {
                    if *dry_run {
                        f.push("dry_run");
                    }
                    if *uninstall {
                        f.push("uninstall");
                    }
                }
                crate::commands::schedule::ScheduleSubcommands::RunNightly => {}
            },
            Commands::Demo(DemoArgs {
                keep,
                output,
                force,
            }) => {
                if *keep {
                    f.push("keep");
                }
                if output.is_some() {
                    f.push("output");
                }
                if *force {
                    f.push("force");
                }
            }
            Commands::SearchTrigrams(SearchTrigramsArgs { limit, .. }) => {
                if *limit != 100 {
                    f.push("limit");
                }
            }
            Commands::Internal { command } => match command {
                // msg_file is positional; no long flag names.
                InternalCommands::HookCommitMsg { .. } | InternalCommands::HookPostCommit => {}
            },
            #[cfg(feature = "daemon")]
            Commands::Daemon { interval } => {
                if *interval != 1000 {
                    f.push("interval");
                }
            }
            #[cfg(feature = "viz-server")]
            Commands::VizServer {
                port,
                bind,
                open,
                stop,
            } => {
                if *port != 9000 {
                    f.push("port");
                }
                if bind != "127.0.0.1" {
                    f.push("bind");
                }
                if *open {
                    f.push("open");
                }
                if *stop {
                    f.push("stop");
                }
            }
            #[cfg(feature = "web")]
            Commands::Web { command } => match command {
                WebCommands::Start(args) => {
                    if args.port != 52001 {
                        f.push("port");
                    }
                    if args.bind != "127.0.0.1" {
                        f.push("bind");
                    }
                    if args.spa_dir.is_some() {
                        f.push("spa_dir");
                    }
                    if args.open {
                        f.push("open");
                    }
                    if args.allow_public {
                        f.push("allow_public");
                    }
                    if args.background {
                        f.push("background");
                    }
                    // print_token defaults to false (0090 W1); record only when opted in.
                    if args.print_token {
                        f.push("print_token");
                    }
                    if args.token.is_some() {
                        f.push("token");
                    }
                }
                WebCommands::Stop | WebCommands::Status => {}
            },
            #[cfg(feature = "sync")]
            Commands::Sync { subcommand } => match subcommand {
                SyncSubcommands::Init { force, with_secret } => {
                    if *force {
                        f.push("force");
                    }
                    if with_secret.is_some() {
                        f.push("with_secret");
                    }
                }
                SyncSubcommands::Pair {
                    code,
                    list,
                    revoke,
                    force,
                } => {
                    if code.is_some() {
                        f.push("code");
                    }
                    if *list {
                        f.push("list");
                    }
                    if revoke.is_some() {
                        f.push("revoke");
                    }
                    if *force {
                        f.push("force");
                    }
                }
                SyncSubcommands::Run { once } => {
                    if *once {
                        f.push("once");
                    }
                }
                SyncSubcommands::Setup { enable, json } => {
                    if *enable {
                        f.push("enable");
                    }
                    if *json {
                        f.push("json");
                    }
                }
                SyncSubcommands::Status { json } => {
                    if *json {
                        f.push("json");
                    }
                }
                SyncSubcommands::Verify { .. } => {
                    // path is positional value — never hashed.
                }
                SyncSubcommands::Cursor { set } => {
                    if set.is_some() {
                        f.push("set");
                    }
                }
                SyncSubcommands::Log { tail } => {
                    if tail.is_some() {
                        f.push("tail");
                    }
                }
            },
            #[cfg(feature = "usage-metrics")]
            Commands::Usage { command: _ } => {}
            #[cfg(feature = "mcp")]
            Commands::Mcp { command } => match command {
                None | Some(McpCommands::Serve) => {}
                Some(McpCommands::Install {
                    platforms,
                    scope,
                    launcher,
                    dry_run,
                    force,
                    no_backup,
                    json,
                }) => {
                    if !platforms.is_empty() {
                        f.push("platform");
                    }
                    if scope.is_some() {
                        f.push("scope");
                    }
                    // launcher always has a default; record non-default only
                    if *launcher != McpLauncher::Auto {
                        f.push("launcher");
                    }
                    if *dry_run {
                        f.push("dry_run");
                    }
                    if *force {
                        f.push("force");
                    }
                    if *no_backup {
                        f.push("no_backup");
                    }
                    if *json {
                        f.push("json");
                    }
                }
                Some(McpCommands::Uninstall {
                    platforms,
                    scope,
                    dry_run,
                    json,
                }) => {
                    if !platforms.is_empty() {
                        f.push("platform");
                    }
                    if scope.is_some() {
                        f.push("scope");
                    }
                    if *dry_run {
                        f.push("dry_run");
                    }
                    if *json {
                        f.push("json");
                    }
                }
                Some(McpCommands::Status { json }) => {
                    if *json {
                        f.push("json");
                    }
                }
            },
            #[cfg(any(feature = "openapi", feature = "web"))]
            Commands::Openapi => {}
        }
        f
    }
}

#[cfg(test)]
mod machine_output_tests {
    use super::*;
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
        Cli::try_parse_from(["ledgerful", "doctor", "--fix", "--dry-run"])
            .expect("--fix --dry-run");
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
        let cli =
            Cli::try_parse_from(["ledgerful", "doctor", "--fix", "--yes"]).expect("--fix --yes");
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
            Cli::try_parse_from(["ledgerful", "ledger", "re-sign", "--all", "--all-invalid",])
                .is_err(),
            "--all must conflict with --all-invalid"
        );
        assert!(
            Cli::try_parse_from(["ledgerful", "ledger", "re-sign", "--all", "--tx", "abc"])
                .is_err(),
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
            err.contains("invalid value")
                || err.contains("possible values")
                || err.contains("fast"),
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
}
