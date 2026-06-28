use crate::config::load::load_config;
use crate::git::RepoSnapshot;
use crate::git::repo::{get_head_info, open_repo};
use crate::git::status::get_repo_status;
use crate::output::diagnostics::success_marker;
use crate::state::layout::Layout;
use crate::state::reports::write_impact_report;
use miette::Result;
use owo_colors::OwoColorize;
use std::env;

/// Run impact analysis using a pre-built `RepoSnapshot`.
///
/// Used by `execute_scan` when `--base-ref` is supplied: the caller has already
/// computed the changed file list via `git diff --name-only` and assembled the
/// snapshot; this function takes ownership and continues with the standard
/// enrichment pipeline.
pub fn execute_impact_silent_with_snapshot(
    snapshot: crate::git::RepoSnapshot,
) -> Result<crate::impact::packet::ImpactPacket> {
    let current_dir = env::current_dir()
        .map_err(|e| miette::miette!("Failed to get current directory: {}", e))?;

    let layout = Layout::new(current_dir.to_string_lossy().as_ref());

    let mut packet = crate::impact::orchestrator::map_snapshot_to_packet(snapshot, &current_dir)?;

    // Load main config for temporal analysis
    let config = load_config(&layout).unwrap_or_default();

    // Persist to SQLite and run Orchestrated Enrichment
    let db_path = layout.state_subdir().join("ledger.db");
    let storage = crate::state::storage::StorageManager::init(db_path.as_std_path())?;

    let orchestrator = crate::impact::orchestrator::ImpactOrchestrator::with_builtins();
    orchestrator.run(&mut packet, &storage, &config, &current_dir)?;

    // Post-processing: Finalize and Redact
    packet.finalize();
    crate::impact::redact::redact_secrets(&mut packet);

    // Save to ledger
    if let Err(e) = storage.save_packet(&packet) {
        tracing::warn!("SQLite save failed: {e}");
    }

    // Write report
    write_impact_report(&layout, &packet)?;

    storage.shutdown()?;

    Ok(packet)
}

pub fn execute_impact_silent() -> Result<crate::impact::packet::ImpactPacket> {
    let current_dir = env::current_dir()
        .map_err(|e| miette::miette!("Failed to get current directory: {}", e))?;

    let repo = open_repo(&current_dir)?;
    let (head_hash, branch_name) = get_head_info(&repo)?;
    let layout = Layout::new(current_dir.to_string_lossy().as_ref());

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

    // Load main config for temporal analysis
    let config = load_config(&layout).unwrap_or_default();

    // Persist to SQLite and run Orchestrated Enrichment
    let db_path = layout.state_subdir().join("ledger.db");
    let storage = crate::state::storage::StorageManager::init(db_path.as_std_path())?;

    let orchestrator = crate::impact::orchestrator::ImpactOrchestrator::with_builtins();
    orchestrator.run(&mut packet, &storage, &config, &current_dir)?;

    // Post-processing: Finalize and Redact
    packet.finalize();
    crate::impact::redact::redact_secrets(&mut packet);

    // Save to ledger
    if let Err(e) = storage.save_packet(&packet) {
        tracing::warn!("SQLite save failed: {e}");
    }

    // Write report
    write_impact_report(&layout, &packet)?;

    storage.shutdown()?;

    Ok(packet)
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

    let mut packet = crate::impact::orchestrator::map_snapshot_to_packet(snapshot, project_root)?;

    // Run Orchestrated Enrichment using the caller's storage (read-only).
    let orchestrator = crate::impact::orchestrator::ImpactOrchestrator::with_builtins();
    orchestrator.run(&mut packet, storage, config, project_root)?;

    // Post-processing: Finalize and Redact (no persist / no report write).
    packet.finalize();
    crate::impact::redact::redact_secrets(&mut packet);

    Ok(packet)
}

