use crate::index::ProjectIndexer;
use crate::state::layout::Layout;
use miette::Result;
use tracing::warn;

/// Export-docs mode: write knowledge-graph data to passive documentation.
pub(super) fn execute_export_docs_mode(
    indexer: &mut ProjectIndexer,
    layout: &Layout,
    doc_type_filter: Option<&str>,
) -> Result<()> {
    if let Some(cozo) = indexer.cozo() {
        match cozo.node_count() {
            Ok(0) => {
                println!("Warning: Knowledge Graph is empty, skipping doc export.");
            }
            Ok(_) => {
                let docs_dir = layout.docs_dir();
                layout.ensure_dir(&docs_dir)?;
                let registry = crate::docs::generator::DocRegistry::default_registry();
                let doc_result = if let Some(dt) = doc_type_filter {
                    let types: Vec<String> = dt.split(',').map(|s| s.trim().to_string()).collect();
                    registry.run_filtered(&types, cozo, &docs_dir)
                } else {
                    registry.run_all(cozo, &docs_dir)
                };
                match doc_result {
                    Ok(paths) => {
                        for path in &paths {
                            println!("Doc: {}", path);
                        }
                    }
                    Err(e) => {
                        warn!("Doc generation failed: {:#}", e);
                    }
                }
            }
            Err(e) => {
                warn!("Failed to query node count: {:#}", e);
                println!("Warning: Knowledge Graph unavailable, skipping doc export.");
            }
        }
    } else {
        println!("Warning: Knowledge Graph unavailable, skipping doc export.");
    }
    Ok(())
}
