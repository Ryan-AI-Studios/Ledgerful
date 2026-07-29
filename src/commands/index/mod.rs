pub use crate::commands::index::modes::execute_index;

pub(crate) mod graph;
pub(crate) mod modes;
pub(crate) mod output;
pub(crate) mod repair;
pub(crate) mod scip;
pub(crate) mod semantic;

/// CLI arguments for the `ledgerful index` command.
#[derive(Default)]
pub struct IndexArgs {
    pub incremental: bool,
    pub check: bool,
    pub strict: bool,
    pub json: bool,
    pub analyze_graph: bool,
    pub docs: bool,
    pub contracts: bool,
    pub semantic: bool,
    pub scip: Option<std::path::PathBuf>,
    pub auto_scip: bool,
    pub export_docs: bool,
    pub doc_type: Option<String>,
    /// CLI override for rayon thread count (HP2). `None` = use config or rayon default.
    pub concurrency: Option<usize>,
    /// Print resolved semantic settings and exit. Optionally takes a path for JSON output.
    pub semantic_dry_run: Option<Option<std::path::PathBuf>>,
    /// Use Gemini for semantic extraction (fast, large context) instead of local model
    pub fast: bool,
    pub repair_metadata: bool,
    pub dry_run: bool,
    pub yes: bool,
}
