use camino::Utf8PathBuf;
use miette::{IntoDiagnostic, Result};
use std::env;

use crate::config::load::load_config;
use crate::docs::index::run_docs_index;
use crate::index::{ProjectIndexer, ServiceIndexStats};
use crate::state::layout::Layout;
use crate::state::storage::StorageManager;
use tracing::{info, warn};

type ParsedSemanticFile = (
    std::path::PathBuf,
    String,
    Vec<crate::semantic::chunker::AstChunk>,
);
type ParsedSemanticFileResult = std::result::Result<ParsedSemanticFile, String>;
const SEMANTIC_EMBEDDING_BATCH_SIZE: usize = 8;

fn semantic_embedding_batches(
    chunks: &[crate::semantic::chunker::AstChunk],
    batch_size: usize,
) -> Vec<Vec<crate::semantic::chunker::AstChunk>> {
    debug_assert!(batch_size > 0);
    chunks
        .chunks(batch_size)
        .map(|batch| batch.to_vec())
        .collect()
}

fn get_repo_root() -> Result<Utf8PathBuf> {
    let current_dir = env::current_dir().into_diagnostic()?;
    let discovered = gix::discover(&current_dir).into_diagnostic()?;
    let root = discovered
        .workdir()
        .ok_or_else(|| miette::miette!("Failed to find work directory for repository"))?;

    Utf8PathBuf::from_path_buf(root.to_path_buf())
        .map_err(|_| miette::miette!("Repository root is not valid UTF-8"))
}

fn get_layout() -> Result<Layout> {
    let root = get_repo_root()?;
    Ok(Layout::new(root))
}

// ── Shared helpers for semantic modes ─────────────────────────────────────────

/// Resolve parse and embed concurrency from CLI override, semantic config,
/// and local-model defaults. Used by both semantic index and dry-run.
fn resolve_semantic_concurrency(
    concurrency_override: Option<usize>,
    config: &crate::config::model::Config,
) -> crate::semantic::concurrency::ResolvedConcurrency {
    use crate::semantic::concurrency::{ResolveOptions, resolve_split_semantic_concurrency};
    let available_parallelism = std::thread::available_parallelism()
        .ok()
        .map(|n| std::num::NonZeroUsize::new(n.get()).expect("available_parallelism is non-zero"));
    let resolve_opts = ResolveOptions {
        available_parallelism,
        ..Default::default()
    };
    resolve_split_semantic_concurrency(
        concurrency_override,
        &config.semantic,
        config.local_model.concurrency,
        resolve_opts,
    )
}

/// Walk the repository for candidate semantic-index files.
fn walk_repo_for_semantic_files(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    fn walk_dir(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if matches!(
                    name,
                    ".git"
                        | ".ledgerful"
                        | "target"
                        | "node_modules"
                        | ".agents"
                        | ".claude"
                        | ".codex"
                        | ".opencode"
                ) {
                    continue;
                }
                walk_dir(&path, out);
            } else {
                let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
                if matches!(ext, "rs" | "ts" | "tsx" | "js" | "jsx" | "py" | "go") {
                    out.push(path);
                }
            }
        }
    }
    let mut out = Vec::new();
    walk_dir(root, &mut out);
    out
}

#[derive(Default)]
pub struct IndexArgs {
    pub incremental: bool,
    pub check: bool,
    pub strict: bool,
    pub json: bool,
    pub analyze_graph: bool,
    pub docs: bool,
    pub contracts: bool,
    pub semantic: bool,
    pub scip: Option<std::path::PathBuf>,
    pub auto_scip: bool,
    pub export_docs: bool,
    pub doc_type: Option<String>,
    /// CLI override for rayon thread count (HP2). `None` = use config or rayon default.
    pub concurrency: Option<usize>,
    /// Print resolved semantic settings and exit. Optionally takes a path for JSON output.
    pub semantic_dry_run: Option<Option<std::path::PathBuf>>,
    /// Use Gemini for semantic extraction (fast, large context) instead of local model
    pub fast: bool,
    pub repair_metadata: bool,
    pub dry_run: bool,
    pub yes: bool,
}

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
        use crate::index::staleness::{EmptyIndexReason, IndexFreshnessState};

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

/// Bundles all index statistics for output formatting.
/// Eliminates the 15-parameter signatures on print helpers.
struct IndexOutputStats {
    stats: crate::index::orchestrator::IndexStats,
    doc_stats: crate::index::docs::DocIndexStats,
    topo_stats: crate::index::topology::TopologyIndexStats,
    ep_stats: crate::index::entrypoint::EntrypointStats,
    service_stats: ServiceIndexStats,
    cg_stats: crate::index::call_graph::CallGraphStats,
    route_stats: crate::index::routes::RouteStats,
    dm_stats: crate::index::data_models::DataModelStats,
    obs_stats: crate::index::observability::ObservabilityStats,
    tm_stats: crate::index::test_mapping::TestMappingStats,
    ci_stats: crate::index::ci_gates::CIGateStats,
    env_stats: crate::index::env_schema::EnvSchemaStats,
    cent_stats: crate::index::centrality::CentralityStats,
    contracts_summary: Option<crate::contracts::index::ContractsIndexSummary>,
    analyze_graph: bool,
}

