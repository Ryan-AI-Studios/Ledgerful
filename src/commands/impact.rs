use crate::commands::helpers::get_layout;
use crate::config::load::load_config;
use crate::git::RepoSnapshot;
use crate::git::repo::{get_head_info, open_repo};
use crate::git::status::get_repo_status;
use crate::output::diagnostics::success_marker;
use crate::state::layout::Layout;
use crate::state::reports::{
    IMPACT_REPORT_RO_HONESTY, ImpactReportWriteOutcome, soft_write_impact_report,
};
use crate::state::storage::StorageManager;
use miette::Result;
use owo_colors::{OwoColorize, Stream};
use std::env;

/// Soft-open storage for impact analysis (0174).
///
/// **Write-first** so durable reports still write on a normal writable tree
/// (B7). When write-open fails and `ledger.db` already exists, fall back to
/// RO / sqlite-only RO so pure RO reviewers can still analyze (stdout-only).
///
/// Differs from change-context (which prefers RO always) because impact has a
/// durable report side effect that must succeed when the FS is writable.
pub(crate) fn open_storage_for_impact(layout: &Layout) -> Result<StorageManager> {
    match StorageManager::init_with_layout(layout) {
        Ok(s) => Ok(s),
        Err(write_err) => {
            let db_path = layout.state_subdir().join("ledger.db");
            if !db_path.exists() {
                return Err(write_err);
            }
            tracing::debug!("impact write-open failed; trying RO for analysis: {write_err}");
            match StorageManager::open_read_only(layout) {
                Ok(s) => Ok(s),
                Err(ro_err) => {
                    tracing::debug!("impact full RO open failed; trying sqlite-only RO: {ro_err}");
                    match StorageManager::open_read_only_sqlite_only(layout) {
                        Ok(s) => Ok(s),
                        Err(_sqlite_err) => Err(write_err),
                    }
                }
            }
        }
    }
}

/// Append greppable RO report-write honesty to the packet when write was skipped.
fn apply_report_skip_honesty(
    packet: &mut crate::impact::packet::ImpactPacket,
    outcome: ImpactReportWriteOutcome,
) {
    if outcome == ImpactReportWriteOutcome::Skipped {
        packet
            .analysis_warnings
            .push(IMPACT_REPORT_RO_HONESTY.to_string());
        packet.analysis_warnings.sort();
        packet.analysis_warnings.dedup();
    }
}

/// True when the durable report was successfully written or intentionally left
/// unchanged (not RO-skipped).
fn report_was_durable(outcome: ImpactReportWriteOutcome) -> bool {
    matches!(
        outcome,
        ImpactReportWriteOutcome::Written | ImpactReportWriteOutcome::Unchanged
    )
}

/// Run impact analysis using a pre-built `RepoSnapshot`.
///
/// Used by `execute_scan` when `--base-ref` is supplied: the caller has already
/// computed the changed file list via `git diff --name-only` and assembled the
/// snapshot; this function takes ownership and continues with the standard
/// enrichment pipeline.
pub fn execute_impact_silent_with_snapshot(
    snapshot: crate::git::RepoSnapshot,
) -> Result<(
    crate::impact::packet::ImpactPacket,
    ImpactReportWriteOutcome,
)> {
    execute_impact_silent_with_snapshot_and_depth(snapshot, None)
}

/// Like [`execute_impact_silent_with_snapshot`] with optional CLI `--blast-depth`.
pub fn execute_impact_silent_with_snapshot_and_depth(
    snapshot: crate::git::RepoSnapshot,
    blast_depth: Option<u32>,
) -> Result<(
    crate::impact::packet::ImpactPacket,
    ImpactReportWriteOutcome,
)> {
    execute_impact_silent_with_snapshot_opts(snapshot, blast_depth, false, "base_ref")
}

