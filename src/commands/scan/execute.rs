use super::git::{files_changed_between, files_changed_since, parse_pr_range};
use super::validate::{
    validate_blast_depth_requires_impact, validate_mode_requires_impact, validate_scan_args,
};
use crate::cli::args::ScanImpactMode;
use crate::commands::scan_pr::{HistoryEnrichment, PrScanContext, PrScanReport};
use crate::config::load::load_config;
use crate::git::FileChange;
use crate::git::RepoSnapshot;
use crate::git::diff::get_diff_summary;
use crate::git::metadata::{DEFAULT_MAX_COMMITS, collect_path_history};
use crate::git::repo::{get_head_info, open_repo};
use crate::git::status::get_repo_status;
use crate::output::human::print_scan_summary;
use crate::output::table::{apply_table_style, resolve_table_style};
use crate::state::layout::Layout;
use crate::state::reports::{ScanDiffSummary, ScanReport};
use crate::state::storage::StorageManager;
use camino::Utf8Path;
use comfy_table::{Cell, Color, Table};
use globset::{Glob, GlobSetBuilder};
use miette::{IntoDiagnostic, Result};
use std::env;
use std::path::PathBuf;
use tracing::info;

/// Whether scan-report RO honesty may print on stdout (human only).
///
/// Machine paths (`--json` / `--out`) must not prefix stdout with honesty text
/// that would break pure-JSON parse (0174 review P1 / Codex P2).
pub(crate) fn should_print_scan_report_honesty(json: bool, has_out: bool) -> bool {
    !json && !has_out
}

/// Emit greppable scan-report RO honesty for human mode; log-only for machine.
fn emit_scan_report_ro_honesty(json: bool, has_out: bool) {
    if should_print_scan_report_honesty(json, has_out) {
        println!("{}", crate::state::reports::SCAN_REPORT_RO_HONESTY);
    } else {
        tracing::warn!("{}", crate::state::reports::SCAN_REPORT_RO_HONESTY);
    }
}

/// Patterns that identify observability configuration files whose changes
/// should trigger automatic graph analysis in `scan --impact`.
const OBSERVABILITY_CONFIG_PATTERNS: &[&str] = &[
    "**/OpenSLO.yaml",
    "**/OpenSLO.yml",
    "**/*.openslo.yaml",
    "**/*.openslo.yml",
    "**/observability/*.yaml",
    "**/observability/*.yml",
    "**/otel-collector.yaml",
    "**/otel-collector.yml",
    "**/prometheus.yml",
    "**/prometheus.yaml",
    "**/jaeger*.yaml",
    "**/jaeger*.yml",
    "**/datadog*.yaml",
    "**/datadog*.yml",
];

/// Compile the observability config glob set. Invalid patterns are ignored and
/// logged, matching the permissive behavior of `coverage::traces`.
fn observability_config_glob_set() -> Option<globset::GlobSet> {
    let mut builder = GlobSetBuilder::new();
    for pattern in OBSERVABILITY_CONFIG_PATTERNS {
        match Glob::new(pattern) {
            Ok(glob) => {
                builder.add(glob);
            }
            Err(e) => {
                tracing::warn!(
                    "Invalid observability config glob pattern '{}': {}",
                    pattern,
                    e
                );
            }
        }
    }
    match builder.build() {
        Ok(set) => Some(set),
        Err(e) => {
            tracing::warn!("Failed to build observability config glob set: {}", e);
            None
        }
    }
}

/// Returns `true` if any changed path matches a known observability config
/// pattern.
pub(super) fn changes_include_observability_config(changes: &[FileChange]) -> bool {
    let Some(set) = observability_config_glob_set() else {
        return false;
    };
    changes.iter().any(|change| {
        let path_str = change.path.to_string_lossy().replace('\\', "/");
        set.is_match(&path_str)
    })
}

/// Check whether the CozoDB knowledge graph is missing or stale for the current
/// repository state. `check_index_staleness(...).is_some()` includes
/// `NeverIndexed`, `StaleEmpty`, and `StalePopulated` — not only never-indexed.
pub(super) fn graph_is_missing_or_stale(storage: &StorageManager, threshold_days: u64) -> bool {
    crate::index::staleness::check_index_staleness(storage, threshold_days).is_some()
}