// ── Output formatting helpers ───────────────────────────────────────────────

fn print_json_output(output: &IndexOutputStats) -> Result<()> {
    let mut merged = serde_json::to_value(&output.stats).into_diagnostic()?;
    let doc_obj = serde_json::to_value(&output.doc_stats).into_diagnostic()?;
    let topo_obj = serde_json::to_value(&output.topo_stats).into_diagnostic()?;
    let ep_obj = serde_json::to_value(&output.ep_stats).into_diagnostic()?;
    let service_obj = serde_json::to_value(&output.service_stats).into_diagnostic()?;
    if let (Some(map), Some(doc)) = (merged.as_object_mut(), doc_obj.as_object()) {
        for (k, v) in doc {
            map.insert(format!("doc_{}", k), v.clone());
        }
    }
    if let (Some(map), Some(topo)) = (merged.as_object_mut(), topo_obj.as_object()) {
        for (k, v) in topo {
            map.insert(format!("topo_{}", k), v.clone());
        }
    }
    if let (Some(map), Some(ep)) = (merged.as_object_mut(), ep_obj.as_object()) {
        for (k, v) in ep {
            map.insert(format!("ep_{}", k), v.clone());
        }
    }
    if let (Some(map), Some(svc)) = (merged.as_object_mut(), service_obj.as_object()) {
        for (k, v) in svc {
            map.insert(format!("service_{}", k), v.clone());
        }
    }
    let cg_obj = serde_json::to_value(&output.cg_stats).into_diagnostic()?;
    if let (Some(map), Some(cg)) = (merged.as_object_mut(), cg_obj.as_object()) {
        for (k, v) in cg {
            map.insert(format!("cg_{}", k), v.clone());
        }
    }
    let route_obj = serde_json::to_value(&output.route_stats).into_diagnostic()?;
    if let (Some(map), Some(route)) = (merged.as_object_mut(), route_obj.as_object()) {
        for (k, v) in route {
            map.insert(format!("route_{}", k), v.clone());
        }
    }
    let dm_obj = serde_json::to_value(&output.dm_stats).into_diagnostic()?;
    if let (Some(map), Some(dm)) = (merged.as_object_mut(), dm_obj.as_object()) {
        for (k, v) in dm {
            map.insert(format!("dm_{}", k), v.clone());
        }
    }
    let obs_obj = serde_json::to_value(&output.obs_stats).into_diagnostic()?;
    if let (Some(map), Some(obs)) = (merged.as_object_mut(), obs_obj.as_object()) {
        for (k, v) in obs {
            map.insert(format!("obs_{}", k), v.clone());
        }
    }
    let tm_obj = serde_json::to_value(&output.tm_stats).into_diagnostic()?;
    if let (Some(map), Some(tm)) = (merged.as_object_mut(), tm_obj.as_object()) {
        for (k, v) in tm {
            map.insert(format!("tm_{}", k), v.clone());
        }
    }
    let ci_obj = serde_json::to_value(&output.ci_stats).into_diagnostic()?;
    if let (Some(map), Some(ci)) = (merged.as_object_mut(), ci_obj.as_object()) {
        for (k, v) in ci {
            map.insert(format!("ci_{}", k), v.clone());
        }
    }
    let env_obj = serde_json::to_value(&output.env_stats).into_diagnostic()?;
    if let (Some(map), Some(env)) = (merged.as_object_mut(), env_obj.as_object()) {
        for (k, v) in env {
            map.insert(format!("env_{}", k), v.clone());
        }
    }
    if output.analyze_graph {
        let cent_obj = serde_json::to_value(&output.cent_stats).into_diagnostic()?;
        if let (Some(map), Some(cent)) = (merged.as_object_mut(), cent_obj.as_object()) {
            for (k, v) in cent {
                map.insert(format!("cent_{}", k), v.clone());
            }
        }
    }
    if let Some(ref cs) = output.contracts_summary {
        let cs_obj = serde_json::to_value(cs).into_diagnostic()?;
        if let (Some(map), Some(cs)) = (merged.as_object_mut(), cs_obj.as_object()) {
            for (k, v) in cs {
                map.insert(format!("contracts_{}", k), v.clone());
            }
        }
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&merged).into_diagnostic()?
    );
    Ok(())
}

