use crate::config::load::load_config;
use crate::config::model::Config;
use crate::state::layout::Layout;
use miette::Result;

pub use crate::state::layout::{
    LEDGERFUL_STATE_DIR_ENV, get_layout, get_layout_or_cwd_if_not_git, get_repo_root,
    resolve_state_dir, resolve_state_dir_override,
};

pub fn load_ledger_config(layout: &Layout) -> Result<Config> {
    load_config(layout)
}
