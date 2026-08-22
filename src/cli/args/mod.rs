use crate::commands::bridge::BridgeCommands;
use clap::{Parser, Subcommand};

mod agent;
mod coverage;
mod index;
mod inventory;
mod ledger;
mod lifecycle;
mod meta;
mod ops;

pub use agent::*;
pub use coverage::*;
pub use index::*;
pub use inventory::*;
pub use ledger::*;
pub use lifecycle::*;
pub use ops::*;

#[derive(Parser, Debug)]
#[command(
    about = "Ledgerful change intelligence and transactional provenance for software engineering",
    long_about = None,
    before_help = "Agent default path (Daily 5): doctor --json · change-context --json · ledger status · search · verify --scope fast — see skill Daily 5."
)]
// Short `-V` = package version only; long `--version` may include build SHA (0137).
#[command(version, long_version = env!("LEDGERFUL_VERSION_LONG"))]
#[command(disable_help_subcommand = true)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,

    /// Enable verbose logging output
    #[arg(long, short, global = true)]
    pub verbose: bool,

    /// Quiet mode: hide per-entry `cli_summary` detail, keep the aggregate summary.
    /// Also selected by `LEDGERFUL_QUIET=1`. Does **not** select machine mode;
    /// use `--json` (or `mcp` / `scan --format json`) for agent-safe stdout.
    #[arg(long, short = 'q', global = true)]
    pub quiet: bool,

    /// Run as if started in PATH (git-shaped; last `-C` wins; empty leaves cwd).
    /// Does not set `LEDGERFUL_STATE_DIR`.
    #[arg(
        short = 'C',
        long = "directory",
        global = true,
        value_name = "PATH",
        overrides_with = "directory"
    )]
    pub directory: Option<String>,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Initialize Ledgerful in the current repository
    Init(InitArgs),

    /// Gate mode configuration
    #[command(after_help = "Default when omitted: mode (show current; does not set).")]
    Gate {
        #[command(subcommand)]
        command: Option<GateCommands>,
    },
    /// Evaluate declared repository policy (CI merge gate)
    #[command(after_help = "Default when omitted: check.")]
    Policy {
        #[command(subcommand)]
        command: Option<PolicyCommands>,
    },
    /// Guided onboarding wizard (welcome → init → doctor → first scan → success)
    Setup(SetupArgs),
    /// Scan git changes and identify affected symbols
    Scan(ScanArgs),
    /// Analyze impact of current changes
    Impact(ImpactArgs),
    /// Budgeted agent change packet (impact + doctor + ledger + readSet)
    ChangeContext(ChangeContextArgs),
    /// Index the project for search and discovery
    Index(IndexArgs),
    /// Search the codebase using high-performance regex or semantic search
    ///
    /// Unquoted multi-word queries are accepted. Flags may appear before or after
    /// query words. Use `--` for hyphen-leading tokens.
    #[command(
        long_about = "Search the codebase using high-performance regex or semantic search.\n\n\
Unquoted multi-word queries are accepted (e.g. `search foo bar` is the same as\n\
`search \"foo bar\"`). Flags such as `--json` and `--limit` may appear before or\n\
after query words. Hyphen-leading tokens need `--` so clap does not parse them\n\
as flags. Shell quotes do not hide a leading hyphen from clap.",
        after_help = "\
Tips:
  ledgerful search foo bar
      Unquoted multi-word OK; same as search \"foo bar\"
  ledgerful search --json foo bar
  ledgerful search foo bar --json
      Flags before or after query words
  ledgerful search -- --json
      Hyphen-leading token; without `--` clap parses flags
      (quotes are stripped by the shell — `search \"--json\"` is still the flag)