fn print_human_output(output: &IndexOutputStats) {
    println!("Indexing complete:");
    println!("  Files indexed:   {}", output.stats.files_indexed);
    println!("  Symbols indexed: {}", output.stats.symbols_indexed);
    if output.stats.parse_failures > 0 {
        println!("  Parse failures:  {}", output.stats.parse_failures);
    }
    if output.stats.skipped_binary > 0 {
        println!("  Skipped binary:  {}", output.stats.skipped_binary);
    }
    if output.stats.skipped_unsupported > 0 {
        println!(
            "  Skipped unsupported: {}",
            output.stats.skipped_unsupported
        );
    }
    println!("  Duration:        {}ms", output.stats.duration_ms);
    println!();
    println!("Documentation:");
    println!("  Docs indexed:    {}", output.doc_stats.docs_indexed);
    if output.doc_stats.parse_failures > 0 {
        println!("  Doc parse failures: {}", output.doc_stats.parse_failures);
    }
    if output.doc_stats.missing_readme {
        println!("  README:          not found");
    } else {
        println!("  README:          found");
    }
    println!();
    println!("Topology:");
    println!(
        "  Directories classified: {}",
        output.topo_stats.directories_classified
    );
    if output.topo_stats.unclassified > 0 {
        println!("  Unclassified:    {}", output.topo_stats.unclassified);
    }
    let role_order = [
        crate::index::topology::DirectoryRole::Source,
        crate::index::topology::DirectoryRole::Test,
        crate::index::topology::DirectoryRole::Config,
        crate::index::topology::DirectoryRole::Infrastructure,
        crate::index::topology::DirectoryRole::Documentation,
        crate::index::topology::DirectoryRole::Generated,
        crate::index::topology::DirectoryRole::Vendor,
        crate::index::topology::DirectoryRole::BuildArtifact,
    ];
    for role in &role_order {
        if let Some(count) = output.topo_stats.role_counts.get(role) {
            println!("  {}: {}", role.as_str(), count);
        }
    }
    println!();
    println!("Entrypoints:");
    println!("  Entrypoints:   {}", output.ep_stats.entrypoints);
    println!("  Handlers:      {}", output.ep_stats.handlers);
    println!("  Public APIs:   {}", output.ep_stats.public_apis);
    println!("  Tests:         {}", output.ep_stats.tests);
    println!("  Internal:     {}", output.ep_stats.internal);
    println!();
    println!("Call Graph:");
    println!("  Edges:          {}", output.cg_stats.total_edges);
    println!("  Resolved:       {}", output.cg_stats.resolved_edges);
    println!("  Unresolved:     {}", output.cg_stats.unresolved_edges);
    println!("  Ambiguous:      {}", output.cg_stats.ambiguous_edges);
    println!("  Files processed: {}", output.cg_stats.files_processed);
    println!();
    println!("API Routes:");
    println!("  Total routes:   {}", output.route_stats.total_routes);
    if !output.route_stats.frameworks_detected.is_empty() {
        println!(
            "  Frameworks:    {}",
            output.route_stats.frameworks_detected.join(", ")
        );
    }
    println!("  Files processed: {}", output.route_stats.files_processed);
    println!();
    println!("Data Models:");
    println!("  Total models:   {}", output.dm_stats.total_models);
    println!("  Files processed: {}", output.dm_stats.files_processed);
    println!();
    println!("Observability:");
    println!("  Total patterns: {}", output.obs_stats.total_patterns);
    println!(
        "  Error handling patterns: {}",
        output.obs_stats.error_handling_patterns
    );
    println!(
        "  Telemetry patterns: {}",
        output.obs_stats.telemetry_patterns
    );
    println!("  Files processed: {}", output.obs_stats.files_processed);
    println!();
    println!("Test Mapping:");
    println!("  Total mappings: {}", output.tm_stats.total_mappings);
    println!("  Import mappings: {}", output.tm_stats.import_mappings);
    println!(
        "  Naming convention mappings: {}",
        output.tm_stats.naming_convention_mappings
    );
    println!("  Files processed: {}", output.tm_stats.files_processed);
    println!();
    println!("CI/CD Gates:");
    println!("  Total gates: {}", output.ci_stats.total_gates);
    println!("  GitHub Actions: {}", output.ci_stats.github_actions_gates);
    println!("  GitLab CI: {}", output.ci_stats.gitlab_ci_gates);
    println!("  CircleCI: {}", output.ci_stats.circleci_gates);
    println!("  Makefile: {}", output.ci_stats.makefile_gates);
    println!("  Files processed: {}", output.ci_stats.files_processed);
    println!();
    println!("Env Schema:");
    println!(
        "  Total declarations: {}",
        output.env_stats.total_declarations
    );
    println!("  Total references: {}", output.env_stats.total_references);
    println!(
        "  Dotenv declarations: {}",
        output.env_stats.dotenv_declarations
    );
    println!(
        "  Config declarations: {}",
        output.env_stats.config_declarations
    );
    println!("  Files processed: {}", output.env_stats.files_processed);
    if output.analyze_graph {
        println!();
        println!("Centrality:");
        println!("  Entry points:   {}", output.cent_stats.entry_points_count);
        println!("  Symbols computed: {}", output.cent_stats.symbols_computed);
        println!("  Max reachable:  {}", output.cent_stats.max_reachable);
    }

    if let Some(ref cs) = output.contracts_summary {
        println!();
        println!("Contracts:");
        println!("  Specs parsed:     {}", cs.specs_parsed);
        println!("  New endpoints:    {}", cs.endpoints_new);
        println!("  Skipped:          {}", cs.endpoints_skipped);
        println!("  Deleted:          {}", cs.endpoints_deleted);
    }

    println!();
    println!("Services:");
    println!(
        "  Services inferred: {}",
        output.service_stats.services_inferred
    );
    println!(
        "  Files assigned:    {}",
        output.service_stats.files_assigned
    );
}