/// Snapshot silent path with 0173 pathMode / analysisMode.
pub fn execute_impact_silent_with_snapshot_opts(
    snapshot: crate::git::RepoSnapshot,
    blast_depth: Option<u32>,
    include_governance: bool,
    analysis_mode: &str,
) -> Result<(
    crate::impact::packet::ImpactPacket,
    ImpactReportWriteOutcome,
)> {
    let current_dir = env::current_dir()
        .map_err(|e| miette::miette!("Failed to get current directory: {}", e))?;

    let layout = get_layout()?;

    let mut packet = crate::impact::orchestrator::map_snapshot_to_packet(snapshot, &current_dir)?;
    apply_impact_honesty_fields(&mut packet, include_governance, analysis_mode, Vec::new());

    // Load main config for temporal analysis
    let mut config = load_config(&layout).unwrap_or_default();
    if let Some(note) = crate::impact::enrichment::blast::apply_cli_blast_depth(
        &mut config.impact.blast_depth,
        config.impact.blast_depth_max,
        blast_depth,
    ) {
        packet.analysis_warnings.push(note);
    }

    // Soft-open: prefer RO when ledger.db exists (0174).
    let storage = open_storage_for_impact(&layout)?;

    let orchestrator = crate::impact::orchestrator::ImpactOrchestrator::with_builtins();
    orchestrator.run(&mut packet, &storage, &config, &current_dir)?;

    // Post-processing: Finalize and Redact
    packet.finalize();
    crate::impact::redact::redact_secrets(&mut packet);

    // Save to ledger (already soft on RO / failure)
    if let Err(e) = storage.save_packet(&packet) {
        tracing::warn!("SQLite save failed: {e}");
    }

    // Soft-write report: RO / PermissionDenied → Skipped, not hard-fail (0174).
    let write_outcome = soft_write_impact_report(&layout, &packet, storage.is_read_only())?;
    apply_report_skip_honesty(&mut packet, write_outcome);

    storage.shutdown()?;

    Ok((packet, write_outcome))
}

pub fn execute_impact_silent() -> Result<(
    crate::impact::packet::ImpactPacket,
    ImpactReportWriteOutcome,
)> {
    execute_impact_silent_with_depth(None)
}

/// Like [`execute_impact_silent`] with optional CLI `--blast-depth`.
pub fn execute_impact_silent_with_depth(
    blast_depth: Option<u32>,
) -> Result<(
    crate::impact::packet::ImpactPacket,
    ImpactReportWriteOutcome,
)> {
    execute_impact_silent_with_depth_opts(blast_depth, false)
}

/// Working-tree silent path with 0173 `--include-governance`.
pub fn execute_impact_silent_with_depth_opts(
    blast_depth: Option<u32>,
    include_governance: bool,
) -> Result<(
    crate::impact::packet::ImpactPacket,
    ImpactReportWriteOutcome,
)> {
    let current_dir = env::current_dir()
        .map_err(|e| miette::miette!("Failed to get current directory: {}", e))?;

    let repo = open_repo(&current_dir)?;
    let (head_hash, branch_name) = get_head_info(&repo)?;
    let layout = get_layout()?;

    // Filter changes against config ignore_patterns
    let config = load_config(&layout).unwrap_or_else(|_| crate::config::model::Config::default());
    let all_changes = get_repo_status(&repo)?;
    let changes = crate::git::ignore::filter_ignored_changes(
        all_changes,
        &config.watch.ignore_patterns,
        true,
    )?;

    let is_clean = changes.is_empty();

    let snapshot = RepoSnapshot {
        head_hash,
        branch_name,
        is_clean,
        changes,
    };

    let mut packet = crate::impact::orchestrator::map_snapshot_to_packet(snapshot, &current_dir)?;
    apply_impact_honesty_fields(&mut packet, include_governance, "working_tree", Vec::new());

    // Load main config for temporal analysis
    let mut config = load_config(&layout).unwrap_or_default();
    if let Some(note) = crate::impact::enrichment::blast::apply_cli_blast_depth(
        &mut config.impact.blast_depth,
        config.impact.blast_depth_max,
        blast_depth,
    ) {
        packet.analysis_warnings.push(note);
    }

    // Soft-open: prefer RO when ledger.db exists (0174).
    let storage = open_storage_for_impact(&layout)?;

    let orchestrator = crate::impact::orchestrator::ImpactOrchestrator::with_builtins();
    orchestrator.run(&mut packet, &storage, &config, &current_dir)?;

    // Post-processing: Finalize and Redact
    packet.finalize();
    crate::impact::redact::redact_secrets(&mut packet);

    // Save to ledger (already soft on RO / failure)
    if let Err(e) = storage.save_packet(&packet) {
        tracing::warn!("SQLite save failed: {e}");
    }

    // Soft-write report: RO / PermissionDenied → Skipped, not hard-fail (0174).
    let write_outcome = soft_write_impact_report(&layout, &packet, storage.is_read_only())?;
    apply_report_skip_honesty(&mut packet, write_outcome);

    storage.shutdown()?;

    Ok((packet, write_outcome))
}

