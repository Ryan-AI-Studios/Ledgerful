pub mod advisories;
pub mod analysis;
pub mod ast_worker;
/// File-scope import/mod bindings for call resolution (0092).
pub mod bindings;
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
/// Module path derivation from source file paths (0092).
pub mod module_path;
pub mod normalize;
pub mod observability;
pub mod orchestrator;
pub mod references;
/// Shared call-graph callee resolution (full + incremental paths).
pub mod resolve;
pub mod routes;
pub mod rows;
pub mod runtime_usage;
/// Function/method signature extraction (not Ed25519 ledger crypto — see module docs).
pub mod signature;
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
    AutoIndexAction, ContentHashDrift, StalenessWarning, apply_content_drift_override,
    check_index_staleness, count_content_hash_drift, is_non_interactive, mark_index_stale,
    plan_auto_index_action, print_staleness_warning, try_auto_index, warn_if_stale,
};
pub use types::{ProjectFile, ProjectSymbol, symbol_to_project_symbol};

/// Re-export the shared graph-analysis driver so commands like `scan --impact`
/// can invoke the same `--analyze-graph` logic that the `index` CLI uses.
pub use orchestrator::graph::{SqliteExtractPolicy, run_graph_analysis};