/// Render human-readable output for a pre-computed `ImpactPacket`.
///
/// Used by `execute_scan` so that the `--base-ref` snapshot flows through to
/// the human output path without re-deriving changes from working-tree status.
pub fn execute_impact_human(
    packet: &crate::impact::packet::ImpactPacket,
    summary: bool,
    base_ref_mode: bool,
) -> Result<()> {
    use crate::output::diagnostics::success_marker;
    use owo_colors::OwoColorize;

    if packet.tree_clean && packet.changes.is_empty() {
        if base_ref_mode {
            println!("\n{} No changes detected vs base ref.", success_marker());
            println!("  All files between base ref and HEAD are clean.");
        } else {
            println!(
                "\n{} Working tree is clean — impact report refreshed.",
                success_marker()
            );
        }
        return Ok(());
    }

    if summary {
        crate::output::human::print_impact_brief(packet);
    } else {
        crate::output::human::print_impact_summary(packet);
    }

    println!(
        "\n{} Wrote impact report to {}",
        success_marker(),
        ".ledgerful/reports/latest-impact.json".cyan()
    );

    Ok(())
}

pub fn execute_impact(
    all_parents: bool,
    summary: bool,
    _telemetry_coverage: bool,
    dead_code: bool,
    json: bool,
    out: Option<std::path::PathBuf>,
) -> Result<()> {
    let current_dir = env::current_dir()
        .map_err(|e| miette::miette!("Failed to get current directory: {}", e))?;

    let repo = open_repo(&current_dir)?;
    let (head_hash, branch_name) = get_head_info(&repo)?;
    let layout = Layout::new(current_dir.to_string_lossy().as_ref());

    // Filter changes against config ignore_patterns
    let mut config =
        load_config(&layout).unwrap_or_else(|_| crate::config::model::Config::default());
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

    // CLI override
    if all_parents {
        config.temporal.all_parents = true;
    }
    if dead_code {
        config.dead_code.enabled = true;
    }

    // Persist to SQLite and run Orchestrated Enrichment
    let db_path = layout.state_subdir().join("ledger.db");
    let storage = crate::state::storage::StorageManager::init(db_path.as_std_path())?;

    let orchestrator = crate::impact::orchestrator::ImpactOrchestrator::with_builtins();
    orchestrator.run(&mut packet, &storage, &config, &current_dir)?;

    // Post-processing: Finalize and Redact
    packet.finalize();
    let redactions = crate::impact::redact::redact_secrets(&mut packet);
    if !redactions.is_empty() {
        tracing::info!("Redacted {} secret(s) from impact packet", redactions.len());
    }

    // Save to ledger
    if let Err(e) = storage.save_packet(&packet) {
        tracing::warn!("SQLite save failed: {e}");
    }

    // Write report
    write_impact_report(&layout, &packet)?;

    storage.shutdown()?;

    // Handle --json and --out: serialize to stdout or file
    if json || out.is_some() {
        let json_output = serde_json::to_string_pretty(&packet)
            .map_err(|e| miette::miette!("Failed to serialize impact report: {}", e))?;

        if let Some(path) = out {
            std::fs::write(&path, &json_output).map_err(|e| {
                miette::miette!(
                    "Failed to write impact report to '{}': {}",
                    path.display(),
                    e
                )
            })?;
            eprintln!(
                "Wrote impact report to {}",
                path.display().to_string().cyan()
            );
        } else {
            println!("{}", json_output);
        }
        return Ok(());
    }

    if packet.tree_clean && packet.changes.is_empty() {
        println!(
            "\n{} Working tree is clean — impact report refreshed.",
            success_marker()
        );
        return Ok(());
    }

    if summary {
        crate::output::human::print_impact_brief(&packet);
    } else {
        crate::output::human::print_impact_summary(&packet);
    }

    println!(
        "\n{} Wrote impact report to {}",
        success_marker(),
        ".ledgerful/reports/latest-impact.json".cyan()
    );

    Ok(())
}