/// Compute a fresh `ImpactPacket` in-memory without persisting it.
///
/// This is the DX6 auto-scan path used by `ledgerful ask --auto-scan`
/// (and by `[ask].auto_scan_default = true`). It mirrors
/// `execute_impact_silent`'s pipeline (snapshot → `map_snapshot_to_packet` →
/// orchestrator enrichment → finalize → redact) but deliberately skips the
/// two side effects that make the silent helpers "not-quite-in-memory":
///
/// - `storage.save_packet` (SQLite `snapshots` insert)
/// - `write_impact_report` (`.ledgerful/reports/latest-impact.json`)
///
/// so the cached/stored packet and report are left untouched. The caller
/// (`ask`) feeds the returned packet directly into its RAG context and
/// suppresses the stale-impact warning, since the packet reflects the live
/// working tree by construction.
///
/// The caller's existing `StorageManager` is reused (rather than opening a
/// second SQLite handle) to avoid Windows file-lock contention. Note the
/// orchestrator's enrichment providers write to the CozoDB knowledge graph
/// during enrichment (same side effect as `execute_impact_silent`); the DX6
/// non-persistence contract is scoped to the impact packet and the
/// `latest-impact.json` report, which this path does not touch.
///
/// Delegates to [`compute_impact_in_memory_at`] with `env::current_dir()` as
/// `project_root`. Callers that have already resolved the repo workdir (e.g.
/// `deploy impact` from a subdirectory) should call `_at` directly so deploy
/// manifest detection resolves root-relative paths against the repo root.
pub fn compute_impact_in_memory(
    storage: &crate::state::storage::StorageManager,
    config: &crate::config::model::Config,
) -> Result<crate::impact::packet::ImpactPacket> {
    let current_dir = env::current_dir()
        .map_err(|e| miette::miette!("Failed to get current directory: {}", e))?;
    compute_impact_in_memory_at(storage, config, &current_dir)
}

/// Repo-root-aware variant of [`compute_impact_in_memory`].
///
/// Used by callers that have already resolved the repository working directory
/// (e.g. `ledgerful deploy impact` invoked from a subdirectory, where
/// `env::current_dir()` is the subdir but the repo root is the parent). The
/// `project_root` argument is used consistently for git discovery
/// (`open_repo`), snapshot-to-packet mapping (`map_snapshot_to_packet`), and
/// orchestrator enrichment (`orchestrator.run`), so deploy manifest detection
/// — which does `project_root.join(&file.path)` and reads root-relative paths
/// like `docker-compose.yml` — resolves against the true repo root instead of
/// the current directory. [`compute_impact_in_memory`] is the CWD-based
/// convenience wrapper that delegates here with `env::current_dir()`, preserved
/// for the DX6 `ask` callers whose signature must not change.
pub fn compute_impact_in_memory_at(
    storage: &crate::state::storage::StorageManager,
    config: &crate::config::model::Config,
    project_root: &std::path::Path,
) -> Result<crate::impact::packet::ImpactPacket> {
    let repo = open_repo(project_root)?;
    let (head_hash, branch_name) = get_head_info(&repo)?;

    // Filter changes against config ignore_patterns (consistent with scan/impact).
    let all_changes = get_repo_status(&repo)?;
    let changes = crate::git::ignore::filter_ignored_changes(
        all_changes,
        &config.watch.ignore_patterns,
        true,
    )?;

    let is_clean = changes.is_empty();

    let snapshot = RepoSnapshot {
        head_hash,
        branch_name,
        is_clean,
        changes,
    };

    compute_impact_from_snapshot_in_memory(storage, config, project_root, snapshot)
}

/// In-memory impact from a pre-built [`RepoSnapshot`] (e.g. `--base-ref` diff).
///
/// Mirrors [`compute_impact_in_memory_at`]'s enrich → finalize → redact path
/// and **never** calls `save_packet` or `write_impact_report`. Used by
/// `change-context` so base-ref structure can time-travel without clobbering
/// `latest-impact.json`.
///
/// Defaults: `pathMode=code`, `analysisMode=working_tree`. Prefer
/// [`compute_impact_from_snapshot_in_memory_with_mode`] when flags are known.
pub fn compute_impact_from_snapshot_in_memory(
    storage: &crate::state::storage::StorageManager,
    config: &crate::config::model::Config,
    project_root: &std::path::Path,
    snapshot: RepoSnapshot,
) -> Result<crate::impact::packet::ImpactPacket> {
    compute_impact_from_snapshot_in_memory_with_mode(
        storage,
        config,
        project_root,
        snapshot,
        false,
        "working_tree",
        Vec::new(),
    )
}

