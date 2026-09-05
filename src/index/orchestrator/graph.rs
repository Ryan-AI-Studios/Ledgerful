use super::ProjectIndexer;
use crate::state::layout::Layout;
use miette::Result;
use std::path::PathBuf;
use tracing::{info, warn};

/// Whether `run_graph_analysis` should re-run SQLite extract stages.
///
/// `Run` (default) is fail-safe for standalone callers such as
/// `scan --impact`. `AlreadyRan` skips `incremental_index` through
/// `infer_services` when `execute_main_mode` already extracted.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SqliteExtractPolicy {
    AlreadyRan,
    #[default]
    Run,
}

/// Run the graph analysis pipeline used by `index --analyze-graph`.
///
/// This is decoupled from the CLI path so that `scan --impact` can trigger
/// graph analysis internally when observability config files change and the
/// graph is missing or stale. When `policy` is [`SqliteExtractPolicy::Run`],
/// it incrementally extracts SQLite enrichment phases, then builds the native
/// knowledge graph in CozoDB, computes centrality, and (if requested) runs
/// semantic enrichment. When `policy` is [`SqliteExtractPolicy::AlreadyRan`],
/// extract is skipped (caller already ran it); SCIP / KG / centrality still
/// run.
///
/// Returns the computed `CentralityStats` and optional SCIP augment result
/// so callers (e.g. `index --analyze-graph`) can surface counts without
/// recomputing. Returns zeroed stats if CozoDB is unavailable, so callers
/// degrade gracefully on platforms without graph storage.
///
/// SCIP: when `auto_scip` / `scip_path` is set, runs edge augment **only**
/// here after extract-or-skip and **before** `build_kg_native` +
/// `compute_centrality` (0095 §2.2b — exclusive with the main-mode site;
/// one augment site per invocation so SCIP is not fed into `infer_services`
/// on the graph path).
pub fn run_graph_analysis(
    indexer: &mut ProjectIndexer,
    policy: SqliteExtractPolicy,
    enable_semantic: bool,
    fast: bool,
    auto_scip: bool,
    scip_path: Option<PathBuf>,
    layout: Option<&Layout>,
) -> Result<(
    crate::index::centrality::CentralityStats,
    Option<crate::scip::ScipIndexJson>,
)> {
    let scip_requested = auto_scip || scip_path.is_some();
    if indexer.cozo().is_none() {
        info!("CozoDB not available, skipping graph analysis (KG/centrality)");
        // SCIP edges live in SQLite only. When main mode deferred SCIP to this
        // path under --analyze-graph, still apply edges against the native floor
        // already present on the indexer.
        let scip_status = if scip_requested {
            let owned_layout;
            let layout_ref = if let Some(l) = layout {
                l
            } else {
                owned_layout = Layout::new(&indexer.repo_path);
                &owned_layout
            };
            let config = indexer.config.clone();
            Some(crate::scip::maybe_run_scip_augment(
                layout_ref,
                indexer.storage_mut(),
                &config,
                auto_scip,
                scip_path,
            ))
        } else {
            None
        };
        return Ok((
            crate::index::centrality::CentralityStats {
                entry_points_count: 0,
                symbols_computed: 0,
                max_reachable: 0,
            },
            scip_status,
        ));
    }
    // Light pre-flight: if the CozoDB store is reachable but empty, we still
    // want to run the pipeline because `observability diff` needs the
    // OpenSLO nodes loaded from the `observability/` directory.
    if let Some(cozo) = indexer.cozo() {
        let _ = cozo.node_count();
    }

    if policy == SqliteExtractPolicy::Run {
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

        if indexer.config.coverage.service_inference_state()
            == crate::config::model::ServiceInferenceState::Enabled
        {
            indexer.infer_services()?;
        }
        indexer.clear_content_cache();
    }

    // SCIP augment after extract-or-skip, before KG + centrality (0095 §2.2b)
    let scip_json = if auto_scip || scip_path.is_some() {
        let owned_layout;
        let layout_ref = if let Some(l) = layout {
            l
        } else {
            owned_layout = Layout::new(&indexer.repo_path);
            &owned_layout
        };
        let config = indexer.config.clone();
        Some(crate::scip::maybe_run_scip_augment(
            layout_ref,
            indexer.storage_mut(),
            &config,
            auto_scip,
            scip_path,
        ))
    } else {
        None
    };

    indexer.build_kg_native(
        &indexer.config.local_model,
        &indexer.config.gemini,
        enable_semantic,
        fast,
    )?;
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
    let Some(cozo) = indexer.cozo() else {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::model::Config;
    use crate::scip::ScipRunStatus;
    use crate::state::layout::Layout;
    use crate::state::storage::connection::in_memory_storage;

    /// 0095 P3 / 0105 roll-in: when Cozo is unavailable but SCIP is requested,
    /// still attempt SQLite-side SCIP augment and return zeroed centrality +
    /// an explicit scip status (not silent skip of SCIP).
    #[test]
    fn cozo_unavailable_still_attempts_scip_when_requested() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root =
            camino::Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).expect("utf8 temp path");
        let layout = Layout::new(&root);
        let storage = in_memory_storage();
        assert!(
            storage.cozo().is_none(),
            "in_memory_storage must have cozo=None for this branch"
        );
        let config = Config::default();
        let missing = tmp.path().join("definitely-missing-0105.scip");
        let mut indexer = ProjectIndexer::new(storage, root, config);

        let (cent, scip) = run_graph_analysis(
            &mut indexer,
            SqliteExtractPolicy::Run,
            false,
            true,
            false,
            Some(missing),
            Some(&layout),
        )
        .expect("Cozo-missing path must not hard-fail");

        assert_eq!(cent.entry_points_count, 0);
        assert_eq!(cent.symbols_computed, 0);
        assert_eq!(cent.max_reachable, 0);

        let scip = scip.expect("SCIP requested → Some(ScipIndexJson)");
        assert!(
            matches!(scip.status, ScipRunStatus::Failed | ScipRunStatus::Success),
            "requested SCIP must not be DidNotRun when Cozo is missing; got {:?}",
            scip.status
        );
        // Missing path should fail (not invent success).
        assert!(
            matches!(scip.status, ScipRunStatus::Failed),
            "missing SCIP file should report failed; got {:?} msg={:?}",
            scip.status,
            scip.message
        );
    }

    #[test]
    fn cozo_unavailable_without_scip_returns_none_scip() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root =
            camino::Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).expect("utf8 temp path");
        let layout = Layout::new(&root);
        let storage = in_memory_storage();
        let config = Config::default();
        let mut indexer = ProjectIndexer::new(storage, root, config);

        let (cent, scip) = run_graph_analysis(
            &mut indexer,
            SqliteExtractPolicy::Run,
            false,
            true,
            false,
            None,
            Some(&layout),
        )
        .expect("ok");

        assert_eq!(cent.max_reachable, 0);
        assert!(scip.is_none());
    }
}
