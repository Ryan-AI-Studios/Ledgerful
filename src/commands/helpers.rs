use crate::config::load::load_config;
use crate::config::model::Config;
use crate::state::layout::Layout;
use camino::Utf8PathBuf;
use miette::{IntoDiagnostic, Result};
use std::env;

pub fn get_repo_root() -> Result<Utf8PathBuf> {
    let current_dir = env::current_dir().into_diagnostic()?;
    let discovered = gix::discover(&current_dir).into_diagnostic()?;
    let root = discovered
        .workdir()
        .ok_or_else(|| miette::miette!("Failed to find work directory for repository"))?;

    Utf8PathBuf::from_path_buf(root.to_path_buf())
        .map_err(|_| miette::miette!("Repository root is not valid UTF-8"))
}

pub fn get_layout() -> Result<Layout> {
    let root = get_repo_root()?;
    Ok(Layout::new(root))
}

pub fn load_ledger_config(layout: &Layout) -> Result<Config> {
    load_config(layout)
}