/// In-memory impact with explicit path/analysis mode (0173).
///
/// Sets `path_mode` / `analysis_mode` / `prospective_paths` **before** enrichment
/// and risk analysis so temporal demotion matches the agent path mode.
/// **Never** writes `latest-impact.json` or `save_packet`.
pub fn compute_impact_from_snapshot_in_memory_with_mode(
    storage: &crate::state::storage::StorageManager,
    config: &crate::config::model::Config,
    project_root: &std::path::Path,
    snapshot: RepoSnapshot,
    include_governance: bool,
    analysis_mode: &str,
    prospective_paths: Vec<String>,
) -> Result<crate::impact::packet::ImpactPacket> {
    let mut packet = crate::impact::orchestrator::map_snapshot_to_packet(snapshot, project_root)?;
    apply_impact_honesty_fields(
        &mut packet,
        include_governance,
        analysis_mode,
        prospective_paths,
    );

    let orchestrator = crate::impact::orchestrator::ImpactOrchestrator::with_builtins();
    orchestrator.run(&mut packet, storage, config, project_root)?;

    packet.finalize();
    crate::impact::redact::redact_secrets(&mut packet);

    Ok(packet)
}

/// Apply 0173 honesty fields before analysis (pathMode / analysisMode / prospectivePaths).
pub fn apply_impact_honesty_fields(
    packet: &mut crate::impact::packet::ImpactPacket,
    include_governance: bool,
    analysis_mode: &str,
    prospective_paths: Vec<String>,
) {
    packet.path_mode =
        crate::impact::path_class::path_mode_from_include_governance(include_governance)
            .to_string();
    packet.analysis_mode = analysis_mode.to_string();
    packet.prospective_paths = prospective_paths;
}

/// Max prospective `--paths` entries (hard cap).
pub const MAX_PROSPECTIVE_PATHS: usize = 50;

/// Normalize and validate prospective CLI/MCP paths.
///
/// Empty/whitespace-only input → error. Cap ≤ [`MAX_PROSPECTIVE_PATHS`].
/// Paths are normalized to `/`, sorted, and deduped for determinism.
pub fn parse_prospective_paths(raw: &[String]) -> Result<Vec<String>> {
    let mut out: Vec<String> = raw
        .iter()
        .map(|p| crate::impact::path_class::normalize_path(p.trim()))
        .filter(|p| !p.is_empty())
        .collect();
    if out.is_empty() {
        return Err(miette::miette!(
            "--paths requires at least one non-empty path (comma- or repeat-separated)"
        ));
    }
    // Dedup before cap so duplicate inputs do not trip the unique-path limit.
    out.sort();
    out.dedup();
    if out.len() > MAX_PROSPECTIVE_PATHS {
        return Err(miette::miette!(
            "--paths accepts at most {MAX_PROSPECTIVE_PATHS} unique paths (got {})",
            out.len()
        ));
    }
    Ok(out)
}

/// Build a synthetic [`RepoSnapshot`] for prospective `--paths` analysis (0173).
///
/// - Disk hit → `Modified`; missing → `Added` (greenfield; no hard-fail).
/// - `is_clean = false` always (bypass 0147 empty-tree short-circuit).
/// - Changes non-empty when paths non-empty.
pub fn build_prospective_snapshot(
    project_root: &std::path::Path,
    paths: &[String],
) -> Result<RepoSnapshot> {
    use crate::git::{ChangeType, FileChange};

    let repo = open_repo(project_root)?;
    let (head_hash, branch_name) = get_head_info(&repo)?;

    let mut changes: Vec<FileChange> = Vec::with_capacity(paths.len());
    for p in paths {
        let norm = crate::impact::path_class::normalize_path(p);
        let full = project_root.join(&norm);
        let change_type = if full.is_file() || full.is_dir() {
            ChangeType::Modified
        } else {
            ChangeType::Added
        };
        changes.push(FileChange {
            path: std::path::PathBuf::from(norm),
            change_type,
            is_staged: false,
        });
    }
    changes.sort();

    Ok(RepoSnapshot {
        head_hash,
        branch_name,
        is_clean: false,
        changes,
    })
}

