use crate::index::rows::{get_file_id_by_path, insert_symbol_row};
use crate::scip::{
    ScipIndex, ScipSymbolMapper, is_scip_stale, normalize_scip_path, register_scip_index,
};
use crate::state::layout::Layout;
use crate::state::storage::StorageManager;
use miette::{IntoDiagnostic, Result};
use std::collections::HashMap;
use std::path::PathBuf;
use tracing::{info, warn};

pub(crate) fn execute_scip_index(
    layout: &Layout,
    storage: &mut StorageManager,
    scip_path: PathBuf,
) -> Result<()> {
    info!("Ingesting SCIP index from {:?}", scip_path);
    let scip_index = ScipIndex::load(&scip_path)?;

    let conn = storage.get_connection();
    if !is_scip_stale(conn, &scip_path, &scip_index.file_hash)? {
        info!("SCIP index is up to date, skipping ingestion.");
        return Ok(());
    }

    let conn_mut = storage.get_connection_mut();
    let tx = conn_mut.unchecked_transaction().into_diagnostic()?;

    let mut symbols_ingested = 0usize;
    let mut files_skipped = 0usize;

    for document in &scip_index.index.documents {
        let relative_path =
            match normalize_scip_path(layout.root.as_std_path(), &document.relative_path) {
                Ok(p) => p.to_string_lossy().to_string(),
                Err(e) => {
                    warn!(
                        "Failed to normalize SCIP path {}: {}",
                        document.relative_path, e
                    );
                    continue;
                }
            };

        let file_id = match get_file_id_by_path(&tx, &relative_path) {
            Ok(id) => id,
            Err(_) => {
                files_skipped += 1;
                continue;
            }
        };

        let symbol_info_map: HashMap<_, _> = scip_index
            .index
            .external_symbols
            .iter()
            .chain(scip_index.index.documents.iter().flat_map(|d| &d.symbols))
            .map(|s| (&s.symbol, s))
            .collect();

        for occurrence in &document.occurrences {
            if occurrence.symbol.is_empty() || occurrence.symbol.starts_with("local ") {
                continue;
            }

            if let Some(symbol_info) = symbol_info_map.get(&occurrence.symbol) {
                let project_symbol =
                    ScipSymbolMapper::map_to_project_symbol(file_id, symbol_info, occurrence);
                insert_symbol_row(&tx, &project_symbol, file_id)?;
                symbols_ingested += 1;
            }
        }
    }

    register_scip_index(&tx, &scip_path, &scip_index.file_hash)?;
    tx.commit().into_diagnostic()?;

    info!(
        "SCIP ingestion complete: {} symbols ingested, {} files skipped (not in project index).",
        symbols_ingested, files_skipped
    );

    Ok(())
}
