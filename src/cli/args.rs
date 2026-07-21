use crate::commands::ask::Backend;
use crate::commands::bridge::BridgeCommands;
use crate::commands::data_models::DataModelSubcommands;
use crate::commands::observability::ObservabilitySubcommands;

use crate::commands::security::SecuritySubcommands;
use crate::ledger::types::Category;
use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    about = "Ledgerful change intelligence and transactional provenance for software engineering",
    long_about = None
)]
#[command(version)]
#[command(disable_help_subcommand = true)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,

    /// Enable verbose logging output
    #[arg(long, short, global = true)]
    pub verbose: bool,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Initialize Ledgerful in the current repository
    Init {
        /// Force re-initialization (overwrites existing config)
        #[arg(short, long)]
        force: bool,
        /// Start in enforce mode instead of the default observe mode
        #[arg(long)]
        enforce: bool,
    },

    /// Gate mode configuration
    Gate {
        #[command(subcommand)]
        command: GateCommands,
    },
    /// Evaluate declared repository policy (CI merge gate)
    Policy {
        #[command(subcommand)]
        command: PolicyCommands,
    },
    /// Guided onboarding wizard (welcome → init → doctor → first scan → success)
    Setup {
        /// Skip all prompts, accept defaults (for CI/scripted use)
        #[arg(short, long)]
        yes: bool,
        /// Skip the first-scan step
        #[arg(long)]
        skip_scan: bool,
    },
    /// Scan git changes and identify affected symbols
    Scan {
        /// Run impact analysis on changes
        #[arg(short, long)]
        impact: bool,
        /// Output a high-level summary only
        #[arg(short, long)]
        summary: bool,
        /// Output as JSON (requires --impact)
        #[arg(short, long)]
        json: bool,
        /// Write JSON output to file
        #[arg(short, long)]
        out: Option<PathBuf>,
        /// Git ref to compare against instead of working-tree status. Used in CI.
        #[arg(long, value_name = "REF")]
        base_ref: Option<String>,
        /// PR-style git range, e.g. `main...HEAD` or `main..HEAD`. Mutually
        /// exclusive with --impact.
        #[arg(long, value_name = "RANGE")]
        pr: Option<String>,
        /// Output format for PR scan: `json` (machine-readable) or `text`
        /// (human-readable). Requires --pr.
        #[arg(long, value_name = "FORMAT")]
        format: Option<String>,
    },
    /// Analyze impact of current changes
    Impact {
        /// Traverse all parent commits for temporal coupling
        #[arg(long)]
        all_parents: bool,
        /// Output a concise summary
        #[arg(short, long)]
        summary: bool,
        /// Enable telemetry coverage analysis
        #[arg(long)]
        telemetry: bool,
        /// Run dead-code analysis on affected files
        #[arg(long)]
        dead_code: bool,
        /// Output as JSON
        #[arg(long)]
        json: bool,
        /// Write output to file
        #[arg(short, long)]
        out: Option<PathBuf>,
    },
    /// Index the project for search and discovery
    Index {
        /// Perform incremental index (only changed files)
        #[arg(long, short)]
        incremental: bool,
        /// Force a full re-index
        #[arg(long, short)]
        full: bool,
        /// Refresh the knowledge graph (analyze structure)
        #[arg(long)]
        analyze_graph: bool,
        /// Index documentation files
        #[arg(long)]
        docs: bool,
        /// Index API contract files (OpenAPI/Swagger)
        #[arg(long)]
        contracts: bool,
        /// Index code snippets for semantic search (local embeddings)
        #[arg(long)]
        semantic: bool,
        /// Ingest an external SCIP index (Protobuf)
        #[arg(long)]
        scip: Option<std::path::PathBuf>,
        /// Automatically detect, generate, and ingest SCIP indices
        #[arg(long)]
        auto_scip: bool,
        /// Export knowledge graph data to passive documentation
        #[arg(long)]
        export_docs: bool,
        /// Filter exported documentation by type (e.g. mermaid, markdown)
        #[arg(long)]
        doc_type: Option<String>,
        /// Check index freshness
        #[arg(long)]
        check: bool,
        /// Output as JSON
        #[arg(long)]
        json: bool,
        /// Strict mode for check (exit 1 if stale)
        #[arg(long)]
        strict: bool,
        /// Number of parallel threads for semantic indexing (default: logical CPUs)
        #[arg(long, short = 'j')]
        concurrency: Option<usize>,
        /// Print resolved semantic settings and exit. Optionally takes a path for JSON output.
        #[arg(long, value_name = "OUTPUT_PATH", num_args = 0..=1)]
        semantic_dry_run: Option<Option<std::path::PathBuf>>,
        /// Use Gemini for semantic extraction (fast, large context) instead of local model
        #[arg(long)]
        fast: bool,
        /// Repair corrupt or missing completion metadata. Rebuilds index safely.
        #[arg(long)]
        repair_metadata: bool,
        /// Dry run for repair-metadata (shows proposed changes without writing)
        #[arg(long)]
        dry_run: bool,
        /// Automatically confirm repair operations (non-interactive)
        #[arg(long)]
        yes: bool,
    },
    /// Search the codebase using high-performance regex or semantic search
    Search {
        /// The query string
        query: String,
        /// Use regular expression search
        #[arg(short, long)]
        regex: bool,
        /// Use semantic search (requires local model and indexed snippets)
        #[arg(short, long)]
        semantic: bool,
        /// Limit the number of results
        #[arg(short, long, default_value_t = 10)]
        limit: usize,
        /// Force re-index before searching
        #[arg(short, long)]
        index: bool,
        /// Output results as NDJSON BridgeRecord entries
        #[arg(long)]
        json: bool,
        /// Automatically run incremental index before searching if the index is stale
        #[arg(long)]
        auto_index: bool,
        /// Use hybrid search (combines regex and BM25 results)
        #[arg(long)]
        hybrid: bool,
    },
    /// Rank files by change frequency and complexity (Hotspots)
    Hotspots {
        #[command(flatten)]
        args: HotspotArgs,
    },
    /// List and filter API endpoints
    Endpoints(crate::commands::endpoints::EndpointsArgs),
    /// Manage cross-repo federation
    Federate {
        #[command(subcommand)]
        command: FederateCommands,
    },
    /// Service boundary and topology commands
    Services {
        #[command(subcommand)]
        command: ServiceSubcommands,
    },
    /// Manage data models and schema migrations
    #[command(name = "data-models")]
    DataModels(crate::commands::data_models::DataModelsArgs),
    /// CI configuration and gate commands
    Ci(crate::commands::deploy::CiArgs),
    /// Deployment manifest and surface commands
    Deploy(crate::commands::deploy::DeployArgs),
    /// Manage project dependencies and security advisories
    Dependencies(crate::commands::dependencies::DependenciesArgs),
    /// Manage runtime observability and SLOs
    Observability(crate::commands::observability::ObservabilityArgs),
    /// Manage security boundaries and policies
    Security(crate::commands::security::SecurityArgs),
    /// List tests validating a specific entity
    Tests(crate::commands::test_mapping::TestsForEntityArgs),
    /// Manage the data interchange bridge (export/import Ledgerful state as versioned NDJSON).
    #[command(hide = true)]
    Bridge {
        #[command(subcommand)]
        subcommand: BridgeCommands,
    },
    /// Manage project ledger and transactional provenance
    #[command(
        long_about = "Manage project ledger and transactional provenance.\n\nNOTE: Ledgerful uses a two-step commit model. Git hooks cannot see the final hash pre-commit, so a pending sidecar is created first, and the post-commit hook promotes it to the ledger."
    )]
    Ledger {
        #[command(subcommand)]
        command: LedgerCommands,
    },
    /// Run verification plan (predictive Bayesian testing)
    Verify {
        /// Optional specific command or step to run
        command: Option<String>,
        /// Transaction ID to associate with this verification run
        #[arg(long)]
        tx_id: Option<String>,
        /// Timeout in seconds
        #[arg(long, short, default_value_t = 600)]
        timeout: u64,
        /// Disable Bayesian failure prediction
        #[arg(long)]
        no_predict: bool,
        /// Explain failure probability via local LLM for a specific entity
        #[arg(long)]
        explain: bool,
        /// Entity path for verification explanation (use with --explain; does not narrow executed steps)
        #[arg(long, short)]
        entity: Option<String>,
        /// Show detailed health of the verification system
        #[arg(long)]
        health: bool,
        /// Mathematically verify all transaction signatures in the ledger
        #[arg(long)]
        signatures: bool,
        /// Verify ledger chain continuity end-to-end (requires --signatures or
        /// validates the chain linkage separately)
        #[arg(long)]
        chain: bool,
        /// Compare the live chain head against a previously exported SOC2 zip
        #[arg(long, value_name = "PATH")]
        against_export: Option<std::path::PathBuf>,
        /// Show the verification plan without executing any commands
        #[arg(long)]
        dry_run: bool,
        /// Verification scope: `fast` (scoped test selection via test_mapping,
        /// for pre-push) or `full` (entire suite, for CI and manual runs).
        /// Default: `full`. The pre-push hook uses `fast`.
        #[arg(long, default_value = "full")]
        scope: crate::verify::plan::VerifyScope,
        /// Automatically refresh a stale or empty `test_mapping` index before
        /// scoped selection on `--scope fast`. Falls back to full suite with
        /// an announcement if indexing fails. Opt-in to avoid surprise latency.
        #[arg(long)]
        auto_index: bool,
    },
    /// Ask Gemini or a local model for assistance based on the current context
    Ask {
        /// The query to ask
        query: Option<String>,
        /// Use semantic search for code snippets instead of full impact context
        #[arg(long, short)]
        semantic: bool,
        /// Maximum number of code snippets to include in context
        #[arg(long, short, default_value_t = 10)]
        limit: usize,
        /// Gemini interaction mode
        #[arg(long, short, default_value = "analyze")]
        mode: crate::gemini::modes::GeminiMode,
        /// Enable narrative mode (Senior Architect summary)
        #[arg(long)]
        narrative: bool,
        /// Backend to use (local, gemini, ollama-cloud, openrouter, or auto)
        #[arg(long)]
        backend: Option<Backend>,
        /// Automatically run incremental index before querying if the index is stale
        #[arg(long)]
        auto_index: bool,
        /// Per-request timeout in seconds for LLM backend calls (default: 15).
        /// Prevents `ledgerful ask` from hanging when a backend is slow or unresponsive.
        #[arg(long, default_value_t = 15)]
        timeout: u64,
        /// Disable Knowledge Graph BM25 fallback when semantic index is empty
        #[arg(long)]
        no_kg_fallback: bool,
        /// Compute a fresh ImpactPacket in-memory from the live working tree
        /// instead of reading the cached packet, and suppress the stale-impact
        /// warning. Equivalent to running `scan --impact` before `ask` but
        /// without writing the report. See also `[ask].auto_scan_default`.
        #[arg(long)]
        auto_scan: bool,
    },
    /// Manage Ledgerful intent capture and TUI interaction
    Intent {
        #[command(subcommand)]
        command: IntentCommands,
    },
    /// Reset Ledgerful state or configuration
    Reset {
        /// Remove configuration file
        #[arg(long)]
        remove_config: bool,
        /// Remove local rules
        #[arg(long)]
        remove_rules: bool,
        /// Reset the ledger (history and pending transactions)
        #[arg(long)]
        include_ledger: bool,
        /// Remove all state and configuration (total reset)
        #[arg(long, short)]
        all: bool,
        /// Skip confirmation prompt
        #[arg(long, short = 'y')]
        yes: bool,
        /// Show what files/directories would be deleted without deleting them
        #[arg(long = "dry-run")]
        dry_run: bool,
    },
    /// Health check for Ledgerful and local model stack
    Doctor,
    /// Quick status check of the project ledger and pending transactions
    Status,
    /// Configuration management
    Config {
        #[command(subcommand)]
        command: ConfigCommands,
    },
    /// Detect likely dead code across the repository
    DeadCode {
        /// Minimum confidence threshold to report a finding
        #[arg(long, default_value_t = 0.75)]
        threshold: f64,
        /// Maximum number of findings to display
        #[arg(long, default_value_t = 50)]
        limit: usize,
        /// Automatically run incremental index before detection if the index is stale
        #[arg(long)]
        auto_index: bool,
        /// Include standard trait implementations (Eq, Ord, Clone, Debug, etc.) in
        /// results. By default these are suppressed because they are typically used
        /// implicitly via derive macros or blanket impls.
        #[arg(long)]
        include_traits: bool,
        /// Interactively prompt to remove high-confidence dead code and record the
        /// deletions as a pending ledger transaction.
        #[arg(long)]
        prune: bool,
        /// Show full per-symbol table instead of grouped-by-file view
        #[arg(long)]
        expand: bool,
        /// Explain why a specific file is flagged as dead code (per-symbol breakdown)
        #[arg(long)]
        explain: Option<String>,
    },
    /// Perform a holistic project audit or history for an entity
    Audit {
        /// Entity path to audit (e.g. src/main.rs)
        #[arg(short, long, conflicts_with = "pos_entity")]
        entity: Option<String>,
        /// Entity path to audit (positional fallback)
        #[arg(hide = true)]
        pos_entity: Option<String>,
        /// Include unaudited drift in the report
        #[arg(long, short)]
        include_unaudited: bool,
        /// Maximum number of entries to display
        #[arg(long, short, default_value_t = 10)]
        limit: usize,
        /// Offset for pagination
        #[arg(long, default_value_t = 0)]
        offset: usize,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Local-only per-command timing analysis (Track 0043; `--global` is Track 0044)
    Timings {
        /// Aggregate command timings across all discovered repos on disk (0044)
        #[arg(long)]
        global: bool,
        /// Output as JSON
        #[arg(long)]
        json: bool,
        /// Show the top N commands by total time (default 20)
        #[arg(long)]
        top: Option<u32>,
        /// Limit analysis to the last N days (default 30)
        #[arg(long)]
        days: Option<u32>,
        /// Write output to PATH (JSON for summary; collapsed stacks for --flame)
        #[arg(long, value_name = "PATH")]
        export: Option<PathBuf>,
        /// Show aggregated inner-span breakdown
        #[arg(long)]
        inner: bool,
        /// Filter --inner / --flame to a specific command name
        #[arg(long, value_name = "NAME")]
        command: Option<String>,
        /// Emit Brendan Gregg collapsed-stack text (speedscope-compatible)
        #[arg(long)]
        flame: bool,
        /// One-sentence explanation for a command (with week-over-week delta)
        #[arg(long, value_name = "COMMAND")]
        explain: Option<String>,
        /// Delete old timing rows (use with --older-than)
        #[arg(long)]
        prune: bool,
        /// Age threshold for --prune, e.g. 90d or 30d
        #[arg(long, value_name = "Nd")]
        older_than: Option<String>,
        /// Re-enable local self-timing capture
        #[arg(long, conflicts_with = "opt_out")]
        opt_in: bool,
        /// Disable local self-timing capture (writes self_timing = false)
        #[arg(long, conflicts_with = "opt_in")]
        opt_out: bool,
    },
    /// Generate an interactive visualization of the knowledge graph
    Viz {
        /// Custom output path for the HTML file
        #[arg(long, short, alias = "out")]
        output: Option<String>,
        /// Maximum number of nodes to include
        #[arg(long, short, default_value_t = 1000)]
        limit: usize,
        /// Maximum depth for relationship traversal
        #[arg(long, short, default_value_t = 2)]
        depth: usize,
        /// Filter by specific entity (root of the graph)
        #[arg(long, short)]
        entity: Option<String>,
        /// Visualization view: "graph" (default) or "services" (K4 service connectivity)
        #[arg(long, default_value = "graph")]
        view: String,
    },
    /// Update the Ledgerful binary or migrate repository state
    #[command(alias = "upgrade")]
    Update {
        /// Perform repository state migration (re-index and schema upgrade)
        #[arg(long)]
        migrate: bool,
        /// Update the Ledgerful binary to the latest version
        #[arg(long)]
        binary: bool,
        /// Skip confirmation prompts
        #[arg(long, short)]
        force: bool,
        /// Force unlock CozoDB by terminating other running Ledgerful processes
        #[arg(long = "force-unlock")]
        force_unlock: bool,
        /// Use fast semantic index bypass (skip LLM semantic extraction during migration)
        #[arg(long)]
        fast: bool,
        /// Show what update actions would be performed without executing them
        #[arg(long = "dry-run")]
        dry_run: bool,
        /// Rewrite retired Ledgerful hook commands to invoke `ledgerful`
        #[arg(long = "repair-hooks")]
        repair_hooks: bool,
    },
    /// Watch repository for changes and run incremental graph sync
    Watch {
        /// Throttle interval in milliseconds for debouncing file events.
        /// Defaults to `watch.debounce_ms` from config when not specified.
        #[arg(long, short, default_value_t = 0)]
        interval: u64,
        /// Output watch events as JSON
        #[arg(long, short)]
        json: bool,
        /// Disable Knowledge Graph sync during watch
        #[arg(long = "no-graph-sync")]
        no_graph_sync: bool,
    },
    /// Team ledger synchronization
    #[cfg(feature = "sync")]
    Sync {
        #[command(subcommand)]
        subcommand: SyncSubcommands,
    },
    /// Schedule nightly indexing and graph analysis tasks
    Schedule {
        #[command(subcommand)]
        subcommand: crate::commands::schedule::ScheduleSubcommands,
    },
    /// High-performance trigram-based search (low-level)
    #[command(hide = true)]
    SearchTrigrams {
        /// Trigrams to search for (space separated)
        trigrams: Vec<String>,
        /// Limit results
        #[arg(long, short, default_value_t = 100)]
        limit: usize,
    },
    #[cfg(feature = "daemon")]
    Daemon {
        /// The interval in milliseconds to batch events
        #[arg(long, short, default_value_t = 1000)]
        interval: u64,
    },
    /// Knowledge graph visualization server
    #[cfg(feature = "viz-server")]
    VizServer {
        /// Port to listen on
        #[arg(long, short, default_value_t = 9000)]
        port: u16,
        /// Address to bind to
        #[arg(long, short, default_value = "127.0.0.1")]
        bind: String,
        /// Open the visualization in the default browser
        #[arg(long)]
        open: bool,
        /// Stop a running visualization server
        #[arg(long)]
        stop: bool,
    },
    /// Export evidence artifacts (SOC2, etc.)
    Export {
        #[command(subcommand)]
        command: ExportCommands,
    },
    /// Launch the Ledgerful local web dashboard
    #[cfg(feature = "web")]
    Web {
        #[command(subcommand)]
        command: WebCommands,
    },
    /// Internal helper commands for git hooks and lifecycle management
    #[command(hide = true)]
    Internal {
        #[command(subcommand)]
        command: InternalCommands,
    },
    /// Manage opt-in usage metrics
    #[cfg(feature = "usage-metrics")]
    Usage {
        #[command(subcommand)]
        command: UsageCommands,
    },
    /// Run the MCP server (stdio transport)
    #[cfg(feature = "mcp")]
    Mcp,
    /// Print the canonical OpenAPI JSON spec for this build to stdout
    #[cfg(any(feature = "openapi", feature = "web"))]
    Openapi,

    /// Generate a disposable demonstration repo with signed ledger entries, cryptographic VALID proof, and a DEMO evidence export (see docs/golden-path.md)
    Demo {
        /// Keep the demo repo and openable DEMO evidence zip after completion (required for the golden-path walkthrough; default: clean up)
        #[arg(short, long)]
        keep: bool,
        /// Output directory for the demo repo (default: ./ledgerful-demo)
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// Overwrite a non-empty target directory
        #[arg(short, long)]
        force: bool,
    },
}

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
            Commands::Init { .. } => "init",
            Commands::Gate { command } => match command {
                GateCommands::Mode { mode } => {
                    if mode.is_some() {
                        "gate_mode_set"
                    } else {
                        "gate_mode_show"
                    }
                }
            },
            Commands::Policy { command } => match command {
                PolicyCommands::Check { .. } => "policy_check",
            },

            Commands::Setup { .. } => "setup",
            Commands::Scan { .. } => "scan",
            Commands::Impact { .. } => "impact",
            Commands::Index { .. } => "index",
            Commands::Search { .. } => "search",
            Commands::Hotspots { args } => match &args.command {
                Some(HotspotSubcommands::Trend { .. }) => "hotspots_trend",
                Some(HotspotSubcommands::Explain { .. }) => "hotspots_explain",
                Some(HotspotSubcommands::Budget { .. }) => "hotspots_budget",
                None => "hotspots",
            },
            Commands::Endpoints(_) => "endpoints",
            Commands::Export { command } => match command {
                ExportCommands::Evidence { .. } => "export_evidence",
            },
            Commands::Federate { command } => match command {
                FederateCommands::Export { .. } => "federate_export",
                FederateCommands::Scan => "federate_scan",
                FederateCommands::Status => "federate_status",
            },
            Commands::Services { command } => match command {
                ServiceSubcommands::Diff(_) => "services_diff",
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
            },
            Commands::Verify { .. } => "verify",
            Commands::Ask { .. } => "ask",
            Commands::Intent { command } => match command {
                IntentCommands::Demo => "intent_demo",
            },
            Commands::Reset { .. } => "reset",
            Commands::Doctor => "doctor",
            Commands::Status => "status",
            Commands::Timings { .. } => "timings",
            Commands::Config { command } => match command {
                ConfigCommands::Verify { .. } => "config_verify",
                ConfigCommands::View { .. } => "config_view",
                ConfigCommands::Schema { .. } => "config_schema",
                ConfigCommands::Diff { .. } => "config_diff",
                ConfigCommands::Set { .. } => "config_set",
                ConfigCommands::Unset { .. } => "config_unset",
            },
            Commands::DeadCode { .. } => "dead_code",
            Commands::Viz { .. } => "viz",
            Commands::Update { .. } => "update",
            Commands::Watch { .. } => "watch",
            #[cfg(feature = "sync")]
            Commands::Sync { subcommand } => match subcommand {
                SyncSubcommands::Init { .. } => "sync_init",
                SyncSubcommands::Pair { .. } => "sync_pair",
                SyncSubcommands::Run { .. } => "sync_run",
                SyncSubcommands::Status => "sync_status",
                SyncSubcommands::Verify { .. } => "sync_verify",
                SyncSubcommands::Cursor { .. } => "sync_cursor",
                SyncSubcommands::Log { .. } => "sync_log",
            },
            Commands::SearchTrigrams { .. } => "search_trigrams",
            Commands::Audit { .. } => "audit",
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
            Commands::Demo { .. } => "demo",
            #[cfg(feature = "usage-metrics")]
            Commands::Usage { command } => match command {
                UsageCommands::Enable => "usage_enable",
                UsageCommands::Disable => "usage_disable",
                UsageCommands::Status => "usage_status",
                UsageCommands::ShowPayload => "usage_show_payload",
            },
            #[cfg(feature = "mcp")]
            Commands::Mcp => "mcp",
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

    /// Long flag names that are present (values stripped). Used only for hashing.
    fn present_flag_names(&self) -> Vec<&'static str> {
        let mut f = Vec::new();
        match self {
            Commands::Init { force, enforce } => {
                if *force {
                    f.push("force");
                }
                if *enforce {
                    f.push("enforce");
                }
            }
            Commands::Scan {
                impact,
                summary,
                json,
                out,
                base_ref,
                pr,
                format,
            } => {
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
            }
            Commands::Impact {
                all_parents,
                summary,
                telemetry,
                dead_code,
                json,
                out,
            } => {
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
            }
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
            } => {
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
            Commands::Verify {
                command,
                tx_id,
                no_predict,
                explain,
                entity,
                health,
                signatures,
                chain,
                against_export,
                dry_run,
                auto_index,
                // `scope` always present (default full) — include name only when not default
                // would leak value; we record the flag name always when user would care.
                // Values are stripped: record "scope" unconditionally so fast/full group
                // separately only if we include the enum discriminant without user paths.
                // Spec: flag *names* only. Recording "scope" for every verify run is fine
                // and keeps path/tx_id out of the hash.
                scope: _,
                timeout: _,
            } => {
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
                if *dry_run {
                    f.push("dry_run");
                }
                // Always note scope was part of the shape (defaulted or not).
                f.push("scope");
                if *auto_index {
                    f.push("auto_index");
                }
            }
            Commands::Timings {
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
            } => {
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
            Commands::Search {
                query: _,
                regex,
                semantic,
                limit,
                index,
                json,
                auto_index,
                hybrid,
            } => {
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
                match &args.command {
                    Some(HotspotSubcommands::Trend {
                        entity,
                        days: _,
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
            Commands::Ask {
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
            } => {
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
                if *timeout != 15 {
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
            Commands::DeadCode {
                threshold,
                limit,
                auto_index,
                include_traits,
                prune,
                expand,
                explain,
            } => {
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
            }
            Commands::Ledger { command } => match command {
                LedgerCommands::Start { .. } => {
                    // category/message/entity are values — names only via presence of required flags.
                    f.push("category");
                    f.push("message");
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
                    dry_run,
                    yes,
                } => {
                    if tx.is_some() {
                        f.push("tx");
                    }
                    if *all_invalid {
                        f.push("all_invalid");
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
            },
            Commands::Setup { yes, skip_scan } => {
                if *yes {
                    f.push("yes");
                }
                if *skip_scan {
                    f.push("skip_scan");
                }
            }
            Commands::Policy { command } => match command {
                PolicyCommands::Check {
                    pr,
                    fail_on,
                    policy,
                    format,
                } => {
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
            Commands::Audit {
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
            Commands::Doctor => {}
            Commands::Status => {}
            Commands::Gate { command } => match command {
                // `mode` is a positional optional value, not a long flag.
                GateCommands::Mode { .. } => {}
            },
            Commands::Viz {
                output,
                limit,
                depth,
                entity,
                view,
            } => {
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
            Commands::Update {
                migrate,
                binary,
                force,
                force_unlock,
                fast,
                dry_run,
                repair_hooks,
            } => {
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
            Commands::Watch {
                interval,
                json,
                no_graph_sync,
            } => {
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
            Commands::Reset {
                remove_config,
                remove_rules,
                include_ledger,
                all,
                yes,
                dry_run,
            } => {
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
            },
            Commands::Federate { command } => match command {
                FederateCommands::Export { dry_run, out } => {
                    if *dry_run {
                        f.push("dry_run");
                    }
                    if out.is_some() {
                        f.push("out");
                    }
                }
                FederateCommands::Scan | FederateCommands::Status => {}
            },
            Commands::Services { command } => match command {
                ServiceSubcommands::Diff(args) => {
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
                crate::commands::deploy::CiSubcommands::Diff { json } => {
                    if *json {
                        f.push("json");
                    }
                }
            },
            Commands::Deploy(args) => match &args.command {
                crate::commands::deploy::DeploySubcommands::Impact { changed, json } => {
                    if *changed {
                        f.push("changed");
                    }
                    if *json {
                        f.push("json");
                    }
                }
            },
            Commands::Dependencies(args) => match &args.command {
                crate::commands::dependencies::DependencySubcommands::List { json, verbose } => {
                    if *json {
                        f.push("json");
                    }
                    if *verbose {
                        f.push("verbose");
                    }
                }
                crate::commands::dependencies::DependencySubcommands::Audit { json, .. } => {
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
            Commands::Demo {
                keep,
                output,
                force,
            } => {
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
            Commands::SearchTrigrams { limit, .. } => {
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
                SyncSubcommands::Pair { .. } => {}
                SyncSubcommands::Run { once } => {
                    if *once {
                        f.push("once");
                    }
                }
                SyncSubcommands::Status => {}
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
            Commands::Mcp => {}
            #[cfg(any(feature = "openapi", feature = "web"))]
            Commands::Openapi => {}
        }
        f
    }
}

#[cfg(feature = "sync")]
#[derive(Subcommand, Debug)]
pub enum SyncSubcommands {
    /// Initialize sync for this device
    Init {
        /// Force re-initialization (overwrites existing key)
        #[arg(short, long)]
        force: bool,
        /// Inject a test secret (hex encoded) for non-interactive use
        #[arg(long, hide = true)]
        with_secret: Option<String>,
    },
    /// Generate or accept a pairing code
    Pair {
        /// The pairing code to accept
        code: Option<String>,
    },
    /// Run the sync loop
    Run {
        /// Run only once and exit
        #[arg(long)]
        once: bool,
    },
    /// Show sync status
    Status,
    /// Verify the integrity of sync bundles
    Verify {
        /// Path to the bundle file
        path: String,
    },
    /// Manage sync cursors
    Cursor {
        /// Set a specific cursor HLC
        #[arg(long)]
        set: Option<String>,
    },
    /// Show sync logs
    Log {
        /// Number of lines to tail
        #[arg(long, short)]
        tail: Option<usize>,
    },
}

#[derive(Subcommand, Debug)]
pub enum ExportCommands {
    /// Export a SOC2 evidence bundle as a zip file
    Evidence {
        /// Export profile (currently only "soc2")
        #[arg(long, default_value = "soc2")]
        profile: String,
        /// Output file path (default: ./ledgerful-soc2-evidence.zip)
        #[arg(short, long)]
        out: Option<std::path::PathBuf>,
        /// Overwrite an existing file
        #[arg(short, long)]
        force: bool,
        /// Control ID(s) to scope the export (e.g. CC8.1, CC7.*). Repeatable.
        #[arg(long = "control")]
        control: Vec<String>,
    },
}

#[derive(Subcommand, Debug)]
#[cfg(feature = "web")]
pub enum WebCommands {
    /// Start the ledgerful web dashboard server
    Start(WebStartArgs),
    /// Stop a running ledgerful web dashboard server
    Stop,
    /// Show whether the ledgerful web server is running
    Status,
}

#[derive(Args, Debug)]
#[cfg(feature = "web")]
pub struct WebStartArgs {
    /// Port to listen on
    #[arg(long, short, default_value_t = 52001)]
    pub port: u16,
    /// Address to bind to
    #[arg(long, short, default_value = "127.0.0.1")]
    pub bind: String,
    /// Serve a custom SPA directory instead of the embedded dashboard
    #[arg(long)]
    pub spa_dir: Option<camino::Utf8PathBuf>,
    /// Open the dashboard in the default browser
    #[arg(long)]
    pub open: bool,
    /// Allow binding to non-loopback addresses
    #[arg(long)]
    pub allow_public: bool,
    /// Run the server in the background
    #[arg(long)]
    pub background: bool,
    /// Pre-generated session token (used when daemonizing so the parent and
    /// child share the same authenticated URL).
    #[arg(long, hide = true)]
    pub token: Option<String>,
}

#[derive(Subcommand, Debug)]
pub enum AdrSubcommands {
    /// Export MADR files from ledger history
    Export {
        /// Output path for ADR files
        #[arg(short, long, alias = "output-dir", default_value = "docs/adr")]
        output: String,
        /// Filter entries from the last N days
        #[arg(short, long)]
        days: Option<u64>,
    },
    /// Update lifecycle status of an ADR
    UpdateStatus {
        /// ADR ID (transaction ID or prefix)
        adr_id: String,
        /// New status
        #[arg(value_enum)]
        status: crate::ledger::types::AdrStatus,
    },
    /// Link an ADR as superseding another
    Link {
        /// Current ADR ID
        adr_id: String,
        /// ID of the ADR being superseded
        #[arg(short, long)]
        supersedes: String,
    },
    /// Record a review for an ADR
    Review {
        /// ADR ID
        adr_id: String,
        /// Optional review notes
        #[arg(short, long)]
        message: Option<String>,
    },
    /// List all ADRs in the ledger
    List,
}

#[derive(Subcommand, Debug)]
pub enum ServiceSubcommands {
    /// Show service boundary changes and topology
    Diff(crate::commands::services_diff::ServicesDiffArgs),
}

#[derive(Subcommand, Debug)]
pub enum ValidatorSubcommands {
    /// List all registered commit validators
    List {
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Enable a commit validator
    Enable {
        /// Name of the validator
        name: String,
    },
    /// Disable a commit validator
    Disable {
        /// Name of the validator
        name: String,
    },
    /// Remove a commit validator from the registry
    Remove {
        /// Name of the validator
        name: String,
    },
    /// Check validator executables and report health
    Doctor,
}

#[derive(Subcommand, Debug)]
pub enum InternalCommands {
    /// Internal git hook command for commit message validation
    #[command(name = "hook-commit-msg")]
    HookCommitMsg {
        /// The file containing the commit message
        msg_file: PathBuf,
    },
    /// Internal git hook command for post-commit processing
    #[command(name = "hook-post-commit")]
    HookPostCommit,
}

#[derive(Args, Debug)]
pub struct HotspotArgs {
    #[command(subcommand)]
    pub command: Option<HotspotSubcommands>,

    /// Limit the number of hotspots displayed
    #[arg(short, long)]
    pub limit: Option<usize>,

    /// Number of commits to analyze
    #[arg(short, long)]
    pub commits: Option<usize>,

    /// Number of days to analyze
    #[arg(short, long)]
    pub days: Option<u32>,

    /// Specific commit to start from
    #[arg(long)]
    pub since: Option<String>,

    /// Output as JSON
    #[arg(long)]
    pub json: bool,

    /// Automatically run incremental index before calculation if the index is stale
    #[arg(long)]
    pub auto_index: bool,

    /// Traverse all parent commits (useful for branch merges)
    #[arg(long)]
    pub all_parents: bool,

    /// Include centrality data (requires prior `index --analyze-graph`)
    #[arg(long)]
    pub centrality: bool,

    /// Filter by entity path
    #[arg(short, long)]
    pub entity: Option<String>,

    /// Find semantically similar code clusters (duplication hotspots)
    #[arg(long, short)]
    pub semantic: bool,

    /// Persist the results as a snapshot in the history tables
    #[arg(long)]
    pub snapshot: bool,
}

#[derive(Subcommand, Debug)]
pub enum HotspotSubcommands {
    /// Show hotspot and temporal coupling trends over time
    Trend {
        /// Entity path to filter by
        #[arg(short, long)]
        entity: Option<String>,
        /// Number of days to look back
        #[arg(short, long, default_value_t = 30)]
        days: u32,
        /// Output as JSON
        #[arg(long)]
        json: bool,
        /// Backfill trend history from historical commits without mutating the
        /// working tree. Records hotspot scores for the last N commits.
        #[arg(long)]
        bootstrap: bool,
        /// Number of historical commits to sample during --bootstrap
        /// (default: 30).
        #[arg(long, requires = "bootstrap")]
        samples: Option<usize>,
        /// Re-bootstrap from scratch without prompting, clearing any existing
        /// trend data.
        #[arg(long, requires = "bootstrap")]
        force: bool,
    },
    /// Explain why a file is a hotspot or highly coupled
    Explain {
        /// Entity path to explain
        entity: String,
    },
    /// Check hotspot and coupling budgets
    Budget {
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand, Debug)]
pub enum IntentCommands {
    /// Launch the interactive intent confirmation UI with mock data
    Demo,
}

#[derive(Subcommand, Debug)]
pub enum GateCommands {
    /// Show or set the gate mode
    Mode {
        /// Set mode: observe or enforce
        mode: Option<String>,
    },
}

#[derive(Subcommand, Debug)]
pub enum PolicyCommands {
    /// Evaluate declared policy against PR/diff/ledger state
    Check {
        /// PR-style git range, e.g. `main...HEAD` or `main..HEAD`
        #[arg(long, value_name = "RANGE")]
        pr: Option<String>,
        /// Risk threshold that fails the check: off | low | medium | high
        /// (overrides config `rules.fail_on` for this run)
        #[arg(long, value_name = "LEVEL")]
        fail_on: Option<String>,
        /// Trusted policy file path (org/CI). When set, this path is used
        /// instead of base-branch or working-tree policy resolution.
        #[arg(long, value_name = "PATH")]
        policy: Option<PathBuf>,
        /// Output format: `json` (machine contract) or `text` (human report).
        /// Default: text.
        #[arg(long, value_name = "FORMAT")]
        format: Option<String>,
    },
}

#[derive(Subcommand, Debug)]
pub enum FederateCommands {
    /// Export public interfaces for other repositories to consume
    Export {
        /// Preview the schema without writing to .ledgerful/state/schema.json
        #[arg(long, short = 'd')]
        dry_run: bool,
        /// Custom output path for the schema file
        #[arg(long, short)]
        out: Option<String>,
    },
    /// Scan sibling directories for Ledgerful schemas
    Scan,
    /// Show status of federated links
    Status,
}

#[derive(Subcommand, Debug)]
pub enum LedgerCommands {
    /// Start a new change transaction
    Start {
        /// Entity path to track
        entity: String,
        /// Category of change (ARCHITECTURE, FEATURE, BUGFIX, REFACTOR, INFRA, SECURITY, DOCS, CHORE, TOOLING)
        #[arg(short, long)]
        category: Category,
        /// Intent message for the change
        #[arg(short, long)]
        message: String,
    },
    /// Finalize and commit a change transaction
    Commit {
        /// Transaction ID to commit (optional, defaults to current)
        tx_id: Option<String>,
        /// Summary of the change
        #[arg(short, long)]
        summary: String,
        /// Reason for the change (Architecture Decision)
        #[arg(short, long)]
        reason: String,
        /// Mark as a breaking change
        #[arg(long)]
        breaking: bool,
        /// Bypass verification gate enforcement
        #[arg(long)]
        force: bool,
        /// Create a git commit after the ledger commit succeeds
        #[arg(long)]
        with_git: bool,
        /// Override the generated git commit message
        #[arg(long, requires = "with_git")]
        git_message: Option<String>,
        /// Skip adding a git Signed-off-by trailer
        #[arg(long, requires = "with_git")]
        no_signoff: bool,
        /// Print the git commit command without executing it
        #[arg(long, requires = "with_git")]
        dry_run: bool,
    },
    /// Roll back an active transaction
    Rollback {
        /// Transaction ID to rollback (optional, defaults to current)
        tx_id: Option<String>,
        /// Reason for the rollback
        #[arg(short, long)]
        reason: String,
    },
    /// Record a surgical atomic change without a full session
    Atomic {
        /// Entity path
        entity: String,
        /// Category of change
        #[arg(short, long)]
        category: Category,
        /// Summary
        #[arg(short, long)]
        summary: String,
        /// Reason
        #[arg(short, long)]
        reason: String,
        /// Bypass verification gate enforcement
        #[arg(long)]
        force: bool,
    },
    /// Show status of active transactions and uncommitted drift
    Status {
        /// Show all historical transactions
        #[arg(short, long)]
        all: bool,
        /// Filter status by entity path
        #[arg(short, long)]
        entity: Option<String>,
        /// Output a compact view
        #[arg(short, long)]
        compact: bool,
        /// Exit with 1 if there is unaudited drift
        #[arg(long)]
        exit_code: bool,
        /// Perform signature verification and exit with 1 if signatures are invalid
        #[arg(long = "verify-signatures")]
        verify_signatures: bool,
        /// Output as JSON
        #[arg(long)]
        json: bool,
        /// Aggregate posture across all discovered repos on disk
        #[arg(long)]
        global: bool,
        /// Scope the global rollup to a single repo path
        #[arg(long, requires = "global")]
        repo: Option<String>,
        /// Force a fresh walk of the configured roots
        #[arg(long, requires = "global")]
        reindex: bool,
        /// Disable the global rollup view in user config
        #[arg(long, conflicts_with = "opt_in", requires = "global")]
        opt_out: bool,
        /// Re-enable the global rollup view in user config
        #[arg(long, conflicts_with = "opt_out", requires = "global")]
        opt_in: bool,
    },
    /// Register a new tech stack rule or commit validator
    Register {
        #[command(subcommand)]
        command: RegisterCommands,
    },
    /// Show active tech stack enforcement rules
    Stack {
        /// Filter by category (e.g. Database, Auth)
        category: Option<Category>,
    },
    /// Architectural Decision Records (MADR format)
    Adr {
        #[command(subcommand)]
        command: AdrSubcommands,
    },
    /// Manage commit validators
    Validator {
        #[command(subcommand)]
        command: ValidatorSubcommands,
    },
    /// Show the entity graph neighborhood governed by a transaction
    Graph(crate::commands::ledger_graph::LedgerGraphArgs),
    /// Full-text search across ledger history
    Search {
        /// Search query
        query: String,
        /// Filter by category
        #[arg(short, long)]
        category: Option<Category>,
        /// Number of days to look back
        #[arg(short, long)]
        days: Option<u64>,
        /// Filter by breaking changes only
        #[arg(short, long)]
        breaking: bool,
        /// Limit results
        #[arg(short, long, default_value_t = 10)]
        limit: usize,
        /// Offset for pagination
        #[arg(long, default_value_t = 0)]
        offset: usize,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Reconcile detected drift with a transaction or pattern
    Reconcile {
        /// Transaction ID to associate drift with
        #[arg(short, long)]
        tx_id: Option<String>,
        /// File pattern to reconcile (glob)
        #[arg(short, long)]
        pattern: Option<String>,
        /// Reconcile all current drift
        #[arg(long)]
        all: bool,
        /// Reason for reconciliation
        #[arg(short, long)]
        reason: Option<String>,
    },
    /// Adopt drift as a new committed transaction
    Adopt {
        /// File pattern to adopt
        #[arg(short, long)]
        pattern: Option<String>,
        /// Adopt all current drift
        #[arg(long)]
        all: bool,
        /// Category for the new transaction
        #[arg(short, long)]
        category: Category,
        /// Summary for the new transaction
        #[arg(short, long)]
        summary: String,
        /// Reason for the new transaction
        #[arg(short, long)]
        reason: String,
    },
    /// Perform a holistic project audit or history for an entity
    Audit {
        /// Entity path to audit (e.g. src/main.rs)
        #[arg(short, long, conflicts_with = "pos_entity")]
        entity: Option<String>,
        /// Entity path to audit (positional fallback)
        #[arg(hide = true)]
        pos_entity: Option<String>,
        /// Include unaudited drift in the report
        #[arg(long, short)]
        include_unaudited: bool,
        /// Maximum number of entries to display
        #[arg(long, short, default_value_t = 10)]
        limit: usize,
        /// Offset for pagination
        #[arg(long, default_value_t = 0)]
        offset: usize,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Add a lightweight note/lesson to a transaction for an entity
    Note {
        /// Entity path
        entity: String,
        /// The note content
        #[arg(required_unless_present = "message")]
        note: Option<String>,
        /// The note content (takes precedence over positional note)
        #[arg(short, long)]
        message: Option<String>,
    },
    /// Re-sign ledger entries with invalid signatures (key-repair)
    ReSign {
        /// Re-sign a single transaction by id or prefix
        #[arg(short, long, conflicts_with = "all_invalid")]
        tx: Option<String>,
        /// Re-sign all entries whose stored signatures fail verification
        #[arg(long, conflicts_with = "tx")]
        all_invalid: bool,
        /// Preview candidates and keys that would be used; do not mutate
        #[arg(long = "dry-run")]
        dry_run: bool,
        /// Skip interactive confirmation and proceed with backup + re-sign
        #[arg(long)]
        yes: bool,
    },
    /// Garbage collect orphaned or stale ledger entries
    Gc {
        /// Remove PENDING transactions older than TTL
        #[arg(long)]
        stale: bool,
        /// Remove transactions with no corresponding git commit
        #[arg(long)]
        orphans: bool,
        /// Time-to-live for PENDING transactions in hours (used with --stale)
        #[arg(long, default_value_t = 72)]
        ttl_hours: u64,
        /// Force removal without confirmation
        #[arg(short, long)]
        force: bool,
        /// Show what would be removed without actually deleting it
        #[arg(long = "dry-run")]
        dry_run: bool,
    },
    /// Resume a pending transaction by ID or the most recent pending transaction
    Resume {
        /// Transaction ID to resume (optional, defaults to most recent pending)
        #[arg(short, long = "tx", value_name = "TX_ID")]
        tx_id: Option<String>,
    },
    /// Export committed ledger entries as a stable JSON provenance artifact
    ExportProvenance {
        /// Output path for the JSON provenance file (default: stdout)
        #[arg(short, long, value_name = "PATH")]
        out_path: Option<PathBuf>,
        /// Overwrite an existing output file
        #[arg(short, long)]
        force: bool,
    },
    /// Export a redacted, cryptographically verifiable public ledger bundle
    ExportPublic {
        /// Output directory for the bundle files
        #[arg(short, long, value_name = "DIR")]
        output: PathBuf,
        /// Sign the manifest with the bot keypair
        #[arg(long)]
        sign: bool,
        /// Override the bot key directory (holds bot key, bot public key, and pseudonym secret)
        #[arg(long, value_name = "PATH")]
        key: Option<PathBuf>,
    },
    /// Repair a stale post-commit hook sidecar after a crash
    HookRepair {
        /// Roll back the stale transaction and remove the sidecar
        #[arg(short, long)]
        force: bool,
    },
}

#[derive(Subcommand, Debug)]
pub enum RegisterCommands {
    /// Register a forbidden term (tech stack enforcement)
    Rule {
        /// Forbidden term or technology name
        term: String,
        /// Category (e.g. Database, ORM)
        #[arg(short, long)]
        category: Category,
        /// Reason for prohibition
        #[arg(short, long)]
        reason: String,
    },
    /// Register a commit validator script
    Validator {
        /// Name of the validator
        name: String,
        /// Command to execute (supports {entity} placeholder)
        #[arg(short = 'x', long)]
        command: String,
        /// Category this validator applies to (or 'ALL')
        #[arg(short, long)]
        category: String,
        /// Timeout in seconds
        #[arg(long, default_value_t = 30)]
        timeout: u64,
    },
}

#[derive(Subcommand, Debug)]
pub enum ConfigCommands {
    /// Verify current configuration and environment health
    Verify {
        /// Output results as JSON
        #[arg(long)]
        json: bool,
        /// Filter by specific section name (e.g. backend, semantic)
        #[arg(long, short)]
        section: Option<String>,
        /// Include defaults that are normally hidden
        #[arg(long, short)]
        verbose: bool,
    },
    /// View resolved project configuration
    View {
        /// Output as JSON
        #[arg(long)]
        json: bool,
        /// Filter view by section (e.g. local_model)
        #[arg(long, short)]
        section: Option<String>,
        /// Filter view by key within section (requires --section, or searches top-level)
        #[arg(long, short)]
        key: Option<String>,
    },
    /// Manage environment and config schemas
    Schema {
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Show differences between declared and inferred config
    Diff {
        /// Output as JSON
        #[arg(long)]
        json: bool,
        /// Show all env vars including internal ones (no filtering)
        #[arg(long)]
        show_internal: bool,
    },
    /// Set a configuration value in .ledgerful/config.toml by dotted key
    /// (e.g. `coverage.services.enabled=true`). Preserves comments and
    /// formatting. Value is parsed as TOML (bool/int/float/string/array);
    /// an unquoted bareword that is not valid TOML is stored as a string.
    Set {
        /// Dotted key and TOML value, e.g. `coverage.services.enabled=true`
        key_value: String,
    },
    /// Remove an array-of-tables entry from .ledgerful/config.toml by
    /// indexed key (e.g. `ask.providers.priority[1]`).
    Unset {
        /// Dotted key with array index, e.g. `ask.providers.priority[1]`
        key: String,
    },
}

#[derive(Subcommand, Debug)]
#[cfg(feature = "usage-metrics")]
pub enum UsageCommands {
    /// Enable anonymous usage metrics
    Enable,
    /// Disable anonymous usage metrics
    Disable,
    /// Show usage metrics status
    Status,
    /// Show the exact payload that would be sent
    ShowPayload,
}