/// Render human-readable output for a pre-computed `ImpactPacket`.
///
/// Used by `execute_scan` so that the `--base-ref` snapshot flows through to
/// the human output path without re-deriving changes from working-tree status.
///
/// `report_write_outcome` controls clean-tree wording honesty (0147): "refreshed"
/// only when `latest-impact.json` was actually rewritten. Skipped (0174 RO)
/// never claims write/refresh and prints greppable honesty.
pub fn execute_impact_human(
    packet: &crate::impact::packet::ImpactPacket,
    summary: bool,
    base_ref_mode: bool,
    report_write_outcome: ImpactReportWriteOutcome,
) -> Result<()> {
    use crate::output::diagnostics::success_marker;
    use owo_colors::{OwoColorize, Stream};

    if packet.tree_clean && packet.changes.is_empty() {
        if base_ref_mode {
            println!("\n{} No changes detected vs base ref.", success_marker());
            println!("  All files between base ref and HEAD are clean.");
        } else {
            match report_write_outcome {
                ImpactReportWriteOutcome::Written => {
                    println!(
                        "\n{} Working tree is clean — impact report refreshed.",
                        success_marker()
                    );
                }
                ImpactReportWriteOutcome::Unchanged => {
                    println!("\n{} Working tree is clean.", success_marker());
                }
                ImpactReportWriteOutcome::Skipped => {
                    println!("\n{} Working tree is clean.", success_marker());
                    println!("{IMPACT_REPORT_RO_HONESTY}");
                }
            }
        }
        return Ok(());
    }

    if summary {
        crate::output::human::print_impact_brief(packet);
    } else {
        crate::output::human::print_impact_summary(packet);
    }

    if report_was_durable(report_write_outcome) {
        println!(
            "\n{} Wrote impact report to {}",
            success_marker(),
            ".ledgerful/reports/latest-impact.json".if_supports_color(Stream::Stdout, |s| s.cyan())
        );
    } else {
        println!("\n{IMPACT_REPORT_RO_HONESTY}");
    }

    Ok(())
}

/// Impact entrypoint (default blast depth from config).
pub fn execute_impact(
    all_parents: bool,
    summary: bool,
    telemetry_coverage: bool,
    dead_code: bool,
    json: bool,
    out: Option<std::path::PathBuf>,
) -> Result<()> {
    execute_impact_with_opts(
        all_parents,
        summary,
        telemetry_coverage,
        dead_code,
        json,
        out,
        None,
        Vec::new(),
        false,
    )
}

/// Impact entrypoint with optional CLI `--blast-depth` (DoD-9 dual surface).
pub fn execute_impact_with_blast_depth(
    all_parents: bool,
    summary: bool,
    telemetry_coverage: bool,
    dead_code: bool,
    json: bool,
    out: Option<std::path::PathBuf>,
    blast_depth: Option<u32>,
) -> Result<()> {
    execute_impact_with_opts(
        all_parents,
        summary,
        telemetry_coverage,
        dead_code,
        json,
        out,
        blast_depth,
        Vec::new(),
        false,
    )
}

/// Impact entrypoint with 0173 `--paths` / `--include-governance`.
#[allow(clippy::too_many_arguments)]
pub fn execute_impact_with_opts(
    all_parents: bool,
    summary: bool,
    _telemetry_coverage: bool,
    dead_code: bool,
    json: bool,
    out: Option<std::path::PathBuf>,
    blast_depth: Option<u32>,
    paths: Vec<String>,
    include_governance: bool,
) -> Result<()> {
    let current_dir = env::current_dir()
        .map_err(|e| miette::miette!("Failed to get current directory: {}", e))?;

    let layout = get_layout()?;
    let mut config =
        load_config(&layout).unwrap_or_else(|_| crate::config::model::Config::default());

    if all_parents {
        config.temporal.all_parents = true;
    }
    if dead_code {
        config.dead_code.enabled = true;
    }

    // Prefer layout root so `--paths` resolve against the repo root even when
    // invoked from a subdirectory (parity with change-context).
    let project_root = layout.root.as_std_path();
    let work_dir = if project_root.exists() {
        project_root
    } else {
        current_dir.as_path()
    };

    let prospective = !paths.is_empty();
    let (snapshot, analysis_mode, prospective_paths) = if prospective {
        let parsed = parse_prospective_paths(&paths)?;
        let snap = build_prospective_snapshot(work_dir, &parsed)?;
        (snap, "prospective", parsed)
    } else {
        let repo = open_repo(work_dir)?;
        let (head_hash, branch_name) = get_head_info(&repo)?;
        let all_changes = get_repo_status(&repo)?;
        let changes = crate::git::ignore::filter_ignored_changes(
            all_changes,
            &config.watch.ignore_patterns,
            true,
        )?;
        let is_clean = changes.is_empty();
        (
            RepoSnapshot {
                head_hash,
                branch_name,
                is_clean,
                changes,
            },
            "working_tree",
            Vec::new(),
        )
    };

    // Soft-open: prefer RO when ledger.db exists (0174); prospective included.
    let storage = open_storage_for_impact(&layout)?;

    // Prospective: in-memory only — no save_packet / latest-impact.json clobber (0173-G).
    if prospective {
        // Apply blast depth on config before orchestrator runs.
        let mut depth_note = None;
        if let Some(note) = crate::impact::enrichment::blast::apply_cli_blast_depth(
            &mut config.impact.blast_depth,
            config.impact.blast_depth_max,
            blast_depth,
        ) {
            depth_note = Some(note);
        }
        let mut packet = compute_impact_from_snapshot_in_memory_with_mode(
            &storage,
            &config,
            work_dir,
            snapshot,
            include_governance,
            analysis_mode,
            prospective_paths,
        )?;
        if let Some(note) = depth_note {
            packet.analysis_warnings.push(note);
            packet.analysis_warnings.sort();
            packet.analysis_warnings.dedup();
        }
        storage.shutdown()?;
        return emit_impact_output(&packet, summary, json, out, false, None);
    }

    let mut packet = crate::impact::orchestrator::map_snapshot_to_packet(snapshot, work_dir)?;
    apply_impact_honesty_fields(
        &mut packet,
        include_governance,
        analysis_mode,
        prospective_paths,
    );

    if let Some(note) = crate::impact::enrichment::blast::apply_cli_blast_depth(
        &mut config.impact.blast_depth,
        config.impact.blast_depth_max,
        blast_depth,
    ) {
        packet.analysis_warnings.push(note);
    }

    let orchestrator = crate::impact::orchestrator::ImpactOrchestrator::with_builtins();
    orchestrator.run(&mut packet, &storage, &config, work_dir)?;

    packet.finalize();
    let redactions = crate::impact::redact::redact_secrets(&mut packet);
    if !redactions.is_empty() {
        tracing::info!("Redacted {} secret(s) from impact packet", redactions.len());
    }

    if let Err(e) = storage.save_packet(&packet) {
        tracing::warn!("SQLite save failed: {e}");
    }

    let write_outcome = soft_write_impact_report(&layout, &packet, storage.is_read_only())?;
    apply_report_skip_honesty(&mut packet, write_outcome);
    storage.shutdown()?;

    let wrote = report_was_durable(write_outcome);
    emit_impact_output(&packet, summary, json, out, wrote, Some(write_outcome))
}