/// Run automatic graph analysis when an observability config file changed and
/// the graph is missing/stale. This prevents empty-state errors in
/// `observability diff` without requiring a manual `index --analyze-graph`.
///
/// Opens write storage **once** when auto-graph is eligible (obs-config in
/// changes and `ledger.db` already exists). Returns that handle so silent
/// impact can reuse it (`into_storage`) instead of a third `init_with_layout`.
/// Missing db / uninitialized trees skip without creating `ledger.db`.
pub(super) fn maybe_auto_analyze_graph(
    changes: &[FileChange],
    project_root: &std::path::Path,
    config: &crate::config::model::Config,
    layout: &Layout,
) -> Result<Option<StorageManager>> {
    if changes.is_empty() || !changes_include_observability_config(changes) {
        return Ok(None);
    }
    // Do not create ledger.db as a side effect of auto-graph.
    if !layout.state_subdir().join("ledger.db").exists() {
        tracing::debug!("Skipping observability auto-analysis: storage not initialized yet");
        return Ok(None);
    }

    let write_storage = StorageManager::init_with_layout(layout)?;
    if !graph_is_missing_or_stale(&write_storage, config.index.stale_threshold_days) {
        return Ok(Some(write_storage));
    }

    info!(
        "Auto-triggering graph analysis: observability config changed and graph is missing/stale"
    );

    let utf8_repo_path = match camino::Utf8PathBuf::from_path_buf(project_root.to_path_buf()) {
        Ok(p) => p,
        Err(_) => {
            return Err(miette::miette!("Repository root is not valid UTF-8"));
        }
    };
    let mut indexer =
        crate::index::ProjectIndexer::new(write_storage, utf8_repo_path, config.clone());

    crate::index::run_graph_analysis(
        &mut indexer,
        crate::index::SqliteExtractPolicy::Run,
        false,
        false,
        false,
        None,
        Some(layout),
    )?;
    Ok(Some(indexer.into_storage()))
}

/// Emit gitScan envelope (0180-D): `--out` → file only (no stdout); else pretty stdout.
fn emit_git_scan_json(report: &ScanReport, out: Option<&PathBuf>) -> Result<()> {
    use crate::state::reports::ScanGitJson;
    let envelope = ScanGitJson::from_report(report);
    let json_output = serde_json::to_string_pretty(&envelope).into_diagnostic()?;
    if let Some(path) = out {
        std::fs::write(path, json_output).into_diagnostic()?;
    } else {
        println!("{json_output}");
    }
    Ok(())
}

/// Emit impact JSON/human for in-memory paths (prospective / docs mode).
/// Never claims `latest-impact.json` was written.
fn emit_scan_impact_in_memory(
    impact_packet: &crate::impact::packet::ImpactPacket,
    write_impact_json: bool,
    out: Option<PathBuf>,
    summary: bool,
    full: bool,
    prospective: bool,
) -> Result<()> {
    if write_impact_json {
        let json_output = serde_json::to_string_pretty(impact_packet).into_diagnostic()?;
        if let Some(path) = out {
            std::fs::write(&path, json_output).into_diagnostic()?;
        } else {
            println!("{}", json_output);
        }
    } else if summary {
        crate::output::human::print_impact_brief(impact_packet);
        if prospective {
            println!(
                "\nProspective analysis (in-memory only — did not rewrite latest-impact.json)"
            );
        }
    } else {
        crate::output::human::print_impact_summary_with_full(impact_packet, full);
        if prospective {
            println!(
                "\nProspective analysis (in-memory only — did not rewrite latest-impact.json)"
            );
        }
    }
    Ok(())
}

/// Scan entrypoint (default blast depth from config).
pub fn execute_scan(
    run_impact: bool,
    summary: bool,
    json: bool,
    out: Option<PathBuf>,
    base_ref: Option<String>,
    pr: Option<String>,
    format: Option<String>,
) -> Result<()> {
    execute_scan_with_opts(
        run_impact,
        summary,
        json,
        out,
        base_ref,
        pr,
        format,
        None,
        Vec::new(),
        false,
        None,
        false,
    )
}