"
    )]
    Search(SearchCliArgs),
    /// Rank files by change frequency and complexity (Hotspots)
    Hotspots {
        #[command(flatten)]
        args: HotspotArgs,
    },
    /// List and filter API endpoints
    Endpoints(crate::commands::endpoints::EndpointsArgs),
    /// List indexed symbols (scoped path/changed/kind/pub inventory; not search)
    #[command(
        long_about = "List indexed symbols from project_symbols (scoped inventory).\n\n\
This is a bounded catalog of definitions under a path or change set — not BM25/semantic search.\n\n\
--path is a path *prefix* (file equals prefix or lives under prefix/), not a substring like endpoints --path.\n\
Class and Interface kinds are accepted but currently unpopulated by extractors (reserved).\n\
--changed includes Deleted paths still present in the index until re-index."
    )]
    Symbols(crate::commands::symbols::SymbolsArgs),
    /// Inventory of advanced surfaces (ready / empty / gated)
    #[command(
        visible_alias = "tour",
        long_about = "Read-only inventory of six advanced surfaces: services, deploy, \
security, observability, config schema, and data-models.\n\n\
Each row is ready, empty, or gated by coverage. Does not enable coverage \
or add content. Alias: tour."
    )]
    Surfaces(SurfacesArgs),
    /// Manage cross-repo federation
    #[command(
        after_help = "Default when omitted: status (read-only; export/scan require explicit subcommand)."
    )]
    Federate {
        #[command(subcommand)]
        command: Option<FederateCommands>,
    },
    /// Service boundary and topology commands
    #[command(after_help = "Default when omitted: diff.")]
    Services {
        #[command(subcommand)]
        command: Option<ServiceSubcommands>,
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
    Verify(VerifyArgs),
    /// Ask Gemini or a local model for assistance based on the current context
    ///
    /// Unquoted multi-word queries are accepted. Flags (e.g. `--semantic`,
    /// `--backend`) must precede unquoted query words; post-query flags are
    /// treated as query text.
    #[command(
        long_about = "Ask Gemini or a local model for assistance based on the current context.\n\n\
Unquoted multi-word queries are accepted (e.g. `ask what is change-context`).\n\
Flags such as `--semantic` and `--backend` must precede unquoted query words;\n\
anything after the first non-flag word is treated as query text (including\n\
flag-like tokens). Prefer: `ask --semantic what is X`.",
        after_help = "\
Tips:
  ledgerful ask what is change-context
      Unquoted multi-word OK; put flags before query words
  ledgerful ask --semantic what is X
      Flags first — semantic=true, query = \"what is X\"
  ledgerful ask what is X --semantic
      Flags after words are query text (semantic stays false)
"
    )]
    Ask(AskArgs),
    /// Manage Ledgerful intent capture and TUI interaction
    Intent {
        #[command(subcommand)]
        command: IntentCommands,
    },
    /// Reset Ledgerful state or configuration
    Reset(ResetArgs),
    /// Health check for Ledgerful and local model stack
    Doctor(DoctorArgs),
    /// Ledger pending/drift status (`--json` / `--compact`; not a full alias of `ledger status`)
    Status(StatusArgs),
    /// Configuration management
    // after_help only on Config (0100 DoD-8): clap auto-help is insufficient for
    // key=value set examples; do not spray after_help on every command group.
    Config {
        #[command(subcommand)]
        command: ConfigCommands,
    },
    /// Detect likely dead code across the repository
    DeadCode(DeadCodeArgs),
    /// Perform a holistic project audit or history for an entity
    Audit(AuditArgs),
    /// Local-only per-command timing analysis (Track 0043; `--global` is Track 0044)
    Timings(TimingsCliArgs),
    /// Generate an interactive visualization of the knowledge graph
    Viz(VizArgs),
    /// Update the Ledgerful binary or migrate repository state
    #[command(alias = "upgrade")]
    Update(UpdateArgs),
    /// Watch repository for changes and run incremental graph sync
    Watch(WatchArgs),
    /// Team ledger synchronization [Available — opt-in shared-folder v1]
    ///
    /// Opt-in forever (`[sync].enabled = false` by default). Pairing, secure
    /// transport/apply, setup checklist, and status next-action are real.
    /// Not default-on, not cloud SaaS, not CRDT. See docs/team-sync.md.
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
    SearchTrigrams(SearchTrigramsArgs),
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
    /// Run the MCP server (stdio) or install/uninstall host platform config
    #[cfg(feature = "mcp")]
    Mcp {
        #[command(subcommand)]
        command: Option<McpCommands>,
    },
    /// Print the canonical OpenAPI JSON spec for this build to stdout
    #[cfg(any(feature = "openapi", feature = "web"))]
    Openapi,

    /// Generate a disposable demonstration repo with signed ledger entries, cryptographic VALID proof, and a DEMO evidence export (see docs/golden-path.md)
    Demo(DemoArgs),
}
