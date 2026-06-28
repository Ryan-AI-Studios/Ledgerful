use crate::state::layout::Layout;
use camino::Utf8PathBuf;

pub const DEFAULT_LOCK_NAME: &str = "ledgerful.lock";

pub fn default_lock_path(layout: &Layout) -> Utf8PathBuf {
    layout.state_subdir().join(DEFAULT_LOCK_NAME)
}
