use crate::config::load::load_config;
use crate::contracts::index::{ContractsIndexSummary, index_contracts};
use crate::docs::index::run_docs_index;
use crate::state::layout::Layout;
use crate::state::storage::StorageManager;
use miette::{IntoDiagnostic, Result};
use rusqlite::Connection;
use std::path::Path;
use tracing::warn;

pub(crate) fn execute_docs_index(layout: &Layout, storage: &StorageManager) -> Result<()> {
    let config = match load_config(layout) {
        Ok(c) => c,
        Err(e) => {
            warn!("Failed to load config: {:#}", e);
            println!("No doc paths configured — skipping doc index.");
            return Ok(());
        }
    };

    if config.docs.include.is_empty() {
        println!("No doc paths configured in [docs].include — skipping doc index.");
        return Ok(());
    }

    let conn = storage.get_connection();
    let summary = run_docs_index(&config, &layout.root, conn)
        .map_err(|e| miette::miette!("Docs index failed: {}", e))?;

    println!(
        "Docs indexed: {} files, {} new chunks, {} updated, {} deleted.",
        summary.files_crawled, summary.chunks_new, summary.chunks_updated, summary.chunks_deleted
    );

    Ok(())
}

pub(crate) fn execute_contracts_index(
    layout: &Layout,
    db_path: &Path,
) -> Result<ContractsIndexSummary> {
    let config = match load_config(layout) {
        Ok(c) => c,
        Err(e) => {
            warn!("Failed to load config: {:#}", e);
            println!("No contracts config — skipping contract index.");
            return Ok(Default::default());
        }
    };

    if config.contracts.spec_paths.is_empty() {
        println!("No spec paths configured in [contracts].spec_paths — skipping contract index.");
        return Ok(Default::default());
    }

    let conn = Connection::open(db_path).into_diagnostic()?;
    let summary = index_contracts(&config.contracts, &conn, &config.local_model, &layout.root)
        .map_err(|e| miette::miette!("Contract index failed: {}", e))?;

    Ok(summary)
}
