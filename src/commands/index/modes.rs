use super::graph::{execute_contracts_index, execute_docs_index};
use super::output::{IndexOutputStats, print_human_output, print_json_output};
use super::repair::execute_repair_metadata;
use super::scip::execute_scip_index;
use super::semantic::{execute_semantic_dry_run, execute_semantic_index};
use super::{IndexArgs, get_layout};
use crate::config::load::load_config;
use crate::index::staleness::{EmptyIndexReason, IndexFreshnessState};
use crate::index::{ProjectIndexer, ServiceIndexStats};
use crate::state::layout::Layout;
use crate::state::storage::StorageManager;
use camino::Utf8PathBuf;
use miette::{IntoDiagnostic, Result};
use tracing::{info, warn};

/// Mode-combination matrix for `ledgerful index`.
///
/// Precedence (early-return order) is critical and must be preserved:
/// 1. `--semantic-dry-run`  → preempts everything (returns immediately).
/// 2. `--auto-scip`         → automatically generate and ingest SCIP.
/// 3. `--scip <PATH>`       → early-returns next.
/// 4. `--semantic` (without `--analyze-graph`) → early-returns.
///    `--semantic --analyze-graph` falls through to the main path,
///    where semantic enrichment is applied inside graph analysis.
/// 5. `--docs` (without `--analyze-graph`) → early-returns.
///    `--docs --analyze-graph` runs docs indexing then continues into
///    the main path so graph analysis also executes.
/// 6. Main path:
///    - `--check` → health report then return.
///    - `--incremental` / default full → full indexing pipeline.
///    - `--analyze-graph` inside main path → centrality + KG build.
///    - `--contracts` inside main path → contract indexing.
///    - `--export-docs` inside main path → doc export (only when not check).
pub fn execute_index(args: IndexArgs) -> Result<()> {
    let layout = get_layout()?;
    let config = load_config(&layout).unwrap_or_else(|err| {
        warn!("Failed to load config: {err}. Using defaults.");
        crate::config::model::Config::default()
    });

    // ── Mode 1: semantic dry-run (highest precedence) ──────────────────────
    if let Some(dry_run_opt) = args.semantic_dry_run {
        return execute_semantic_dry_run(&layout, &config, args.concurrency, dry_run_opt);
    }

    let db_path = layout.state_subdir().join("ledger.db");
    let mut storage = StorageManager::init(db_path.as_std_path())?;
    let repo_path = layout.root.clone();
    // ── Mode: Repair Metadata ──────────────────────────────────────────────
    if args.repair_metadata {
        return execute_repair_metadata(
            &layout,
            storage,
            &config,
            args.dry_run,
            args.yes,
            args.json,
        );
    }

    // ── Mode 2: Automated SCIP ────────────────────────────────────────────
    if args.auto_scip {
        let repo_root = layout.root.as_std_path();
        match crate::scip::orchestrator::ScipToolchain::detect(repo_root) {
            Some(toolchain) => {
                match toolchain.generate(repo_root) {
                    Ok(scip_path) => {
                        info!("Automatically generated SCIP index at {:?}", scip_path);
                        let res = execute_scip_index(&layout, &mut storage, scip_path.clone());

                        // Cleanup temporary index file if it's the default one we generated
                        if scip_path.exists()
                            && scip_path.file_name().and_then(|n| n.to_str())
                                == Some("ledgerful.temp.scip")
                        {
                            let _ = std::fs::remove_file(&scip_path);
                        }

                        // S4: If ingestion fails, we might still want to continue to main indexing,
                        // but the current precedence says SCIP ingestion is an early-return mode.
                        // Given the spec says "gracefully fall back to native Tree-Sitter parsing",
                        // if SCIP fails, we should fall through to the main path.
                        if let Err(e) = res {
                            warn!(
                                "SCIP ingestion failed: {}. Falling back to native indexing.",
                                e
                            );
                        } else {
                            return Ok(());
                        }
                    }
                    Err(e) => {
                        warn!(
                            "SCIP generation failed: {}. Falling back to native indexing.",
                            e
                        );
                    }
                }
            }
            None => {
                warn!("No suitable SCIP indexer found on PATH. Falling back to native indexing.");
            }
        }
    }

    // ── Mode 3: SCIP ingestion ─────────────────────────────────────────────
    if let Some(scip_path) = args.scip {
        return execute_scip_index(&layout, &mut storage, scip_path);
    }

    // ── Mode 3: standalone semantic indexing ─────────────────────────────
    if args.semantic && !args.analyze_graph {
        return execute_semantic_index(
            &layout,
            storage,
            &config,
            args.incremental,
            args.concurrency,
        );
    }

    // ── Mode 4: docs (standalone or combined with graph) ───────────────────
    if args.docs {
        if !args.analyze_graph {
            return execute_docs_index(&layout, &storage);
        }
        execute_docs_index(&layout, &storage)?;
    }

    let contracts_db_path = if args.contracts {
        Some(db_path.clone())
    } else {
        None
    };

    let mut indexer = ProjectIndexer::new(storage, repo_path.clone(), config.clone());

    // ── Mode 5: main indexing pipeline (check / incremental / full / graph / export) ─
    execute_main_mode(
        &mut indexer,
        &args,
        &layout,
        &config,
        contracts_db_path,
        &repo_path,
    )
}

