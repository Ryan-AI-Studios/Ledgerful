use super::ProjectIndexer;
use crate::state::layout::Layout;
use crate::state::storage::StorageManager;
use miette::Result;
use std::path::PathBuf;
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
/// Returns the computed `CentralityStats` and optional SCIP augment result
/// so callers (e.g. `index --analyze-graph`) can surface counts without
/// recomputing. Returns zeroed stats if CozoDB is unavailable, so callers
/// degrade gracefully on platforms without graph storage.
///
/// SCIP: when `auto_scip` / `scip_path` is set, runs edge augment **only**
/// here after `infer_services` and **before** `build_kg_native` +
/// `compute_centrality` (0095 §2.2b — exclusive with the main-mode site;
/// `execute_main_mode` skips SCIP when `--analyze-graph` is set).
#[allow(clippy::too_many_arguments)]
pub fn run_graph_analysis(
    storage: StorageManager,
    repo_path: &std::path::Path,
    config: &crate::config::model::Config,
    enable_semantic: bool,
    fast: bool,
    auto_scip: bool,
    scip_path: Option<PathBuf>,
    layout: Option<&Layout>,
) -> Result<(
    crate::index::centrality::CentralityStats,
    Option<crate::scip::ScipIndexJson>,
)> {
    let Some(cozo) = storage.cozo.as_ref() else {
        info!("CozoDB not available, skipping graph analysis");
        return Ok((
            crate::index::centrality::CentralityStats {
                entry_points_count: 0,
                symbols_computed: 0,
                max_reachable: 0,
            },
            None,
        ));
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

    // SCIP augment after services, before KG + centrality (0095 §2.2b)
    let scip_json = if auto_scip || scip_path.is_some() {
        let owned_layout;
        let layout_ref = if let Some(l) = layout {
            l
        } else {
            owned_layout = Layout::new(&repo_path);
            &owned_layout
        };
        Some(crate::scip::maybe_run_scip_augment(
            layout_ref,
            indexer.storage_mut(),
            config,
            auto_scip,
            scip_path,
        ))
    } else {
        None
    };

    indexer.build_kg_native(&config.local_model, &config.gemini, enable_semantic, fast)?;
    let cent_stats = indexer.compute_centrality()?;

    Ok((cent_stats, scip_json))
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
                        if !result.parse_warnings.is_empty() {
                            warn!(
                                "Semantic extraction produced {} parse/validation warning(s): {:?}",
                                result.parse_warnings.len(),
                                result.parse_warnings
                            );
                        }
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
