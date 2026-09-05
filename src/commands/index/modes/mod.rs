mod check;
mod export_docs;

use super::IndexArgs;
use super::graph::{execute_contracts_index, execute_docs_index};
use super::output::{IndexOutputStats, print_human_output, print_json_output};
use super::repair::execute_repair_metadata;
use super::semantic::{execute_semantic_dry_run, execute_semantic_index};
use crate::commands::helpers::get_layout;
use crate::config::load::load_config;
use crate::index::{ProjectIndexer, ServiceIndexStats};
use crate::scip::maybe_run_scip_augment;
use crate::state::layout::Layout;
use crate::state::storage::StorageManager;
use camino::Utf8PathBuf;
use check::execute_check_mode;
use export_docs::execute_export_docs_mode;
use miette::Result;
use tracing::{info, warn};

/// Mode-combination matrix for `ledgerful index`.
///
/// Precedence (early-return order) is critical and must be preserved:
/// 1. `--semantic-dry-run`  → preempts everything (returns immediately).
/// 2. `--semantic` (without `--analyze-graph`) → early-returns.
///    `--semantic --analyze-graph` falls through to the main path,
///    where semantic enrichment is applied inside graph analysis.
/// 3. `--docs` (without `--analyze-graph`) → early-returns.
///    `--docs --analyze-graph` runs docs indexing then continues into
///    the main path so graph analysis also executes.
/// 4. Main path:
///    - `--check` → health report then return.
///    - `--incremental` / default full → full indexing pipeline.
///    - SCIP augment (`--auto-scip` / `--scip PATH`) is **mutually exclusive**
///      per call site (spec §2.2b): without `--analyze-graph`, only after
///      `build_call_graph()`; with `--analyze-graph`, only inside
///      `run_graph_analysis` after extract-or-skip (never both). SCIP never
///      replaces the native index.
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
    let storage = StorageManager::init_with_layout(&layout)?;
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

    // ── SCIP is no longer an early-return mode (0095) ─────────────────────
    // `--auto-scip` and `--scip PATH` are handled inside execute_main_mode
    // (no-graph) or exclusively inside run_graph_analysis (--analyze-graph).

    // ── Mode: standalone semantic indexing ─────────────────────────────
    // 0161: pass raw force_full / force_incremental / json (do not collapse Auto).
    if args.semantic && !args.analyze_graph {
        return execute_semantic_index(
            &layout,
            storage,
            &config,
            args.full,
            args.incremental,
            args.concurrency,
            args.json,
        );
    }

    // ── Mode: docs (standalone or combined with graph) ───────────────────
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

    // ── Main indexing pipeline (check / incremental / full / graph / export) ─
    execute_main_mode(&mut indexer, &args, &layout, &config, contracts_db_path)
}

/// Main indexing pipeline: check, incremental/full index, all extraction phases,
/// contracts, search index rebuild, output formatting, and doc export.
fn execute_main_mode(
    indexer: &mut ProjectIndexer,
    args: &IndexArgs,
    layout: &Layout,
    config: &crate::config::model::Config,
    contracts_db_path: Option<Utf8PathBuf>,
) -> Result<()> {
    // ── Sub-mode: check ────────────────────────────────────────────────────
    if args.check {
        return execute_check_mode(indexer, args);
    }

    // ── Sub-mode: incremental or full index ──────────────────────────────
    // Graph path still collapses: only explicit `-i` without `--full` is incremental.
    let stats = if args.incremental && !args.full {
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

    // ── SCIP augment (0095 §2.2b): mutually exclusive call sites ────────
    // - without --analyze-graph → only here, after build_call_graph
    // - with --analyze-graph → only inside run_graph_analysis after
    //   extract-or-skip (one augment site per invocation; do not feed SCIP
    //   into infer_services on the graph path)
    let scip_json = if args.analyze_graph {
        let mut deferred = crate::scip::ScipIndexJson::did_not_run();
        if args.auto_scip || args.scip.is_some() {
            deferred.message =
                Some("SCIP deferred to --analyze-graph pass (after infer_services)".to_string());
        }
        deferred
    } else {
        maybe_run_scip_augment(
            layout,
            indexer.storage_mut(),
            config,
            args.auto_scip,
            args.scip.clone(),
        )
    };

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

    indexer.clear_content_cache();

    // Compute centrality if requested
    let (cent_stats, scip_json) = if args.analyze_graph {
        let (cent, scip_from_graph) = crate::index::run_graph_analysis(
            indexer,
            crate::index::SqliteExtractPolicy::AlreadyRan,
            args.semantic,
            args.fast,
            args.auto_scip,
            args.scip.clone(),
            Some(layout),
        )?;
        // Prefer the graph-pass SCIP result when analyze-graph ran (final edges).
        (cent, scip_from_graph.unwrap_or(scip_json))
    } else {
        info!("Centrality computation skipped (use --analyze-graph to enable).");
        (
            crate::index::centrality::CentralityStats {
                entry_points_count: 0,
                symbols_computed: 0,
                max_reachable: 0,
            },
            scip_json,
        )
    };

    let contracts_summary: Option<crate::contracts::index::ContractsIndexSummary> =
        if let Some(ref db_path) = contracts_db_path {
            Some(execute_contracts_index(layout, db_path.as_std_path())?)
        } else {
            None
        };

    // Update Tantivy search index (full-text search; no incremental FTS API)
    let index_path = layout.search_index_dir();
    crate::search::rebuild_tantivy_index(layout)?;

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
        scip: Some(scip_json),
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