/// Main indexing pipeline: check, incremental/full index, all extraction phases,
/// contracts, search index rebuild, output formatting, and doc export.
fn execute_main_mode(
    indexer: &mut ProjectIndexer,
    args: &IndexArgs,
    layout: &Layout,
    config: &crate::config::model::Config,
    contracts_db_path: Option<Utf8PathBuf>,
    repo_path: &camino::Utf8Path,
) -> Result<()> {
    // ── Sub-mode: check ────────────────────────────────────────────────────
    if args.check {
        return execute_check_mode(indexer, args);
    }

    // ── Sub-mode: incremental or full index ──────────────────────────────
    let stats = if args.incremental {
        indexer.incremental_index()?
    } else {
        indexer.full_index()?
    };

    // Backfill git metadata (last_touched_at, last_contributor) in
    // project_files (Track TA30). Skips the git walk if no NULL rows.
    if let Err(e) = indexer.backfill_git_metadata() {
        tracing::warn!("Git metadata backfill failed (non-fatal): {}", e);
    }

    // Index documentation files
    let doc_stats = indexer.index_docs()?;

    // Index directory topology
    let topo_stats = indexer.index_topology()?;

    // Classify entry points
    let ep_stats = indexer.classify_entrypoints()?;

    // Build call graph
    let cg_stats = indexer.build_call_graph()?;

    // Extract API routes
    let route_stats = indexer.extract_routes()?;

    // Extract data models
    let dm_stats = indexer.extract_data_models()?;

    // Extract observability patterns
    let obs_stats = indexer.extract_observability()?;

    // Extract test-to-symbol mappings
    let tm_stats = indexer.extract_test_mappings()?;

    // Extract CI/CD workflow gates
    let ci_stats = indexer.extract_ci_gates()?;

    // Extract env schema (declarations and references)
    let env_stats = indexer.extract_env_schema()?;

    // Infer service boundaries
    let service_stats = if config.coverage.service_inference_state()
        == crate::config::model::ServiceInferenceState::Enabled
    {
        indexer.infer_services()?
    } else {
        info!("Service inference disabled by coverage.services config.");
        ServiceIndexStats {
            services_inferred: 0,
            files_assigned: 0,
        }
    };

    // Compute centrality if requested
    let cent_stats = if args.analyze_graph {
        // Move storage out of the indexer for the shared graph-analysis driver,
        // then leave a fresh in-memory handle so the rest of the command can
        // still read/write SQLite metadata (e.g. contracts, Tantivy) if needed.
        let moved_storage = std::mem::replace(
            indexer.storage_mut(),
            StorageManager::init_from_conn(
                rusqlite::Connection::open_in_memory().into_diagnostic()?,
            ),
        );
        crate::index::run_graph_analysis(
            moved_storage,
            repo_path.as_std_path(),
            config,
            args.semantic,
            args.fast,
        )?
    } else {
        info!("Centrality computation skipped (use --analyze-graph to enable).");
        crate::index::centrality::CentralityStats {
            entry_points_count: 0,
            symbols_computed: 0,
            max_reachable: 0,
        }
    };

    let contracts_summary: Option<crate::contracts::index::ContractsIndexSummary> =
        if let Some(ref db_path) = contracts_db_path {
            Some(execute_contracts_index(layout, db_path.as_std_path())?)
        } else {
            None
        };

    // Update Tantivy search index (full-text search)
    let index_path = layout.search_index_dir();
    {
        let engine = crate::search::TantivySearchEngine::open_or_create(index_path.as_std_path())?;
        engine.clear()?;
        let stream_indexer = crate::search::StreamIndexer::new(engine);
        stream_indexer.index_repository(&layout.root)?;
    }

    // Verify search index integrity on disk
    let engine = crate::search::TantivySearchEngine::open_or_create(index_path.as_std_path())?;
    engine.verify_index_integrity(index_path.as_std_path())?;

    // ── Output formatting ──────────────────────────────────────────────────
    let output_stats = IndexOutputStats {
        stats,
        doc_stats,
        topo_stats,
        ep_stats,
        service_stats,
        cg_stats,
        route_stats,
        dm_stats,
        obs_stats,
        tm_stats,
        ci_stats,
        env_stats,
        cent_stats,
        contracts_summary,
        analyze_graph: args.analyze_graph,
    };
    if args.json {
        print_json_output(&output_stats)?;
    } else {
        print_human_output(&output_stats);
    }

    // ── Sub-mode: export-docs ────────────────────────────────────────────
    if args.export_docs && !args.check {
        execute_export_docs_mode(indexer, layout, args.doc_type.as_deref())?;
    }

    Ok(())
}

