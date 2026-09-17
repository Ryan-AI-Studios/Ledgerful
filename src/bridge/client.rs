use crate::bridge::model::BridgeRecord;
use crate::state::layout::Layout;
use miette::Result;

mod client_cli;
mod status;

pub use status::{
    BRIDGE_ENABLE_HINT, IpcOverride, QueryOutcome, QuerySource, QueryStatus, QueryTransport,
    execute_query, query_status,
};

pub fn query_unified(query: &str) -> Result<Vec<BridgeRecord>> {
    let layout = crate::state::layout::get_layout_or_cwd_if_not_git()?;

    if std::env::var("LEDGERFUL_NON_INTERACTIVE").is_ok() {
        return Ok(Vec::new());
    }

    let outcome = query_status(query, &layout, &QueryTransport::production());
    Ok(outcome.insight_records())
}

pub fn is_bridge_enabled(layout: &Layout) -> bool {
    crate::config::load::load_config(layout)
        .map(|c| c.bridge.enabled)
        .unwrap_or(false)
}

pub fn is_bridge_enabled_or_default() -> bool {
    match crate::state::layout::get_layout_or_cwd_if_not_git() {
        Ok(layout) => is_bridge_enabled(&layout),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::layout::Layout;
    use camino::Utf8Path;
    use tempfile::tempdir;

    #[serial_test::serial(cwd)]
    #[test]
    fn query_unified_disabled_returns_empty() {
        let tmp = tempdir().unwrap();
        let root = Utf8Path::from_path(tmp.path()).unwrap();
        let layout = Layout::new(root);
        layout.ensure_state_dir().unwrap();

        let config_path = layout.config_file();
        std::fs::write(config_path, "[bridge]\nenabled = false\n").unwrap();

        let _guard = crate::tests::DirGuard::new(tmp.path());
        let result = query_unified("test").unwrap();
        assert!(result.is_empty());
    }
}