/// Emit impact JSON or human output.
///
/// When `wrote_report` is false (prospective or RO skip), do not claim
/// `latest-impact.json` was written. RO skip also prints greppable honesty
/// (AI1 BS1 / 0174).
fn emit_impact_output(
    packet: &crate::impact::packet::ImpactPacket,
    summary: bool,
    json: bool,
    out: Option<std::path::PathBuf>,
    wrote_report: bool,
    write_outcome: Option<ImpactReportWriteOutcome>,
) -> Result<()> {
    if json || out.is_some() {
        let json_output = serde_json::to_string_pretty(packet)
            .map_err(|e| miette::miette!("Failed to serialize impact report: {}", e))?;

        if let Some(path) = out {
            std::fs::write(&path, &json_output).map_err(|e| {
                miette::miette!(
                    "Failed to write impact report to '{}': {}",
                    path.display(),
                    e
                )
            })?;
            if !json {
                println!(
                    "Wrote impact report to {}",
                    path.display()
                        .to_string()
                        .if_supports_color(Stream::Stdout, |s| s.cyan())
                );
            }
        } else {
            println!("{}", json_output);
        }
        return Ok(());
    }

    let skipped_ro = matches!(write_outcome, Some(ImpactReportWriteOutcome::Skipped));

    if packet.tree_clean && packet.changes.is_empty() {
        if packet.analysis_mode == "prospective" && !wrote_report {
            println!(
                "\n{} No structural changes in prospective set.",
                success_marker()
            );
        } else {
            match write_outcome {
                Some(ImpactReportWriteOutcome::Written) => {
                    println!(
                        "\n{} Working tree is clean — impact report refreshed.",
                        success_marker()
                    );
                }
                Some(ImpactReportWriteOutcome::Skipped) => {
                    println!("\n{} Working tree is clean.", success_marker());
                    println!("{IMPACT_REPORT_RO_HONESTY}");
                }
                Some(ImpactReportWriteOutcome::Unchanged) | None => {
                    println!("\n{} Working tree is clean.", success_marker());
                }
            }
        }
        return Ok(());
    }

    if summary {
        crate::output::human::print_impact_brief(packet);
    } else {
        crate::output::human::print_impact_summary(packet);
    }

    if wrote_report {
        println!(
            "\n{} Wrote impact report to {}",
            success_marker(),
            ".ledgerful/reports/latest-impact.json".if_supports_color(Stream::Stdout, |s| s.cyan())
        );
    } else if packet.analysis_mode == "prospective" {
        println!(
            "\n{} Prospective analysis (in-memory only — did not rewrite latest-impact.json)",
            success_marker()
        );
    } else if skipped_ro {
        println!("\n{IMPACT_REPORT_RO_HONESTY}");
    }

    Ok(())
}