fn execute_docs_index(layout: &Layout, storage: &StorageManager) -> Result<()> {
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

fn execute_semantic_index(
    layout: &Layout,
    storage: StorageManager,
    config: &crate::config::model::Config,
    incremental: bool,
    concurrency_override: Option<usize>,
) -> Result<()> {
    use crate::semantic::SemanticDiscovery;
    use indicatif::{ProgressBar, ProgressStyle};
    use rayon::prelude::*;

    let cozo = storage
        .cozo
        .as_ref()
        .ok_or_else(|| miette::miette!("CozoDB storage not initialized"))?;

    let semantic = SemanticDiscovery::new_with_semantic_config(
        config.local_model.clone(),
        config.semantic.clone(),
        cozo,
    )?;

    // HP3: ensure the semantic file-hash tracking schema exists
    semantic.ensure_file_hash_schema()?;

    let resolved = resolve_semantic_concurrency(concurrency_override, config);
    let parse_threads = resolved.parse_threads.get();
    let embed_cap = resolved.embed_threads.get();

    info!(
        "Semantic indexing started: incremental={incremental}, cli_concurrency={:?}",
        concurrency_override
    );
    info!("Semantic indexing threads: parse={parse_threads}, embed_concurrency={embed_cap}");

    info!("Indexing repository for semantic search...");

    // ── Phase 1: Collect candidate files ───────────────────────────────────
    let repo_root = layout.root.as_std_path();
    let candidate_paths = walk_repo_for_semantic_files(repo_root);

    // HP3: On incremental runs filter to only files whose hash has changed.
    let files_to_process: Vec<std::path::PathBuf> = if incremental {
        let tracked_files = semantic.get_tracked_files()?;
        for tracked in tracked_files {
            let path = std::path::Path::new(&tracked);
            if !path.exists() {
                info!("Pruning deleted file from semantic index: {}", tracked);
                if let Err(e) = semantic.remove_file_snippets(&tracked) {
                    warn!(
                        "Failed to prune snippets for deleted file {}: {}",
                        tracked, e
                    );
                }
                if let Err(e) = semantic.remove_file_hash(&tracked) {
                    warn!(
                        "Failed to remove file hash for deleted file {}: {}",
                        tracked, e
                    );
                }
            }
        }

        candidate_paths
            .into_iter()
            .filter(|path| {
                let Ok(content) = crate::util::fs::read_to_string_with_encoding(path) else {
                    return true; // re-try unreadable files
                };
                let hash = blake3::hash(content.as_bytes()).to_hex().to_string();
                !semantic.is_file_hash_current(path, &hash)
            })
            .collect()
    } else {
        // Full index: prune snippets for files that no longer exist
        semantic.prune_deleted_snippets(repo_root)?;
        candidate_paths
    };

    if files_to_process.is_empty() {
        info!("Semantic index is up to date: no files changed since last index");
        return Ok(());
    }

    info!(
        "Semantic indexing will process {} files",
        files_to_process.len()
    );

    // ── Phase 2: Configure Rayon thread pool (U13/U14) ──────────────────────

    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(parse_threads)
        .build()
        .map_err(|e| miette::miette!("Failed to build Rayon thread pool: {}", e))?;

    let embed_semaphore =
        std::sync::Arc::new(crate::semantic::concurrency::EmbedSemaphore::new(embed_cap));

    // ── Phase 3: Parallel parse + embed with progress bar (HP2 + HP4) ──────
    let total = files_to_process.len();

    let pb_parse = ProgressBar::new(total as u64);
    if !crate::util::term::is_interactive() {
        pb_parse.set_draw_target(indicatif::ProgressDrawTarget::hidden());
    }
    pb_parse.set_style(
        ProgressStyle::with_template(
            "  {spinner:.cyan} Parsing [{bar:40.cyan/dim}] {pos}/{len} files  {elapsed_precise}",
        )
        .unwrap_or_else(|_| ProgressStyle::with_template("{pos}/{len}").unwrap())
        .progress_chars("█▓░"),
    );
    pb_parse.enable_steady_tick(std::time::Duration::from_millis(80));

    let parsed_files_res: Vec<ParsedSemanticFileResult> = pool.install(|| {
        files_to_process
            .into_par_iter()
            .map(|path| {
                let res = match crate::util::fs::read_to_string_with_encoding(&path) {
                    Ok(content) => {
                        match crate::semantic::chunker::AstChunker::chunk_file(&path, &content) {
                            Ok(chunks) => Ok((path, content, chunks)),
                            Err(e) => Err(format!("{}: {}", path.display(), e)),
                        }
                    }
                    Err(e) => Err(format!("{}: {}", path.display(), e)),
                };
                pb_parse.inc(1);
                res
            })
            .collect()
    });
    pb_parse.finish_and_clear();

    let mut parsed_files = Vec::new();
    let mut parse_errors = Vec::new();
    for res in parsed_files_res {
        match res {
            Ok(val) => parsed_files.push(val),
            Err(e) => parse_errors.push(e),
        }
    }

    for err in &parse_errors {
        warn!("Semantic indexing skipped due to parse error: {}", err);
    }

    // Flatten chunks
    let mut flat_chunks = Vec::new();
    let mut successful_files = Vec::new();
    for (path, content, chunks) in parsed_files {
        let hash = blake3::hash(content.as_bytes()).to_hex().to_string();
        successful_files.push((path.clone(), hash));
        for chunk in chunks {
            flat_chunks.push(chunk);
        }
    }

    let files_indexed_count = successful_files.len();

    // Batch embedding generation
    let mut all_embeddings = Vec::new();
    if !flat_chunks.is_empty() {
        let pb_embed = ProgressBar::new(flat_chunks.len() as u64);
        if !crate::util::term::is_interactive() {
            pb_embed.set_draw_target(indicatif::ProgressDrawTarget::hidden());
        }
        pb_embed.set_style(
            ProgressStyle::with_template(
                "  {spinner:.cyan} Embedding [{bar:40.green/dim}] {pos}/{len} chunks  {elapsed_precise}",
            )
            .unwrap_or_else(|_| ProgressStyle::with_template("{pos}/{len}").unwrap())
            .progress_chars("█▓░"),
        );
        pb_embed.enable_steady_tick(std::time::Duration::from_millis(80));

        let chunk_batches: Vec<Vec<crate::semantic::chunker::AstChunk>> =
            semantic_embedding_batches(&flat_chunks, SEMANTIC_EMBEDDING_BATCH_SIZE);

        let pb_embed_ref = pb_embed.clone();
        let embed_sem_ref = embed_semaphore.clone();
        let embedding_results: Result<Vec<Vec<Vec<f32>>>, String> = pool.install(|| {
            chunk_batches
                .into_par_iter()
                .map(|batch| {
                    let _permit = embed_sem_ref.acquire();
                    let texts: Vec<String> = batch.iter().map(|c| c.to_embedding_text()).collect();
                    let text_refs: Vec<&str> = texts.iter().map(|s| s.as_str()).collect();
                    let embedder_res = semantic
                        .embedder
                        .embed_batch(&text_refs)
                        .map_err(|e| e.to_string());
                    pb_embed_ref.inc(batch.len() as u64);
                    embedder_res
                })
                .collect()
        });

        pb_embed.finish_and_clear();

        match embedding_results {
            Ok(batches) => {
                for batch in batches {
                    all_embeddings.extend(batch);
                }
            }
            Err(e) => {
                return Err(miette::miette!("Embedding generation failed: {}", e));
            }
        }
    }

    // ── Phase 4: Batch ingest into CozoDB (single-threaded for safety) ─────
    if !successful_files.is_empty() {
        info!("Pruning stale semantic database rows...");
        for (path, _) in &successful_files {
            let path_str = path.to_string_lossy();
            if let Err(e) = semantic.remove_file_snippets(&path_str) {
                warn!(
                    "Failed to prune stale snippets for {}: {}",
                    path.display(),
                    e
                );
            }
        }
    }

    if !flat_chunks.is_empty() {
        let spinner = ProgressBar::new_spinner();
        if !crate::util::term::is_interactive() {
            spinner.set_draw_target(indicatif::ProgressDrawTarget::hidden());
        }
        spinner.set_style(
            ProgressStyle::with_template(
                "  {spinner:.yellow} Building HNSW index… {elapsed_precise}",
            )
            .unwrap_or_else(|_| ProgressStyle::default_spinner()),
        );
        spinner.enable_steady_tick(std::time::Duration::from_millis(100));

        info!(
            "Ingesting {} snippets into vector store...",
            flat_chunks.len()
        );
        semantic.index_chunks_batched(flat_chunks, all_embeddings)?;
        spinner.finish_and_clear();
    }

    // Record new hashes only for successfully processed files
    for (path, hash) in successful_files {
        if let Err(e) = semantic.record_file_hash(&path, &hash) {
            warn!("Failed to record file hash for {}: {}", path.display(), e);
        }
    }

    println!(
        "Semantic indexing complete: {files_indexed_count}/{total} files produced embeddings{}.",
        if incremental { " (incremental)" } else { "" }
    );
    Ok(())
}

fn execute_scip_index(
    layout: &Layout,
    storage: &mut StorageManager,
    scip_path: std::path::PathBuf,
) -> Result<()> {
    use crate::index::rows::{get_file_id_by_path, insert_symbol_row};
    use crate::scip::{
        ScipIndex, ScipSymbolMapper, is_scip_stale, normalize_scip_path, register_scip_index,
    };

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

        let symbol_info_map: std::collections::HashMap<_, _> = scip_index
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

fn execute_contracts_index(
    layout: &Layout,
    db_path: &std::path::Path,
) -> miette::Result<crate::contracts::index::ContractsIndexSummary> {
    use crate::contracts::index::index_contracts;
    use rusqlite::Connection;

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

pub fn execute_index_check(
    path: &std::path::Path,
    threshold: u64,
    json: bool,
    strict: bool,
) -> Result<()> {
    let root = camino::Utf8PathBuf::from_path_buf(path.to_path_buf())
        .map_err(|_| miette::miette!("Invalid UTF-8 in path"))?;
    let layout = Layout::new(root.as_str());

    let storage_res = StorageManager::open_read_only(&layout.root);
    let mut warning = match storage_res {
        Ok(ref storage) => crate::index::staleness::check_index_staleness(storage, threshold),
        Err(_) => Some(crate::index::staleness::StalenessWarning {
            days_since_indexed: 999,
            stale_files: 0,
            unindexed_files: 0,
            sample_paths: vec![],
            last_indexed_at: None,
            is_missing: true,
        }),
    };

    if let Ok(repo) = crate::git::repo::open_repo(path)
        && let Ok(files) = crate::git::status::get_repo_status(&repo)
    {
        let indexed_files = if let Ok(ref storage) = storage_res {
            storage.get_active_file_id_map().unwrap_or_default()
        } else {
            std::collections::HashMap::new()
        };

        let mut unindexed = 0;
        for file in &files {
            let rel_path = &file.path;
            if !indexed_files.contains_key(rel_path) {
                let ext = rel_path.extension().and_then(|s| s.to_str()).unwrap_or("");
                if matches!(ext, "rs" | "ts" | "tsx" | "js" | "jsx" | "py" | "go" | "md") {
                    unindexed += 1;
                }
            }
        }

        if unindexed > 0 {
            if let Some(ref mut w) = warning {
                w.unindexed_files = unindexed;
            } else {
                warning = Some(crate::index::staleness::StalenessWarning {
                    days_since_indexed: 0,
                    stale_files: 0,
                    unindexed_files: unindexed,
                    sample_paths: vec![],
                    last_indexed_at: None,
                    is_missing: false,
                });
            }
        }
    }

    if let Some(warning) = warning {
        if json {
            println!("{}", serde_json::to_string(&warning).unwrap_or_default());
        } else {
            crate::index::staleness::print_staleness_warning(&warning);
        }

        if warning.is_missing {
            if warning.unindexed_files > 0 {
                std::process::exit(1);
            }
        } else if strict && (warning.days_since_indexed > threshold || warning.unindexed_files > 0)
        {
            std::process::exit(1);
        }
    } else if json {
        println!(r#"{{"status": "fresh"}}"#);
    } else {
        println!("Index is fresh.");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::symbols::SymbolKind;
    use crate::semantic::chunker::AstChunk;

    fn chunk(name: &str) -> AstChunk {
        AstChunk {
            file_path: "src/lib.rs".to_string(),
            name: name.to_string(),
            kind: SymbolKind::Function,
            content: format!("fn {name}() {{}}"),
            docstring: None,
            range: (0, 0),
            lines: (1, 1),
            offset: 0,
        }
    }

    #[test]
    fn semantic_embedding_batches_preserve_order() {
        let chunks: Vec<AstChunk> = (0..10).map(|i| chunk(&format!("chunk_{i}"))).collect();

        let batches = semantic_embedding_batches(&chunks, 4);
        let flattened_names: Vec<&str> = batches
            .iter()
            .flat_map(|batch| batch.iter().map(|chunk| chunk.name.as_str()))
            .collect();

        assert_eq!(batches.len(), 3);
        assert_eq!(batches[0].len(), 4);
        assert_eq!(batches[1].len(), 4);
        assert_eq!(batches[2].len(), 2);
        assert_eq!(
            flattened_names,
            chunks
                .iter()
                .map(|chunk| chunk.name.as_str())
                .collect::<Vec<_>>()
        );
    }
}

fn execute_semantic_dry_run(
    layout: &Layout,
    config: &crate::config::model::Config,
    concurrency_override: Option<usize>,
    output_path: Option<std::path::PathBuf>,
) -> Result<()> {
    use comfy_table::Table;

    let cozo_path = layout.state_subdir().join("ledger.cozo");
    let cozo = if cozo_path.exists() {
        crate::state::storage_cozo::CozoStorage::new_read_only(cozo_path.as_std_path()).ok()
    } else {
        None
    };

    let resolved = resolve_semantic_concurrency(concurrency_override, config);

    let candidate_paths = walk_repo_for_semantic_files(layout.root.as_std_path());

    let mut total_lines = 0;
    for path in &candidate_paths {
        if let Ok(content) = std::fs::read_to_string(path) {
            total_lines += content.lines().count();
        }
    }
    let estimated_chunk_count = total_lines / 30;

    let current_vector_count = cozo
        .as_ref()
        .map(|db| {
            let relations = db.get_relations().unwrap_or_default();
            if !relations.contains(&"snippet_embedding".to_string()) {
                return 0;
            }
            let script = "?[count(file_path)] := *snippet_embedding{file_path}";
            if let Ok(res) = db.run_script(script)
                && let Some(row) = res.rows.first()
                && let Some(cozo::DataValue::Num(cozo::Num::Int(count))) = row.first()
            {
                *count as usize
            } else {
                0
            }
        })
        .unwrap_or(0);

    let current_file_count = cozo
        .as_ref()
        .map(|db| {
            let relations = db.get_relations().unwrap_or_default();
            if !relations.contains(&"semantic_file_hash".to_string()) {
                return 0;
            }
            let script = "?[file_path] := *semantic_file_hash{file_path}";
            db.run_script(script).map(|res| res.rows.len()).unwrap_or(0)
        })
        .unwrap_or(0);

    let hnsw_rebuild_threshold = config.semantic.hnsw_rebuild_threshold();
    let would_trigger_hnsw_rebuild = estimated_chunk_count > hnsw_rebuild_threshold;

    let embedding_dimensions = config.local_model.dimensions;

    let report = SemanticDryRunReport {
        parse_threads: resolved.parse_threads.get(),
        parse_source: resolved.parse_source.to_string(),
        embed_concurrency: resolved.embed_threads.get(),
        requested_embed_concurrency: resolved.requested_embed_threads.get(),
        embed_source: resolved.embed_source.to_string(),
        embed_concurrency_cap: resolved.embed_cap.get(),
        cap_source: resolved.cap_source.to_string(),
        candidate_file_count: candidate_paths.len(),
        estimated_chunk_count,
        embedding_model: config.local_model.embedding_model.clone(),
        embedding_dimensions,
        hnsw_rebuild_threshold,
        would_trigger_hnsw_rebuild,
        current_vector_count,
        current_file_count,
    };

    if let Some(path) = output_path {
        let json_str = serde_json::to_string_pretty(&report)
            .map_err(|e| miette::miette!("Failed to serialize dry-run report to JSON: {}", e))?;
        std::fs::write(&path, json_str).map_err(|e| {
            miette::miette!(
                "Failed to write dry-run report to {}: {}",
                path.display(),
                e
            )
        })?;
        println!("Dry-run report written to {}", path.display());
    } else {
        println!("Semantic Indexing Dry-Run Report");
        println!("=================================");
        let mut table = Table::new();
        table.set_header(vec!["Metric", "Value", "Source / Reason"]);
        table.add_row(vec![
            "Parse Threads",
            &report.parse_threads.to_string(),
            &report.parse_source,
        ]);
        table.add_row(vec![
            "Requested Embed Concurrency",
            &report.requested_embed_concurrency.to_string(),
            &report.embed_source,
        ]);
        table.add_row(vec![
            "Effective Embed Concurrency",
            &report.embed_concurrency.to_string(),
            "min(Requested Embed Concurrency, Embed Concurrency Cap)",
        ]);
        table.add_row(vec![
            "Embed Concurrency Cap",
            &report.embed_concurrency_cap.to_string(),
            &report.cap_source,
        ]);
        table.add_row(vec![
            "Candidate Files",
            &report.candidate_file_count.to_string(),
            "File walk of repository",
        ]);
        table.add_row(vec![
            "Estimated Chunks",
            &report.estimated_chunk_count.to_string(),
            "Lines count / 30 approximation",
        ]);
        table.add_row(vec![
            "Embedding Model",
            &report.embedding_model,
            "config.local_model.embedding_model",
        ]);
        let dims_str = if report.embedding_dimensions == 0 {
            "0 (probed at runtime)".to_string()
        } else {
            report.embedding_dimensions.to_string()
        };
        table.add_row(vec![
            "Embedding Dimensions",
            &dims_str,
            "config.local_model.dimensions",
        ]);
        table.add_row(vec![
            "HNSW Rebuild Threshold",
            &report.hnsw_rebuild_threshold.to_string(),
            "config.semantic.hnsw_rebuild_threshold",
        ]);
        table.add_row(vec![
            "Would Rebuild HNSW",
            &report.would_trigger_hnsw_rebuild.to_string(),
            "Estimated chunks > threshold",
        ]);
        table.add_row(vec![
            "Current Vectors in DB",
            &report.current_vector_count.to_string(),
            "CozoDB vector store",
        ]);
        table.add_row(vec![
            "Current Files in DB",
            &report.current_file_count.to_string(),
            "CozoDB vector store",
        ]);
        println!("{table}");
    }

    Ok(())
}

#[derive(serde::Serialize)]
struct SemanticDryRunReport {
    pub parse_threads: usize,
    pub parse_source: String,
    pub embed_concurrency: usize,
    pub requested_embed_concurrency: usize,
    pub embed_source: String,
    pub embed_concurrency_cap: usize,
    pub cap_source: String,
    pub candidate_file_count: usize,
    pub estimated_chunk_count: usize,
    pub embedding_model: String,
    pub embedding_dimensions: usize,
    pub hnsw_rebuild_threshold: usize,
    pub would_trigger_hnsw_rebuild: bool,
    pub current_vector_count: usize,
    pub current_file_count: usize,
}

fn execute_repair_metadata(
    layout: &Layout,
    storage: StorageManager,
    config: &crate::config::model::Config,
    dry_run: bool,
    yes: bool,
    json: bool,
) -> Result<()> {
    use crate::index::staleness::assess_index_freshness;

    let threshold = config.index.stale_threshold_days;
    let assessment = assess_index_freshness(&storage, threshold);

    let output = if json {
        serde_json::to_string(&assessment).into_diagnostic()?
    } else {
        format!("Current metadata state: {:?}", assessment.state)
    };

    if !json {
        println!("{}", output);
    }

    if dry_run {
        if !json {
            println!("Dry run mode. Would force full index and replace metadata if successful.");
        }
        return Ok(());
    }

    // Check gross future clock skew
    if assessment
        .warnings
        .iter()
        .any(|w| w.contains("Future timestamp detected"))
        && !json
    {
        println!(
            "WARNING: Material future clock skew detected. Please correct your system clock before repairing."
        );
    }

    if !yes && !crate::index::staleness::is_non_interactive() {
        let ans = inquire::Confirm::new(
            "Are you sure you want to repair metadata and rebuild the index?",
        )
        .with_default(false)
        .prompt()
        .unwrap_or(false);
        if !ans {
            if !json {
                println!("Aborted.");
            }
            return Ok(());
        }
    }

    if !json {
        println!("Repairing metadata (forcing full index pass)...");
    }

    let db_path = layout.state_subdir().join("ledger.db");
    let cozo_path = layout.state_subdir().join("ledger.cozo");
    let shadow_db_path = layout.state_subdir().join("ledger_shadow.db");
    let shadow_cozo_path = layout.state_subdir().join("ledger_shadow.cozo");

    // Ensure clean shadow state
    let _ = std::fs::remove_file(&shadow_db_path);
    let _ = std::fs::remove_dir_all(&shadow_cozo_path);
    let _ = std::fs::remove_file(&shadow_cozo_path);

    let shadow_storage = StorageManager::init(shadow_db_path.as_std_path())?;
    let mut indexer = ProjectIndexer::new(shadow_storage, layout.root.clone(), config.clone());

    match indexer.full_index() {
        Ok(_) => {
            // Drop indexer to release file locks before renaming
            drop(indexer);
            drop(storage);

            // Promote shadow DB
            std::fs::rename(&shadow_db_path, &db_path).into_diagnostic()?;
            if shadow_cozo_path.exists() {
                let _ = std::fs::remove_dir_all(&cozo_path);
                let _ = std::fs::remove_file(&cozo_path);
                std::fs::rename(&shadow_cozo_path, &cozo_path).into_diagnostic()?;
            }

            if !json {
                println!("Repair successful.");
            }
            Ok(())
        }
        Err(e) => {
            drop(indexer);
            let _ = std::fs::remove_file(&shadow_db_path);
            let _ = std::fs::remove_dir_all(&shadow_cozo_path);
            let _ = std::fs::remove_file(&shadow_cozo_path);
            if !json {
                eprintln!("Repair failed: {}. Rollback successful.", e);
            }
            Err(e)
        }
    }
}
