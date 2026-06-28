use super::ProjectIndexer;
use crate::state::storage::StorageManager;
use miette::Result;
use tracing::{info, warn};

/// Run the full graph analysis pipeline used by `index --analyze-graph`.
///
/// This is decoupled from the CLI path so that `scan --impact` can trigger
/// graph analysis internally when observability config files change and the
/// graph is missing or stale. It rebuilds the SQLite index, extracts all
/// enrichment phases, builds the native knowledge graph in CozoDB, computes
/// centrality, and (if requested) runs semantic enrichment via the local
/// model.
///
/// Returns the computed `CentralityStats` so callers (e.g. `index
/// --analyze-graph`) can surface entry-point/symbol/reachability counts
/// without recomputing. Returns zeroed stats if CozoDB is unavailable, so
/// callers degrade gracefully on platforms without graph storage.
pub fn run_graph_analysis(
    storage: StorageManager,
    repo_path: &std::path::Path,
    config: &crate::config::model::Config,
    enable_semantic: bool,
    fast: bool,
) -> Result<crate::index::centrality::CentralityStats> {
    let Some(cozo) = storage.cozo.as_ref() else {
        info!("CozoDB not available, skipping graph analysis");
        return Ok(crate::index::centrality::CentralityStats {
            entry_points_count: 0,
            symbols_computed: 0,
            max_reachable: 0,
        });
    };
    // Light pre-flight: if the CozoDB store is reachable but empty, we still
    // want to run the full pipeline because `observability diff` needs the
    // OpenSLO nodes loaded from the `observability/` directory. The heavy work
    // (incremental index, extraction, KG build) is shared with `index`.
    let _ = cozo.node_count();

    let repo_path = camino::Utf8PathBuf::from_path_buf(repo_path.to_path_buf())
        .map_err(|_| miette::miette!("Repository root is not valid UTF-8"))?;

    let mut indexer = ProjectIndexer::new(storage, repo_path.clone(), config.clone());

    indexer.incremental_index()?;
    indexer.index_docs()?;
    indexer.index_topology()?;
    indexer.classify_entrypoints()?;
    indexer.build_call_graph()?;
    indexer.extract_routes()?;
    indexer.extract_data_models()?;
    indexer.extract_observability()?;
    indexer.extract_test_mappings()?;
    indexer.extract_ci_gates()?;
    indexer.extract_env_schema()?;

    if config.coverage.service_inference_state()
        == crate::config::model::ServiceInferenceState::Enabled
    {
        indexer.infer_services()?;
    }

    indexer.build_kg_native(&config.local_model, &config.gemini, enable_semantic, fast)?;
    let cent_stats = indexer.compute_centrality()?;

    Ok(cent_stats)
}

pub fn build_kg_native(
    indexer: &ProjectIndexer,
    local_model_config: &crate::config::model::LocalModelConfig,
    gemini_config: &crate::config::model::GeminiConfig,
    enable_semantic: bool,
    fast: bool,
) -> Result<()> {
    let Some(cozo) = &indexer.storage.cozo else {
        info!("CozoDB not available, skipping native KG build");
        return Ok(());
    };

    let stats = crate::index::graph_loader::build_native_graph(
        &indexer.storage,
        cozo,
        "native_kg",
        &indexer.config,
    )?;

    if enable_semantic {
        match super::discovery::get_semantic_sample_files(indexer) {
            Ok(sample_files) if !sample_files.is_empty() => {
                info!(
                    "Running semantic enrichment on {} sample files via LLM...",
                    sample_files.len()
                );
                let extractor = crate::ai::semantic_extractor::SemanticExtractor::new(
                    crate::ai::semantic_extractor::SemanticExtractorConfig {
                        fast,
                        ..Default::default()
                    },
                );
                match extractor.extract_batch(sample_files, local_model_config, gemini_config) {
                    Ok(result) => {
                        info!(
                            "Semantic extraction complete: {} nodes, {} edges ({} input tokens, {} output tokens)",
                            result.nodes.len(),
                            result.edges.len(),
                            result.input_tokens,
                            result.output_tokens,
                        );
                        if let Err(e) =
                            crate::ai::semantic_extractor::SemanticExtractor::ingest_into_cozo(
                                &result,
                                cozo,
                                "semantic_kg",
                            )
                        {
                            warn!("Semantic extraction ingestion failed: {}", e);
                        }
                    }
                    Err(e) => {
                        warn!("Semantic extraction failed: {}", e);
                    }
                }
            }
            Ok(_) => {
                info!("No parsed source files found; skipping semantic enrichment.");
            }
            Err(e) => {
                warn!("Failed to collect semantic sample files: {}", e);
            }
        }
    } else {
        info!("Semantic enrichment skipped (pass --semantic to enable LLM-based extraction).");
    }

    let communities = crate::index::graph_loader::run_community_louvain(cozo)?;
    let node_count = cozo.node_count()?;
    let edge_count = cozo.edge_count()?;

    info!(
        "Native KG build complete: {} nodes, {} edges, {} communities ({} files, {} symbols)",
        node_count,
        edge_count,
        communities.len(),
        stats.files_indexed,
        stats.symbols_indexed
    );

    Ok(())
}
