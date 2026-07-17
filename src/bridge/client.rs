use crate::bridge::ipc::IpcClient;
use crate::bridge::model::{BridgeDirection, BridgePayload, BridgeRecord};
use crate::state::layout::Layout;
use miette::{IntoDiagnostic, Result};
use std::time::Duration;

mod client_cli;
use crate::util::query::sanitize_fts5_query;
pub use client_cli::query_external_cli;

pub fn query_unified(query: &str) -> Result<Vec<BridgeRecord>> {
    let current_dir = std::env::current_dir().into_diagnostic()?;
    let layout = Layout::new(current_dir.to_string_lossy().as_ref());
    let project_id = layout.get_project_id();

    if std::env::var("LEDGERFUL_NON_INTERACTIVE").is_ok() {
        return Ok(Vec::new());
    }

    if !is_bridge_enabled(&layout) {
        return Ok(Vec::new());
    }

    let sanitized_query = sanitize_fts5_query(query);

    // 1. Try IPC
    if let Ok(mut client) = IpcClient::connect_with_timeout(Duration::from_millis(200)) {
        let payload = BridgePayload::Query {
            text: sanitized_query.clone(),
        };
        let req = BridgeRecord::new(
            BridgeDirection::Inbound,
            project_id.clone(),
            "query",
            payload,
        );
        if client.send_record(&req).is_ok()
            && let Ok(records) = client.receive_records()
            && !records.is_empty()
        {
            return Ok(records);
        }
    }

    // 2. Fallback to CLI
    query_external_cli(&sanitized_query)
}

pub fn is_bridge_enabled(layout: &Layout) -> bool {
    crate::config::load::load_config(layout)
        .map(|c| c.bridge.enabled)
        .unwrap_or(false)
}

pub fn is_bridge_enabled_or_default() -> bool {
    let current_dir = std::env::current_dir()
        .map(|d| d.to_string_lossy().to_string())
        .unwrap_or_default();
    let layout = Layout::new(&current_dir);
    is_bridge_enabled(&layout)
}

const BRIDGE_ENABLE_HINT: &str =
    "Bridge is disabled. Enable with `bridge.enabled = true` in config or set LEDGERFUL_BRIDGE=1.";

pub fn execute_query(query: String) -> Result<()> {
    let current_dir = std::env::current_dir().into_diagnostic()?;
    let layout = Layout::new(current_dir.to_string_lossy().as_ref());
    if !is_bridge_enabled(&layout) {
        eprintln!("{}", BRIDGE_ENABLE_HINT);
        return Ok(());
    }

    eprintln!("Querying external context provider (IPC → CLI fallback)...");
    let records = query_unified(&query)?;
    if records.is_empty() {
        println!(
            "No memories recalled from external provider for {:?}.",
            query
        );
        println!(
            "If a provider is installed, run the provider's sync/daemon command \
             (e.g. `provider sync query {:?}` or `provider daemon start` to enable IPC).",
            query
        );
    } else {
        println!(
            "Recalled {} memories from external provider:",
            records.len()
        );
        for record in records {
            if let BridgePayload::Insight {
                content, relevance, ..
            } = record.payload
            {
                println!("- [{:.2}] {}", relevance, content);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::layout::Layout;
    use camino::Utf8Path;
    use tempfile::tempdir;

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