/// Check mode: report index health and staleness, exiting on missing or strict-stale.
fn execute_check_mode(indexer: &mut ProjectIndexer, args: &IndexArgs) -> Result<()> {
    let status = indexer.check_status()?;
    let discovered = indexer.discover_files()?;
    let is_missing = status.total_files == 0 && !discovered.is_empty();

    if args.json {
        let output = serde_json::to_string_pretty(&status).into_diagnostic()?;
        println!("{}", output);

        if let Some(assessment) = &status.assessment {
            match assessment.state {
                IndexFreshnessState::FreshEmpty | IndexFreshnessState::StaleEmpty => {
                    match assessment.empty_reason {
                        Some(EmptyIndexReason::NoSupportedFiles)
                        | Some(EmptyIndexReason::AllIndexableCandidatesIgnored) => {
                            println!("Index is up to date (0 indexable files).");
                        }
                        Some(EmptyIndexReason::RepositoryEmpty) => {
                            eprintln!(
                                "Error: Index is missing or empty. Run 'ledgerful index' to build it."
                            );
                        }
                        _ => {
                            if is_missing {
                                eprintln!(
                                    "Error: Index is missing or empty. Run 'ledgerful index' to build it."
                                );
                            } else {
                                println!("Index is up to date.");
                            }
                        }
                    }
                }
                IndexFreshnessState::Indeterminate => {
                    eprintln!(
                        "Error: Index state is indeterminate (metadata corruption or mismatch). Run 'ledgerful index --repair-metadata' to repair."
                    );
                }
                _ => {
                    if is_missing {
                        eprintln!(
                            "Error: Index is missing or empty. Run 'ledgerful index' to build it."
                        );
                    } else if status.stale_files > 0 {
                        if args.strict {
                            eprintln!(
                                "Error: Index is stale ({} files) and --strict is enabled.",
                                status.stale_files
                            );
                        } else {
                            println!(
                                "Warning: Index is stale ({} files). Run 'ledgerful index --incremental' to update.",
                                status.stale_files
                            );
                        }
                    } else {
                        println!("Index is up to date.");
                    }
                }
            }
        } else {
            // Fallback if assessment is missing for some reason
            if is_missing {
                eprintln!("Error: Index is missing or empty. Run 'ledgerful index' to build it.");
            } else if status.stale_files > 0 {
                if args.strict {
                    eprintln!(
                        "Error: Index is stale ({} files) and --strict is enabled.",
                        status.stale_files
                    );
                } else {
                    println!(
                        "Warning: Index is stale ({} files). Run 'ledgerful index --incremental' to update.",
                        status.stale_files
                    );
                }
            } else {
                println!("Index is up to date.");
            }
        }

        println!("Index Status:");
        println!("  Files indexed:   {}", status.total_files);
        println!("  Symbols indexed: {}", status.total_symbols);
        println!("  Stale files:     {}", status.stale_files);
        if let Some(last) = &status.last_indexed_at {
            println!("  Last indexed:    {}", last);
        } else {
            println!("  Last indexed:     never");
        }
    }

    let is_empty_expected = status.assessment.as_ref().map(|a| {
        (a.state == crate::index::staleness::IndexFreshnessState::FreshEmpty || a.state == crate::index::staleness::IndexFreshnessState::StaleEmpty) &&
        matches!(a.empty_reason, Some(crate::index::staleness::EmptyIndexReason::NoSupportedFiles) | Some(crate::index::staleness::EmptyIndexReason::AllIndexableCandidatesIgnored))
    }).unwrap_or(false);

    if is_missing && !is_empty_expected {
        std::process::exit(1);
    }
    if matches!(
        status.assessment.as_ref().map(|a| &a.state),
        Some(crate::index::staleness::IndexFreshnessState::Indeterminate)
    ) {
        std::process::exit(1);
    }
    if status.stale_files > 0 && args.strict {
        std::process::exit(1);
    }
    Ok(())
}

/// Export-docs mode: write knowledge-graph data to passive documentation.
fn execute_export_docs_mode(
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