/// Scan entrypoint with optional CLI `--blast-depth` (DoD-9 dual surface).
#[allow(clippy::too_many_arguments)]
pub fn execute_scan_with_blast_depth(
    run_impact: bool,
    summary: bool,
    json: bool,
    out: Option<PathBuf>,
    base_ref: Option<String>,
    pr: Option<String>,
    format: Option<String>,
    blast_depth: Option<u32>,
) -> Result<()> {
    execute_scan_with_opts(
        run_impact,
        summary,
        json,
        out,
        base_ref,
        pr,
        format,
        blast_depth,
        Vec::new(),
        false,
        None,
        false,
    )
}

/// Scan entrypoint with 0173 `--paths` / `--include-governance` and 0227 `--mode`.
#[allow(clippy::too_many_arguments)]
pub fn execute_scan_with_opts(
    run_impact: bool,
    summary: bool,
    json: bool,
    out: Option<PathBuf>,
    base_ref: Option<String>,
    pr: Option<String>,
    format: Option<String>,
    blast_depth: Option<u32>,
    paths: Vec<String>,
    include_governance: bool,
    mode: Option<ScanImpactMode>,
    full: bool,
) -> Result<()> {
    let current_dir = env::current_dir()
        .map_err(|e| miette::miette!("Failed to get current directory: {}", e))?;

    validate_scan_args(&pr, &base_ref, &format, run_impact, summary, json, &out)?;
    validate_blast_depth_requires_impact(run_impact, &pr, blast_depth)?;
    validate_mode_requires_impact(run_impact, mode)?;

    if !paths.is_empty() {
        if !run_impact {
            return Err(miette::miette!("--paths requires --impact"));
        }
        if base_ref.is_some() {
            return Err(miette::miette!(
                "--paths and --base-ref are mutually exclusive"
            ));
        }
        if pr.is_some() {
            return Err(miette::miette!("--paths and --pr are mutually exclusive"));
        }
    }

    // open_repo first so no-repo errors keep the stable discover message
    // (MCP test_scan_no_repo / CLI). Then layout.root for repo-root path
    // resolution when invoked from a subdirectory.
    let repo = open_repo(&current_dir)?;
    let (head_hash, branch_name) = get_head_info(&repo)?;
    let layout = crate::commands::helpers::get_layout()?;
    let config = load_config(&layout).unwrap_or_default();
    let project_root = layout.root.as_std_path();
    let work_dir = if project_root.exists() {
        project_root
    } else {
        current_dir.as_path()
    };

    let prospective = !paths.is_empty();
    let prospective_parsed = if prospective {
        Some(crate::commands::impact::parse_prospective_paths(&paths)?)
    } else {
        None
    };

    let (changes, is_clean, pr_base_ref, pr_head_ref) = if let Some(ref range) = pr {
        let (base, head, git_range) = parse_pr_range(range)?;
        let all_changes = files_changed_between(work_dir, &git_range, &base)?;
        let filtered = crate::git::ignore::filter_ignored_changes(
            all_changes,
            &config.watch.ignore_patterns,
            run_impact,
        )?;
        let clean = filtered.is_empty();
        (filtered, clean, Some(base), Some(head))
    } else if let Some(ref ref_str) = base_ref {
        let all_changes = files_changed_since(work_dir, ref_str)?;
        let filtered = crate::git::ignore::filter_ignored_changes(
            all_changes,
            &config.watch.ignore_patterns,
            run_impact,
        )?;
        let clean = filtered.is_empty();
        (filtered, clean, None, None)
    } else if let Some(ref parsed) = prospective_parsed {
        let snap = crate::commands::impact::build_prospective_snapshot(work_dir, parsed)?;
        (snap.changes, false, None, None)
    } else {
        let all_changes = get_repo_status(&repo)?;
        let filtered = crate::git::ignore::filter_ignored_changes(
            all_changes,
            &config.watch.ignore_patterns,
            run_impact,
        )?;
        let clean = filtered.is_empty();
        (filtered, clean, None, None)
    };

    let snapshot = RepoSnapshot {
        head_hash,
        branch_name,
        is_clean,
        changes,
    };

    // PR path is intentionally index-free (0115 DoD-5): never create `.ledgerful`
    // via write_scan_report / tombstone. Soft-open for testGaps is existence-check only.
    // Prospective (--paths): also skip durable scan report write (0173-G — no
    // hypothetical clobber of latest-scan.json).
    let mut durable_scan_report: Option<ScanReport> = None;
    if pr.is_none() && !prospective {
        // Working-tree diffs are empty when --base-ref is used; skip get_diff_summary.
        let mut diff_summaries = if base_ref.is_some() {
            vec![]
        } else {
            snapshot
                .changes
                .iter()
                .filter_map(|change| {
                    get_diff_summary(&repo, &change.path).map(|summary| ScanDiffSummary {
                        path: change.path.to_string_lossy().to_string(),
                        summary,
                    })
                })
                .collect::<Vec<_>>()
        };
        diff_summaries.sort_by(|a, b| a.path.cmp(&b.path));

        let scan_report = ScanReport::from_snapshot(&snapshot, diff_summaries);
        // Soft-degrade report writes under RO-class fail (0174-E) — no hard-fail.
        let scan_written = crate::state::reports::soft_write_scan_report(&layout, &scan_report)?;
        // Honesty: human only — never prefix machine stdout for --json / --out
        // (0174 review P1; impact puts honesty in analysis_warnings instead).
        if !scan_written {
            emit_scan_report_ro_honesty(json, out.is_some());
        }

        if !run_impact && snapshot.is_clean {
            let tomb_ok = crate::state::reports::soft_write_clean_tree_tombstone(
                &layout,
                snapshot.head_hash.clone(),
                snapshot.branch_name.clone(),
            )?;
            if !tomb_ok && scan_written {
                // Avoid duplicate honesty if scan report already printed it.
                emit_scan_report_ro_honesty(json, out.is_some());
            }
        }
        durable_scan_report = Some(scan_report);
    }

    // 0180: bare scan --json / --out → gitScan envelope (not auto-impact). Early
    // return avoids human summary and all impact/storage work (AI1 P2-3).
    if !run_impact && pr.is_none() && (json || out.is_some()) {
        let report =
            durable_scan_report.unwrap_or_else(|| ScanReport::from_snapshot(&snapshot, vec![]));
        emit_git_scan_json(&report, out.as_ref())?;
        return Ok(());
    }

    // write_impact_json is only impact/PR-reachable after the non-impact machine
    // early return above (0180-C).
    let write_impact_json = json || out.is_some();

    // PR-mode output: either JSON report or human summary.
    if let (Some(base), Some(head)) = (pr_base_ref, pr_head_ref) {
        // Index-free history enrichment (schema v2): churn + recency from a
        // bounded first-parent walk. No author names; see git::metadata docs.
        let history = match Utf8Path::from_path(&current_dir) {
            Some(root) => match collect_path_history(root, DEFAULT_MAX_COMMITS) {
                Ok(result) => HistoryEnrichment::from_path_history(result),
                Err(e) => {
                    tracing::warn!("PR history enrichment failed; emitting empty history: {e}");
                    HistoryEnrichment::empty()
                }
            },
            None => {
                tracing::warn!(
                    "PR history enrichment skipped: current_dir is not valid UTF-8: {}",
                    current_dir.display()
                );
                HistoryEnrichment::empty()
            }
        };
        // Soft-open test_gaps + affected_flows: existence-check only; never
        // init_with_layout (0115 / 0118).
        let test_gaps = compute_pr_scan_test_gaps(&layout, &snapshot);
        let affected_flows = compute_pr_scan_affected_flows(&layout, &snapshot);
        let report = PrScanReport::new_with_test_gaps(
            PrScanContext {
                base_ref: base,
                head_ref: head,
                head_hash: snapshot.head_hash.clone(),
                branch_name: snapshot.branch_name.clone(),
                tree_clean: snapshot.is_clean,
            },
            &snapshot.changes,
            &[], // analysisWarnings reserved — always empty (0086)
            &history,
            test_gaps,
            affected_flows,
        );

        if format.as_deref() == Some("json") {
            let json_output = serde_json::to_string_pretty(&report).into_diagnostic()?;
            if let Some(path) = out {
                std::fs::write(&path, json_output).into_diagnostic()?;
            } else {
                println!("{}", json_output);
            }
        } else {
            if report.test_gaps.unmapped_count > 0 {
                eprintln!(
                    "warning: {} changed source path(s) lack structural test mapping (see testGaps)",
                    report.test_gaps.unmapped_count
                );
            }
            print_pr_scan_summary(&report);
        }
        return Ok(());
    }

    if !write_impact_json {
        print_scan_summary(&snapshot);
    }

    if run_impact {
        // Auto-trigger graph analysis when observability config files changed
        // and the graph is missing/stale, so `observability diff` can populate
        // correctly without a manual `index --analyze-graph`. Guarded by a
        // non-empty changes list so a clean tree (or a repo with no
        // `.ledgerful` state yet) never pays the storage-open cost or fails
        // just because state has not been initialized. Storage open errors are
        // treated as "skip auto-analysis" rather than aborting the scan: the
        // impact path below handles uninitialized state on its own terms, and
        // auto-analysis is strictly an optimization for the observability-diff
        // empty-state case.
        let auto_graph_storage = if !snapshot.changes.is_empty() {
            maybe_auto_analyze_graph(&snapshot.changes, &current_dir, &config, &layout)?
        } else {
            None
        };

        // Always use the snapshot derived above so that --base-ref / --paths
        // changes are passed through regardless of whether --json / --out is set.
        // Thread --blast-depth so scan --impact matches impact CLI (DoD-9).
        // Prospective (--paths) and docs-mode (explicit or auto-detect) are
        // in-memory only — no latest-impact.json clobber (0173-G / 0221 / 0227).
        let docs_mode = crate::impact::lead::docs_mode_active(
            matches!(mode, Some(ScanImpactMode::Docs)),
            snapshot.changes.iter().map(|c| c.path.to_string_lossy()),
        );
        if prospective || docs_mode {
            // Prospective / docs-mode skip write_scan_report, which is what
            // normally creates `.ledgerful/state` before sqlite open.
            layout.ensure_state_dir()?;
            let mut config = load_config(&layout).unwrap_or_default();
            let depth_note = crate::impact::enrichment::blast::apply_cli_blast_depth(
                &mut config.impact.blast_depth,
                config.impact.blast_depth_max,
                blast_depth,
            );
            let storage = match auto_graph_storage {
                Some(s) => s,
                None => crate::commands::impact::open_storage_for_impact(&layout)?,
            };
            let mut impact_packet = if prospective {
                let parsed = prospective_parsed
                    .clone()
                    .ok_or_else(|| miette::miette!("internal: prospective paths missing"))?;
                let snap = crate::commands::impact::build_prospective_snapshot(work_dir, &parsed)?;
                crate::commands::impact::compute_impact_from_snapshot_in_memory_with_mode(
                    &storage,
                    &config,
                    work_dir,
                    snap,
                    include_governance,
                    "prospective",
                    parsed,
                )?
            } else {
                let analysis_mode = if base_ref.is_some() {
                    "base_ref"
                } else {
                    "working_tree"
                };
                crate::commands::impact::compute_impact_from_snapshot_in_memory_with_mode(
                    &storage,
                    &config,
                    work_dir,
                    snapshot,
                    include_governance,
                    analysis_mode,
                    Vec::new(),
                )?
            };
            if let Some(note) = depth_note {
                impact_packet.analysis_warnings.push(note);
                impact_packet.analysis_warnings.sort();
                impact_packet.analysis_warnings.dedup();
            }
            if docs_mode {
                crate::impact::lead::apply_docs_mode_presentation(&mut impact_packet);
            }
            let _ = storage.shutdown();
            emit_scan_impact_in_memory(
                &impact_packet,
                write_impact_json,
                out,
                summary,
                full,
                prospective,
            )?;
            return Ok(());
        }

        let (impact_packet, report_write_outcome) = if base_ref.is_some() {
            crate::commands::impact::execute_impact_silent_with_snapshot_opts_storage(
                snapshot,
                blast_depth,
                include_governance,
                "base_ref",
                auto_graph_storage,
            )?
        } else {
            crate::commands::impact::execute_impact_silent_with_depth_opts_storage(
                blast_depth,
                include_governance,
                auto_graph_storage,
            )?
        };

        if write_impact_json {
            let json_output = serde_json::to_string_pretty(&impact_packet).into_diagnostic()?;

            if let Some(path) = out {
                std::fs::write(&path, json_output).into_diagnostic()?;
            } else {
                println!("{}", json_output);
            }
        } else {
            crate::commands::impact::execute_impact_human(
                &impact_packet,
                summary,
                base_ref.is_some(),
                report_write_outcome,
            )?;
        }
    }

    Ok(())
}

