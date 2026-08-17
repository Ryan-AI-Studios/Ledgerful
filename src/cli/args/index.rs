use clap::Args;

/// Index the project for search and discovery.
#[derive(Args, Debug)]
pub struct IndexArgs {
    /// Perform incremental index (only changed files)
    #[arg(long, short)]
    pub incremental: bool,
    /// Force a full re-index
    #[arg(long, short)]
    pub full: bool,
    /// Refresh the knowledge graph (analyze structure)
    #[arg(long)]
    pub analyze_graph: bool,
    /// Index documentation files
    #[arg(long)]
    pub docs: bool,
    /// Index API contract files (OpenAPI/Swagger)
    #[arg(long)]
    pub contracts: bool,
    /// Index code snippets for semantic search (local embeddings)
    #[arg(long)]
    pub semantic: bool,
    /// Ingest an external SCIP index (Protobuf)
    #[arg(long)]
    pub scip: Option<std::path::PathBuf>,
    /// Automatically detect, generate, and ingest SCIP indices
    #[arg(long)]
    pub auto_scip: bool,
    /// Export knowledge graph data to passive documentation
    #[arg(long)]
    pub export_docs: bool,
    /// Filter exported documentation by type (e.g. mermaid, markdown)
    #[arg(long)]
    pub doc_type: Option<String>,
    /// Check index freshness
    #[arg(long)]
    pub check: bool,
    /// Output as JSON
    #[arg(long)]
    pub json: bool,
    /// Strict mode for check (exit 1 if stale)
    #[arg(long)]
    pub strict: bool,
    /// Number of parallel threads for semantic indexing (default: logical CPUs)
    #[arg(long, short = 'j')]
    pub concurrency: Option<usize>,
    /// Print resolved semantic settings and exit. Optionally takes a path for JSON output.
    #[arg(long, value_name = "OUTPUT_PATH", num_args = 0..=1)]
    pub semantic_dry_run: Option<Option<std::path::PathBuf>>,
    /// Use Gemini for semantic extraction (fast, large context) instead of local model
    #[arg(long)]
    pub fast: bool,
    /// Repair corrupt or missing completion metadata. Rebuilds index safely.
    #[arg(long)]
    pub repair_metadata: bool,
    /// Dry run for repair-metadata (shows proposed changes without writing)
    #[arg(long)]
    pub dry_run: bool,
    /// Automatically confirm repair operations (non-interactive)
    #[arg(long)]
    pub yes: bool,
}

/// Watch repository for changes and run incremental graph sync.
#[derive(Args, Debug)]
pub struct WatchArgs {
    /// Throttle interval in milliseconds for debouncing file events.
    /// Defaults to `watch.debounce_ms` from config when not specified.
    #[arg(long, short, default_value_t = 0)]
    pub interval: u64,
    /// Output watch events as JSON
    #[arg(long, short)]
    pub json: bool,
    /// Disable Knowledge Graph sync during watch
    #[arg(long = "no-graph-sync")]
    pub no_graph_sync: bool,
}
