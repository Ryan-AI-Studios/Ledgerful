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
    /// Manage the Ledgerful bridge (AI-Brains integration)
    Bridge {
        #[command(subcommand)]
        subcommand: BridgeCommands,
    },
    /// Manage project ledger and transactional provenance
    Ledger {
        #[command(subcommand)]
        command: LedgerCommands,
    },
    /// Run verification plan (predictive Bayesian testing)
    Verify {
        /// Optional specific command or step to run
        command: Option<String>,
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
        /// Show the verification plan without executing any commands
        #[arg(long)]
        dry_run: bool,
        /// Verification scope: `fast` (scoped test selection via test_mapping,
        /// for pre-push) or `full` (entire suite, for CI and manual runs).
        /// Default: `full`. The pre-push hook uses `fast`.
        #[arg(long, default_value = "full")]
        scope: crate::verify::plan::VerifyScope,
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
                LedgerCommands::Gc { .. } => "ledger_gc",
            },
            Commands::Verify { .. } => "verify",
            Commands::Ask { .. } => "ask",
            Commands::Intent { command } => match command {
                IntentCommands::Demo => "intent_demo",
            },
            Commands::Reset { .. } => "reset",
            Commands::Doctor => "doctor",
            Commands::Status => "status",
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
            #[cfg(feature = "usage-metrics")]
            Commands::Usage { command } => match command {
                UsageCommands::Enable => "usage_enable",
                UsageCommands::Disable => "usage_disable",
                UsageCommands::Status => "usage_status",
                UsageCommands::ShowPayload => "usage_show_payload",
            },
            #[cfg(feature = "mcp")]
            Commands::Mcp => "mcp",
        }
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