/// Soft-open structural test gaps for PR scan (0115).
///
/// - Missing `ledger.db` → `unavailable` without creating any state.
/// - Open via `open_read_only_sqlite_only` only (never `init_with_layout`).
/// - File-level path only (no `resolve_seeds`).
pub(super) fn compute_pr_scan_test_gaps(
    layout: &Layout,
    snapshot: &RepoSnapshot,
) -> crate::impact::enrichment::test_gaps::TestGapsReport {
    use crate::impact::enrichment::test_gaps::{
        TestGapsOpts, TestGapsReport, compute_change_set_test_gaps_from_files,
    };

    let db_path = layout.state_subdir().join("ledger.db");
    if !db_path.exists() {
        return TestGapsReport::unavailable();
    }

    let storage = match StorageManager::open_read_only_sqlite_only(layout) {
        Ok(s) => s,
        Err(_) => return TestGapsReport::unavailable(),
    };
    let conn = storage.get_connection();
    let paths: Vec<String> = snapshot
        .changes
        .iter()
        .map(|c| c.path.to_string_lossy().replace('\\', "/"))
        .collect();
    let path_refs: Vec<&str> = paths.iter().map(|s| s.as_str()).collect();
    let opts = TestGapsOpts {
        head_hash: snapshot.head_hash.clone(),
    };
    compute_change_set_test_gaps_from_files(conn, &path_refs, &opts)
}