#[cfg(test)]
mod prospective_tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn prospective_snapshot_is_clean_false_and_added_for_missing() {
        let dir = tempdir().expect("tempdir");
        let root = dir.path();
        std::process::Command::new("git")
            .args(["init"])
            .current_dir(root)
            .output()
            .expect("git init");
        std::process::Command::new("git")
            .args(["config", "user.email", "t@t.com"])
            .current_dir(root)
            .output()
            .expect("email");
        std::process::Command::new("git")
            .args(["config", "user.name", "T"])
            .current_dir(root)
            .output()
            .expect("name");
        fs::create_dir_all(root.join("src")).expect("mkdir");
        fs::write(root.join("src/exists.rs"), "fn x() {}").expect("write");
        fs::write(root.join("README.md"), "hi").expect("readme");
        std::process::Command::new("git")
            .args(["add", "-A"])
            .current_dir(root)
            .output()
            .expect("add");
        std::process::Command::new("git")
            .args(["commit", "-m", "init"])
            .current_dir(root)
            .output()
            .expect("commit");

        let paths = parse_prospective_paths(&[
            "src/exists.rs".into(),
            "src/missing.rs".into(),
            "  ".into(),
        ])
        .expect("parse");
        assert_eq!(paths.len(), 2);

        let snap = build_prospective_snapshot(root, &paths).expect("snapshot");
        assert!(!snap.is_clean, "0147 bypass: is_clean must be false");
        assert_eq!(snap.changes.len(), 2);
        use crate::git::ChangeType;
        let mut by_path: std::collections::BTreeMap<String, ChangeType> = snap
            .changes
            .into_iter()
            .map(|c| (c.path.to_string_lossy().replace('\\', "/"), c.change_type))
            .collect();
        assert_eq!(by_path.remove("src/exists.rs"), Some(ChangeType::Modified));
        assert_eq!(by_path.remove("src/missing.rs"), Some(ChangeType::Added));
    }

    #[test]
    fn parse_prospective_paths_rejects_empty_and_cap() {
        assert!(parse_prospective_paths(&[]).is_err());
        assert!(parse_prospective_paths(&["".into(), "  ".into()]).is_err());
        let many: Vec<String> = (0..51).map(|i| format!("src/f{i}.rs")).collect();
        assert!(parse_prospective_paths(&many).is_err());
        let ok: Vec<String> = (0..50).map(|i| format!("src/f{i}.rs")).collect();
        assert_eq!(parse_prospective_paths(&ok).unwrap().len(), 50);
    }

    #[test]
    fn parse_prospective_paths_dedups_before_cap() {
        // 51 entries of the same path should collapse to 1 unique path.
        let dups: Vec<String> = (0..51).map(|_| "src/a.rs".into()).collect();
        let parsed = parse_prospective_paths(&dups).expect("dedup before cap");
        assert_eq!(parsed, vec!["src/a.rs".to_string()]);
    }

    #[test]
    fn prospective_impact_in_memory_does_not_clobber_latest_impact() {
        use crate::state::layout::Layout;
        use crate::state::reports::{LATEST_IMPACT_REPORT, write_impact_report};
        use crate::state::storage::StorageManager;

        let dir = tempdir().expect("tempdir");
        let root = dir.path();
        std::process::Command::new("git")
            .args(["init"])
            .current_dir(root)
            .output()
            .expect("git init");
        std::process::Command::new("git")
            .args(["config", "user.email", "t@t.com"])
            .current_dir(root)
            .output()
            .expect("email");
        std::process::Command::new("git")
            .args(["config", "user.name", "T"])
            .current_dir(root)
            .output()
            .expect("name");
        fs::create_dir_all(root.join("src")).expect("mkdir");
        fs::write(root.join("src/exists.rs"), "fn x() {}").expect("write");
        fs::write(root.join("README.md"), "hi").expect("readme");
        std::process::Command::new("git")
            .args(["add", "-A"])
            .current_dir(root)
            .output()
            .expect("add");
        std::process::Command::new("git")
            .args(["commit", "-m", "init"])
            .current_dir(root)
            .output()
            .expect("commit");

        let utf8 = camino::Utf8Path::from_path(root).expect("utf8");
        let layout = Layout::new(utf8);
        layout.ensure_state_dir().expect("state");
        let seed = crate::impact::packet::ImpactPacket {
            schema_version: "v1".to_string(),
            head_hash: Some("SEED_MARKER_0173_PROSPECTIVE".to_string()),
            risk_reasons: vec!["seed-reason-do-not-clobber".to_string()],
            ..Default::default()
        };
        write_impact_report(&layout, &seed).expect("seed report");
        let report_path = layout.reports_dir().join(LATEST_IMPACT_REPORT);
        let before = fs::read_to_string(report_path.as_std_path()).expect("read before");
        assert!(before.contains("SEED_MARKER_0173_PROSPECTIVE"));

        let storage = StorageManager::init(layout.state_subdir().join("ledger.db").as_std_path())
            .expect("db");
        let config = crate::config::model::Config::default();
        let paths = parse_prospective_paths(&["src/exists.rs".into()]).expect("paths");
        let snapshot = build_prospective_snapshot(root, &paths).expect("snapshot");
        let packet = compute_impact_from_snapshot_in_memory_with_mode(
            &storage,
            &config,
            root,
            snapshot,
            false,
            "prospective",
            paths,
        )
        .expect("prospective impact");
        assert_eq!(packet.analysis_mode, "prospective");
        assert!(!packet.changes.is_empty());

        let after = fs::read_to_string(report_path.as_std_path()).expect("read after");
        assert_eq!(
            before, after,
            "prospective impact must not rewrite latest-impact.json"
        );
        let _ = storage.shutdown();
    }

    /// 0174 T8/T9/T11: soft-write under RO storage skips report + honesty.
    #[test]
    fn soft_write_skips_under_ro_and_emits_honesty() {
        use crate::state::layout::Layout;
        use crate::state::reports::{
            IMPACT_REPORT_RO_HONESTY, ImpactReportWriteOutcome, LATEST_IMPACT_REPORT,
            soft_write_impact_report, write_impact_report,
        };
        use crate::state::storage::StorageManager;

        let dir = tempdir().expect("tempdir");
        let root = dir.path();
        let utf8 = camino::Utf8Path::from_path(root).expect("utf8");
        let layout = Layout::new(utf8);
        layout.ensure_state_dir().expect("state");
        // Seed DB + report; open true RO (simulates pure-RO fallback path).
        let w = StorageManager::init_with_layout(&layout).expect("write init");
        let _ = w.shutdown();

        let seed = crate::impact::packet::ImpactPacket {
            head_hash: Some("SEED_MARKER_0174_SILENT_RO".to_string()),
            risk_reasons: vec!["seed-silent-ro".to_string()],
            ..Default::default()
        };
        write_impact_report(&layout, &seed).expect("seed");
        let report_path = layout.reports_dir().join(LATEST_IMPACT_REPORT);
        let before = fs::read_to_string(report_path.as_std_path()).expect("before");

        let storage = StorageManager::open_read_only(&layout).expect("RO open");
        assert!(storage.is_read_only());

        let mut packet = crate::impact::packet::ImpactPacket {
            head_hash: Some("new-head-would-clobber".to_string()),
            tree_clean: false,
            risk_reasons: vec!["new".to_string()],
            ..Default::default()
        };
        let outcome =
            soft_write_impact_report(&layout, &packet, storage.is_read_only()).expect("soft");
        assert_eq!(outcome, ImpactReportWriteOutcome::Skipped);
        apply_report_skip_honesty(&mut packet, outcome);
        assert!(
            packet
                .analysis_warnings
                .iter()
                .any(|w| w.contains(IMPACT_REPORT_RO_HONESTY)),
            "JSON path must carry greppable honesty: {:?}",
            packet.analysis_warnings
        );
        assert!(!report_was_durable(outcome));

        let after = fs::read_to_string(report_path.as_std_path()).expect("after");
        assert_eq!(before, after, "RO soft-skip must not rewrite report");
        let _ = storage.shutdown();
    }

    /// 0174 T13 / B7: writable open succeeds as write-mode (reports can still write).
    #[test]
    fn open_storage_for_impact_writable_is_write_mode() {
        use crate::state::layout::Layout;
        use crate::state::storage::StorageManager;

        let dir = tempdir().expect("tempdir");
        let root = dir.path();
        let utf8 = camino::Utf8Path::from_path(root).expect("utf8");
        let layout = Layout::new(utf8);
        layout.ensure_state_dir().expect("state");
        // Pre-create DB (typical after prior scan/impact).
        let w = StorageManager::init_with_layout(&layout).expect("init");
        let _ = w.shutdown();

        let opened = open_storage_for_impact(&layout).expect("open");
        assert!(
            !opened.is_read_only(),
            "T13/B7: writable tree must write-open so reports still persist"
        );
        let _ = opened.shutdown();
    }
}
