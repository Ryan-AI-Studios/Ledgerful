use camino::Utf8PathBuf;
use miette::{IntoDiagnostic, Result};
use std::env;

pub use crate::commands::index::check::execute_index_check;
pub use crate::commands::index::modes::execute_index;

pub(crate) mod check;
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

pub(crate) fn get_repo_root() -> Result<Utf8PathBuf> {
    let current_dir = env::current_dir().into_diagnostic()?;
    let discovered = gix::discover(&current_dir).into_diagnostic()?;
    let root = discovered
        .workdir()
        .ok_or_else(|| miette::miette!("Failed to find work directory for repository"))?;

    Utf8PathBuf::from_path_buf(root.to_path_buf())
        .map_err(|_| miette::miette!("Repository root is not valid UTF-8"))
}

pub(crate) fn get_layout() -> Result<crate::state::layout::Layout> {
    let root = get_repo_root()?;
    Ok(crate::state::layout::Layout::new(root))
}