/// Soft-open affected HTTP flows for PR scan (0118).
///
/// - Missing `ledger.db` → `unavailable` without creating any state.
/// - Open via `open_read_only_sqlite_only` only (never `init_with_layout`).
/// - File-path seeds only (no symbol resolution / no blast on this path).
pub(super) fn compute_pr_scan_affected_flows(
    layout: &Layout,
    snapshot: &RepoSnapshot,
) -> crate::impact::enrichment::affected_flows::AffectedFlowsReport {
    use crate::git::ChangeType;
    use crate::impact::enrichment::affected_flows::{
        AffectedFlowsOpts, AffectedFlowsReport, compute_pr_affected_flows_soft,
    };
    use crate::impact::packet::{ChangedFile, FileAnalysisStatus};

    let db_path = layout.state_subdir().join("ledger.db");
    if !db_path.exists() {
        return AffectedFlowsReport::unavailable();
    }

    let storage = match StorageManager::open_read_only_sqlite_only(layout) {
        Ok(s) => s,
        Err(_) => return AffectedFlowsReport::unavailable(),
    };
    let conn = storage.get_connection();

    let changes: Vec<ChangedFile> = snapshot
        .changes
        .iter()
        .map(|c| {
            let (status, old_path) = match &c.change_type {
                ChangeType::Added => ("Added".to_string(), None),
                ChangeType::Modified => ("Modified".to_string(), None),
                ChangeType::Deleted => ("Deleted".to_string(), None),
                ChangeType::Renamed { old_path } => ("Renamed".to_string(), Some(old_path.clone())),
            };
            ChangedFile {
                path: c.path.clone(),
                status,
                old_path,
                is_staged: c.is_staged,
                symbols: None,
                imports: None,
                runtime_usage: None,
                analysis_status: FileAnalysisStatus::default(),
                analysis_warnings: Vec::new(),
                api_routes: Vec::new(),
                data_models: Vec::new(),
                ci_gates: Vec::new(),
            }
        })
        .collect();

    let opts = AffectedFlowsOpts {
        head_hash: snapshot.head_hash.clone(),
    };
    // No blast on PR soft path (index-free CI default; kinds 1–3 only).
    compute_pr_affected_flows_soft(Some(conn), &changes, None, &opts)
}

