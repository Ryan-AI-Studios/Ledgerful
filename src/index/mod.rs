pub mod advisories;
pub mod analysis;
pub mod ast_worker;
pub mod call_graph;
pub mod centrality;
pub mod ci_gates;
pub mod data_models;
pub mod docs;
pub mod entrypoint;
pub mod env_patterns;
pub mod env_schema;
pub mod git_worker;
pub mod graph_loader;
pub mod graph_worker;
pub mod incremental;
pub mod languages;
pub mod metrics;
pub mod migrations;
pub mod normalize;
pub mod observability;
pub mod orchestrator;
pub mod references;
pub mod routes;
pub mod rows;
pub mod runtime_usage;
pub mod staleness;
pub mod storage;
pub mod symbols;
pub mod test_mapping;
pub mod topology;
pub mod types;
pub mod walker;
pub mod worker_pool;

pub use orchestrator::{
    BATCH_SIZE, BINARY_EXTENSIONS, MAX_FILES, PARSER_VERSION, SUPPORTED_EXTENSIONS,
};
pub use orchestrator::{IndexStats, IndexStatus, ProjectIndexer, ServiceIndexStats};
pub use staleness::{
    StalenessWarning, check_index_staleness, is_non_interactive, print_staleness_warning,
    warn_if_stale,
};
pub use types::{ProjectFile, ProjectSymbol, symbol_to_project_symbol};

/// Re-export the shared graph-analysis driver so commands like `scan --impact`
/// can invoke the same `--analyze-graph` logic that the `index` CLI uses.
pub use orchestrator::graph::run_graph_analysis;
