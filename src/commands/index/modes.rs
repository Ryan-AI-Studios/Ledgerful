use super::IndexArgs;
use super::graph::{execute_contracts_index, execute_docs_index};
use super::output::{IndexOutputStats, print_human_output, print_json_output};
use super::repair::execute_repair_metadata;
use super::semantic::{execute_semantic_dry_run, execute_semantic_index};
use crate::commands::helpers::get_layout;
use crate::config::load::load_config;
use crate::index::staleness::{EmptyIndexReason, IndexFreshnessState};
use crate::index::{ProjectIndexer, ServiceIndexStats};
use crate::scip::maybe_run_scip_augment;
use crate::state::layout::Layout;
use crate::state::storage::StorageManager;
use camino::Utf8PathBuf;
use miette::{IntoDiagnostic, Result};
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
///      `run_graph_analysis` after `infer_services` (never both). SCIP never
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
    if args.semantic && !args.analyze_graph {
        return execute_semantic_index(
            &layout,
            storage,
            &config,
            args.incremental,
            args.concurrency,
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

    // ── SCIP augment (0095 §2.2b): mutually exclusive call sites ────────
    // - without --analyze-graph → only here, after build_call_graph
    // - with --analyze-graph → only inside run_graph_analysis after
    //   infer_services (graph rebuild would discard any earlier edges)
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

    // Compute centrality if requested
    let (cent_stats, scip_json) = if args.analyze_graph {
        // Move storage out of the indexer for the shared graph-analysis driver,
        // then leave a fresh in-memory handle so the rest of the command can
        // still read/write SQLite metadata (e.g. contracts, Tantivy) if needed.
        let moved_storage = std::mem::replace(
            indexer.storage_mut(),
            StorageManager::init_from_conn(
                rusqlite::Connection::open_in_memory().into_diagnostic()?,
            ),
        );
        let (cent, scip_from_graph) = crate::index::run_graph_analysis(
            moved_storage,
            repo_path.as_std_path(),
            config,
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

/// Severity of a check-mode message.
/// Errors always go to stderr. Info (warnings/status) go to stdout in human mode
/// and stderr under `--json` so stdout stays pure JSON.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CheckMsgKind {
    Error,
    Info,
}

/// Typed verdict for `index --check`: one place decides messages and exit flags.
#[derive(Debug)]
struct CheckVerdict {
    messages: Vec<(CheckMsgKind, String)>,
    exit_missing: bool,
    exit_indeterminate: bool,
    exit_strict_stale: bool,
}

/// Collapse the is_missing / empty / stale / strict ladder into a single verdict.
/// Used by both the human and `--json` branches so messages cannot drift apart.
fn decide_check_verdict(
    status: &crate::index::orchestrator::IndexStatus,
    is_missing: bool,
    strict: bool,
) -> CheckVerdict {
    let is_empty_expected = status
        .assessment
        .as_ref()
        .map(|a| {
            matches!(
                a.state,
                IndexFreshnessState::FreshEmpty | IndexFreshnessState::StaleEmpty
            ) && matches!(
                a.empty_reason,
                Some(EmptyIndexReason::NoSupportedFiles)
                    | Some(EmptyIndexReason::AllIndexableCandidatesIgnored)
            )
        })
        .unwrap_or(false);

    let is_indeterminate = matches!(
        status.assessment.as_ref().map(|a| &a.state),
        Some(IndexFreshnessState::Indeterminate)
    );

    let mut messages = Vec::new();
    let mut exit_missing = false;
    let mut exit_indeterminate = false;
    let mut exit_strict_stale = false;

    if let Some(assessment) = &status.assessment {
        match assessment.state {
            IndexFreshnessState::FreshEmpty | IndexFreshnessState::StaleEmpty => {
                match assessment.empty_reason {
                    Some(EmptyIndexReason::NoSupportedFiles)
                    | Some(EmptyIndexReason::AllIndexableCandidatesIgnored) => {
                        messages.push((
                            CheckMsgKind::Info,
                            "Index is up to date (0 indexable files).".to_string(),
                        ));
                    }
                    Some(EmptyIndexReason::RepositoryEmpty) => {
                        messages.push((
                            CheckMsgKind::Error,
                            "Error: Index is missing or empty. Run 'ledgerful index' to build it."
                                .to_string(),
                        ));
                        exit_missing = true;
                    }
                    _ => {
                        if is_missing {
                            messages.push((
                                CheckMsgKind::Error,
                                "Error: Index is missing or empty. Run 'ledgerful index' to build it."
                                    .to_string(),
                            ));
                            exit_missing = true;
                        } else {
                            messages.push((CheckMsgKind::Info, "Index is up to date.".to_string()));
                        }
                    }
                }
            }
            IndexFreshnessState::Indeterminate => {
                messages.push((
                    CheckMsgKind::Error,
                    "Error: Index state is indeterminate (metadata corruption or mismatch). Run 'ledgerful index --repair-metadata' to repair."
                        .to_string(),
                ));
                exit_indeterminate = true;
            }
            _ => {
                if is_missing {
                    messages.push((
                        CheckMsgKind::Error,
                        "Error: Index is missing or empty. Run 'ledgerful index' to build it."
                            .to_string(),
                    ));
                    exit_missing = true;
                } else if status.stale_files > 0 {
                    if strict {
                        messages.push((
                            CheckMsgKind::Error,
                            format!(
                                "Error: Index is stale ({} files) and --strict is enabled.",
                                status.stale_files
                            ),
                        ));
                        exit_strict_stale = true;
                    } else {
                        messages.push((
                            CheckMsgKind::Info,
                            format!(
                                "Warning: Index is stale ({} files). Run 'ledgerful index --incremental' to update.",
                                status.stale_files
                            ),
                        ));
                    }
                } else {
                    messages.push((CheckMsgKind::Info, "Index is up to date.".to_string()));
                }
            }
        }
    } else {
        // Fallback if assessment is missing for some reason
        if is_missing {
            messages.push((
                CheckMsgKind::Error,
                "Error: Index is missing or empty. Run 'ledgerful index' to build it.".to_string(),
            ));
            exit_missing = true;
        } else if status.stale_files > 0 {
            if strict {
                messages.push((
                    CheckMsgKind::Error,
                    format!(
                        "Error: Index is stale ({} files) and --strict is enabled.",
                        status.stale_files
                    ),
                ));
                exit_strict_stale = true;
            } else {
                messages.push((
                    CheckMsgKind::Info,
                    format!(
                        "Warning: Index is stale ({} files). Run 'ledgerful index --incremental' to update.",
                        status.stale_files
                    ),
                ));
            }
        } else {
            messages.push((CheckMsgKind::Info, "Index is up to date.".to_string()));
        }
    }

    // Align exit flags with the process::exit sites (missing must respect empty-expected).
    if is_missing && !is_empty_expected {
        exit_missing = true;
        if !messages.iter().any(|(k, _)| *k == CheckMsgKind::Error) {
            messages.push((
                CheckMsgKind::Error,
                "Error: Index is missing or empty. Run 'ledgerful index' to build it.".to_string(),
            ));
        }
    }
    if is_indeterminate {
        exit_indeterminate = true;
        if !messages
            .iter()
            .any(|(k, m)| *k == CheckMsgKind::Error && m.contains("indeterminate"))
        {
            messages.push((
                CheckMsgKind::Error,
                "Error: Index state is indeterminate (metadata corruption or mismatch). Run 'ledgerful index --repair-metadata' to repair."
                    .to_string(),
            ));
        }
    }
    if status.stale_files > 0 && strict {
        exit_strict_stale = true;
        if !messages
            .iter()
            .any(|(k, m)| *k == CheckMsgKind::Error && m.contains("--strict"))
        {
            messages.push((
                CheckMsgKind::Error,
                format!(
                    "Error: Index is stale ({} files) and --strict is enabled.",
                    status.stale_files
                ),
            ));
        }
    }

    CheckVerdict {
        messages,
        exit_missing,
        exit_indeterminate,
        exit_strict_stale,
    }
}

fn emit_check_messages(messages: &[(CheckMsgKind, String)], json: bool) {
    for (kind, msg) in messages {
        match kind {
            CheckMsgKind::Error => eprintln!("{msg}"),
            // Under --json, keep stdout pure JSON: route info to stderr.
            CheckMsgKind::Info if json => eprintln!("{msg}"),
            CheckMsgKind::Info => println!("{msg}"),
        }
    }
}

fn print_check_status_block(status: &crate::index::orchestrator::IndexStatus) {
    println!("Index Status:");
    println!("  Files indexed:   {}", status.total_files);
    println!("  Symbols indexed: {}", status.total_symbols);
    println!("  Stale files:     {}", status.stale_files);
    if let Some(last) = &status.last_indexed_at {
        println!("  Last indexed:    {last}");
    } else {
        println!("  Last indexed:     never");
    }
}

/// Check mode: report index health and staleness, exiting on missing or strict-stale.
/// Mirrors `execute_main_mode`'s `if args.json { … } else { … }` split so human
/// prose is never nested inside the JSON branch (operator-surface-policy §3).
fn execute_check_mode(indexer: &mut ProjectIndexer, args: &IndexArgs) -> Result<()> {
    let status = indexer.check_status()?;
    let discovered = indexer.discover_files()?;
    let is_missing = status.total_files == 0 && !discovered.is_empty();

    let verdict = decide_check_verdict(&status, is_missing, args.strict);

    let will_exit = verdict.exit_missing || verdict.exit_indeterminate || verdict.exit_strict_stale;

    if args.json {
        let output = serde_json::to_string_pretty(&status).into_diagnostic()?;
        println!("{output}");
        // Messages on stderr only — stdout is the JSON payload.
        emit_check_messages(&verdict.messages, true);
    } else {
        emit_check_messages(&verdict.messages, false);
        // Status block is the healthy/warning human report. On exit-1 paths keep
        // stdout empty so CI gates see diagnostics only on stderr (DoD-2).
        if !will_exit {
            print_check_status_block(&status);
        }
    }

    // Emit messages first (above); then exit so both paths diagnose before process::exit.
    if verdict.exit_missing {
        std::process::exit(1);
    }
    if verdict.exit_indeterminate {
        std::process::exit(1);
    }
    if verdict.exit_strict_stale {
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