/// Human-readable summary for `scan --pr --format text`.
fn print_pr_scan_summary(report: &PrScanReport) {
    use owo_colors::{OwoColorize, Stream, Style};

    println!(
        "\n{}",
        "Ledgerful PR Scan Summary"
            .if_supports_color(Stream::Stdout, |s| s.style(Style::new().bold().underline()))
    );
    println!(
        "{:<15} {}",
        "Base:".if_supports_color(Stream::Stdout, |s| s.bold()),
        report.base_ref
    );
    println!(
        "{:<15} {}",
        "Head:".if_supports_color(Stream::Stdout, |s| s.bold()),
        report.head_ref
    );
    println!(
        "{:<15} {}",
        "HEAD commit:".if_supports_color(Stream::Stdout, |s| s.bold()),
        report.head_hash.as_deref().unwrap_or("<none>")
    );
    println!(
        "{:<15} {}",
        "Branch:".if_supports_color(Stream::Stdout, |s| s.bold()),
        report.branch_name.as_deref().unwrap_or("<none>")
    );
    println!(
        "{:<15} {}",
        "Working tree:".if_supports_color(Stream::Stdout, |s| s.bold()),
        match report.tree_clean {
            true => "CLEAN"
                .if_supports_color(Stream::Stdout, |s| s.green())
                .to_string(),
            false => "DIRTY"
                .if_supports_color(Stream::Stdout, |s| s.yellow())
                .to_string(),
        }
    );
    println!(
        "{:<15} {}",
        "Files changed:".if_supports_color(Stream::Stdout, |s| s.bold()),
        report.change_count
    );

    let risk_color = match report.risk_level {
        crate::commands::scan_pr::PrRiskLevel::Low => Color::Green,
        crate::commands::scan_pr::PrRiskLevel::Medium => Color::Yellow,
        crate::commands::scan_pr::PrRiskLevel::High => Color::Red,
    };
    let mut risk_table = Table::new();
    apply_table_style(&mut risk_table, resolve_table_style());
    risk_table.add_row(vec![
        Cell::new("PR RISK"),
        Cell::new(format!("{:?}", report.risk_level).to_uppercase()).fg(risk_color),
    ]);
    println!("{risk_table}");

    if !report.risk_reasons.is_empty() {
        println!(
            "{}",
            "Risk reasons:".if_supports_color(Stream::Stdout, |s| s.bold())
        );
        for reason in &report.risk_reasons {
            println!("  • {}", reason);
        }
    }

    if !report.analysis_warnings.is_empty() {
        println!(
            "{}",
            "Analysis warnings:".if_supports_color(Stream::Stdout, |s| s.bold())
        );
        for warning in &report.analysis_warnings {
            println!("  • {}", warning);
        }
    }

    println!(
        "{:<15} {} (unmapped={})",
        "Test gaps:".if_supports_color(Stream::Stdout, |s| s.bold()),
        report.test_gaps.status.as_str(),
        report.test_gaps.unmapped_count
    );
    println!(
        "{:<15} {} (flowCount={})",
        "Affected flows:".if_supports_color(Stream::Stdout, |s| s.bold()),
        report.affected_flows.status.as_str(),
        report.affected_flows.flow_count
    );

    if !report.changes.is_empty() {
        let mut table = Table::new();
        apply_table_style(&mut table, resolve_table_style());
        table.set_header(vec!["Action", "File Path"]);
        for change in &report.changes {
            let action = match change.change_type.as_str() {
                "added" => "Added"
                    .if_supports_color(Stream::Stdout, |s| s.green())
                    .to_string(),
                "modified" => "Modified"
                    .if_supports_color(Stream::Stdout, |s| s.yellow())
                    .to_string(),
                "deleted" => "Deleted"
                    .if_supports_color(Stream::Stdout, |s| s.red())
                    .to_string(),
                "renamed" => {
                    if let Some(old) = &change.old_path {
                        format!("Renamed ({} → {})", old, change.path)
                            .if_supports_color(Stream::Stdout, |s| s.blue())
                            .to_string()
                    } else {
                        "Renamed"
                            .if_supports_color(Stream::Stdout, |s| s.blue())
                            .to_string()
                    }
                }
                _ => change.change_type.clone(),
            };
            table.add_row(vec![Cell::new(action), Cell::new(&change.path)]);
        }
        println!("{table}");
    }
}
