use clap::{Args, Subcommand, ValueEnum};

/// Extra path classes to include in the CLI hotspot list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum HotspotIncludeScope {
    /// Tests, examples, and benches (audit view; default CLI list omits these)
    Tests,
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

    /// Include test, example, and bench paths (default CLI list omits them)
    #[arg(long, value_enum, value_name = "SCOPE")]
    pub include: Option<HotspotIncludeScope>,
}

#[derive(Subcommand, Debug)]
pub enum HotspotSubcommands {
    /// Top-file hotspot trend summary (default); full matrix via --all
    Trend {
        /// Entity path to filter by (one-file series; ignores --limit / --all)
        #[arg(short, long)]
        entity: Option<String>,
        /// Number of days to look back
        #[arg(short, long, default_value_t = 30)]
        days: u32,
        /// Max files in summary mode (default 20; ignored with --all / --entity).
        /// Parent `hotspots --limit` is list-scoped and does not set this.
        #[arg(
            long,
            default_value_t = 20,
            value_parser = clap::value_parser!(u64).range(1..)
        )]
        limit: u64,
        /// Full timestamp×file matrix for the days window (ignores --limit)
        #[arg(short = 'a', long)]
        all: bool,
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

/// Inventory of advanced surfaces (ready / empty / gated).
#[derive(Args, Debug)]
pub struct SurfacesArgs {
    /// Pure machine JSON on stdout (schemaVersion 1)
    #[arg(long)]
    pub json: bool,
}

/// Detect likely dead code across the repository.
#[derive(Args, Debug)]
pub struct DeadCodeArgs {
    /// Minimum confidence threshold to report a finding
    #[arg(long, default_value_t = 0.75)]
    pub threshold: f64,
    /// Maximum number of findings to display
    #[arg(long, default_value_t = 50)]
    pub limit: usize,
    /// Automatically run incremental index before detection if the index is stale
    #[arg(long)]
    pub auto_index: bool,
    /// Include standard trait implementations (Eq, Ord, Clone, Debug, etc.) in
    /// results. By default these are suppressed because they are typically used
    /// implicitly via derive macros or blanket impls.
    #[arg(long)]
    pub include_traits: bool,
    /// Interactively prompt to remove high-confidence dead code and record the
    /// deletions as a pending ledger transaction.
    #[arg(long)]
    pub prune: bool,
    /// Show full per-symbol table instead of grouped-by-file view
    #[arg(long)]
    pub expand: bool,
    /// Explain why a specific file is flagged as dead code (per-symbol breakdown)
    #[arg(long)]
    pub explain: Option<String>,
    /// Output as JSON (schemaVersion 1 envelope; rejects --prune / --explain)
    #[arg(long)]
    pub json: bool,
}

/// Generate an interactive visualization of the knowledge graph.
#[derive(Args, Debug)]
pub struct VizArgs {
    /// Custom output path for the HTML file
    #[arg(long, short, alias = "out")]
    pub output: Option<String>,
    /// Maximum number of nodes to include
    #[arg(long, short, default_value_t = 1000)]
    pub limit: usize,
    /// Maximum depth for relationship traversal
    #[arg(long, short, default_value_t = 2)]
    pub depth: usize,
    /// Filter by specific entity (root of the graph)
    #[arg(long, short)]
    pub entity: Option<String>,
    /// Visualization view: "graph" (default) or "services" (K4 service connectivity)
    #[arg(long, default_value = "graph")]
    pub view: String,
}
