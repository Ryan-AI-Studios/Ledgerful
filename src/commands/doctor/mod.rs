mod binary_currency;
mod finding;
mod remediation;

pub use binary_currency::{
    BINARY_BEHIND_TREE_CODE, BINARY_BEHIND_TREE_REMEDIATION, BinaryCurrencyLag,
    build_binary_behind_tree_finding, classify_binary_currency, compose_binary_currency_message,
    is_ledgerful_engine_worktree, probe_binary_currency, sha_prefix_equal, shorten_sha_for_display,
    worktree_package_version,
};
pub use finding::{
    DoctorCategory, DoctorFinding, DoctorSeverity, DoctorSummary, dashboard_failures,
    ready_for_publish, summarize,
};
pub use remediation::{
    ContentHashDriftInputs, GraphAgeInputs, GraphIndexHealth, SearchDocsClassification,
    build_graph_content_stale_finding, build_graph_drift_check_failed_finding,
    build_search_empty_finding, build_sig_pin_finding, build_sig_version_finding,
    classify_graph_index_health, classify_search_document_count,
    graph_content_stale_index_health_line, graph_current_empty_cozo_index_health_line,
    graph_current_populated_index_health_line, graph_drift_check_failed_index_health_line,
    search_empty_index_health_line, search_ok_index_health_line,
};

use crate::output::human::print_doctor_report;
use crate::platform::env::ExecutableStatus;
use crate::platform::{check_tools, classify_path, current_platform, detect_shell};
use crate::state::layout::Layout;
use chrono::Utc;
use miette::{IntoDiagnostic, Result};
use owo_colors::{OwoColorize, Stream, Style};
use serde_json::json;
use std::env;

use crate::state::reports::write_clean_tree_tombstone;
use crate::state::storage::StorageManager;

/// Soft-pin warning when `intent.trusted_public_keys` is empty (0072 / 0100 DoD-3).
/// Shares vocabulary with [`crate::ledger::crypto::SignatureTrustStatus::ValidUnknownKey`]:
/// "unknown key", pin, trusted / trusted_public_keys.
/// Message text only (no severity prefix) — severity lives on [`DoctorFinding`].
pub const SIG_PIN_WARNING: &str = "no intent.trusted_public_keys pinned; crypto-valid signatures report VALID (unknown key). Pin keys after init or re-sign.";

/// Run doctor health checks.
///
/// When `json` is true, stdout is pure schema-v1 JSON only (no human banners,
/// sccache/SCIP/VRAM printers). Exit code is 1 iff any **block** finding.
///
/// `--apply-hook-refresh` rewrites only known Ledgerful marker-bounded product
/// templates (0121). Cannot be combined with `--json`.
pub fn execute_doctor(json: bool, apply_hook_refresh: bool, dry_run: bool) -> Result<()> {
    if json && apply_hook_refresh {
        return Err(miette::miette!(
            "doctor --json cannot be combined with --apply-hook-refresh"
        ));
    }

    let current_dir = env::current_dir().into_diagnostic()?;
    // Resolve via git discover so nested cwd and linked worktrees share the
    // correct state home (0108). Never treat cwd as repo root.
    let layout = crate::commands::helpers::get_layout_or_cwd_if_not_git()?;

    // Product hook template refresh is opt-in and always human (0121).
    if apply_hook_refresh {
        let root = layout.root.as_path();
        let refresh = crate::commands::hook_template::refresh_product_templates_at(root, dry_run)?;
        crate::commands::hook_template::print_refresh_report(&refresh);
        // Continue into normal doctor findings so the post-refresh state is
        // visible in the same run (detect-only after apply).
    }

    let platform = current_platform();
    let shell = detect_shell();
    let tools = check_tools();

    layout.ensure_state_dir()?;
    let storage = StorageManager::init_with_layout(&layout)?;

    let platform_str = format!("{:?}", platform);
    let shell_str = format!("{:?}", shell);
    let path_kind_str = format!("{:?}", classify_path(&current_dir));
    let work_root_str = layout.root.to_string();
    let state_dir_str = layout.state_dir.to_string();
    let path_display = current_dir.to_string_lossy().into_owned();

    let mut findings: Vec<DoctorFinding> = Vec::new();

    // 0137: engine binary currency (version + embedded SHA vs worktree HEAD).
    // Engine-only; install remediation only — never auto-install. Runtime HEAD via gix.
    if let Some(finding) = probe_binary_currency(
        layout.root.as_std_path(),
        &current_dir,
        env!("CARGO_PKG_VERSION"),
        env!("LEDGERFUL_GIT_SHA"),
    ) {
        findings.push(finding);
    }

    // Per-tool identity (0109): git missing = block; gemini missing = info/optional.
    for (name, status) in &tools {
        if matches!(status, ExecutableStatus::NotFound) {
            if name == "git" {
                findings.push(DoctorFinding::block(
                    "tool-git",
                    DoctorCategory::Tools,
                    "git NOT FOUND — required for publish-environment path",
                ));
            } else if name == "gemini" || name == "gemini-cli" {
                findings.push(DoctorFinding::info(
                    "tool-gemini",
                    DoctorCategory::Optional,
                    format!("{name} NOT FOUND (optional ask backend CLI)"),
                ));
            } else {
                findings.push(DoctorFinding::warn(
                    format!("tool-{name}"),
                    DoctorCategory::Tools,
                    format!("{name} NOT FOUND"),
                ));
            }
        }
    }

    let mut report = crate::output::human::DoctorReport {
        platform: &platform_str,
        shell: &shell_str,
        tools: &tools,
        path_display: &path_display,
        path_kind: &path_kind_str,
        work_root: &work_root_str,
        state_dir: &state_dir_str,
        is_wsl_mounted: false,
        embedding_model_status: "checking...".to_string(),
        embedding_model_failed: false,
        completion_model_status: "checking...".to_string(),
        native_graph_status: "checking...".to_string(),
        active_ask_backend: "checking...".to_string(),
        index_health: Vec::new(),
        target_triple: env!("TARGET"),
    };

    // Split-brain residue: local worktree `.ledgerful/state/ledger.db` exists
    // and is not the same file as the resolved shared ledger (detect only).
    if let Some(f) = split_brain_ledger_finding(&layout) {
        findings.push(f);
    }

    // --- Intelligence Probes ---
    // Soft-load config: malformed/unreadable config must not abort doctor (0109).
    // Structured `legacy-config` findings come from `doctor_config_findings` later;
    // continue probes with defaults so `--json` / readyForPublish still work.
    let config = match crate::config::load::load_config(&layout) {
        Ok(c) => c,
        Err(e) => {
            tracing::debug!("doctor config load failed (continuing with defaults): {e}");
            crate::config::model::Config::default()
        }
    };
    let mut model_config = config.local_model.clone();
    model_config.timeout_secs = 2;

    report.active_ask_backend = format_active_ask_backend(&config);

    // Gate mode: mismatch → warn finding; ok/no-history → display only (omit finding).
    match gate_mode_status(&layout, &config) {
        GateModeOutcome::Ok(line) | GateModeOutcome::NoHistory(line) => {
            report.index_health.push(line);
        }
        GateModeOutcome::Mismatch { display, finding } => {
            report.index_health.push(display);
            findings.push(finding);
        }
    }

    // Embedding: structured findings from BackendAvailabilityReport (optional).
    {
        let avail = format_embedding_backend_availability(&config.local_model, &model_config);
        report.embedding_model_status = avail.display.clone();
        report.embedding_model_failed = avail.is_failure;
        if let Some(detail) = &avail.debug_detail {
            tracing::debug!("Full embedding model error: {}", detail);
        }
        if let Some(f) = embedding_finding(&config.local_model, &avail) {
            findings.push(f);
        }
    }

    // Completion model (optional).
    if config.local_model.generation_model.is_empty() {
        report.completion_model_status = "Not configured"
            .if_supports_color(Stream::Stdout, |s| s.yellow())
            .to_string();
        findings.push(DoctorFinding::info(
            "completion-not-configured",
            DoctorCategory::Optional,
            "Completion model not configured",
        ));
    } else {
        match probe_with_retry(|| crate::local_model::client::ping_completions(&model_config)) {
            ProbeResult::Healthy(model) => {
                report.completion_model_status = format!(
                    "{} @ {}",
                    model,
                    config
                        .local_model
                        .generation_url
                        .as_deref()
                        .unwrap_or(&config.local_model.base_url)
                );
            }
            ProbeResult::ReachableAfterRetry {
                val: model,
                retries,
            } => {
                report.completion_model_status = format!(
                    "{} @ {} (reachable after retry: flaky/transient - {})",
                    model,
                    config
                        .local_model
                        .generation_url
                        .as_deref()
                        .unwrap_or(&config.local_model.base_url),
                    format!(
                        "{} {}",
                        retries,
                        if retries == 1 { "retry" } else { "retries" }
                    )
                    .if_supports_color(Stream::Stdout, |s| s.green())
                );
            }
            ProbeResult::Unreachable { err, retries } => {
                let retry_suffix = if retries > 0 {
                    format!(" after {} retries", retries)
                } else {
                    "".to_string()
                };
                let truncated: String = err.chars().take(80).collect();
                let detail_hint = if err.chars().count() > 80 {
                    " [set RUST_LOG=debug for details]"
                } else {
                    ""
                };
                report.completion_model_status = format!(
                    "unreachable ({}{}){}",
                    truncated.if_supports_color(Stream::Stdout, |s| s.yellow()),
                    retry_suffix,
                    detail_hint
                );
                tracing::debug!("Full completion model error: {}", err);
                findings.push(DoctorFinding::warn(
                    "completion-unreachable",
                    DoctorCategory::Optional,
                    format!(
                        "Completion model unreachable ({truncated}{retry_suffix}){detail_hint}"
                    ),
                ));
            }
        }
    }

    let mut total_nodes = 0;
    let mut total_edges = 0;

    // --- Graph Probe ---
    if let Some(cozo) = &storage.cozo {
        match cozo.run_script("?[count(n)] := *node{id: n}") {
            Ok(res) => {
                let node_count = res
                    .rows
                    .first()
                    .and_then(|r| r.first())
                    .and_then(|v| match v {
                        cozo::DataValue::Num(cozo::Num::Int(i)) => Some(*i),
                        _ => None,
                    })
                    .unwrap_or(0);

                let edge_res = cozo.run_script("?[count(s)] := *edge{source: s}");
                let edge_count = edge_res
                    .ok()
                    .and_then(|res| res.rows.first().cloned())
                    .and_then(|r| r.first().cloned())
                    .and_then(|v| match v {
                        cozo::DataValue::Num(cozo::Num::Int(i)) => Some(i),
                        _ => None,
                    })
                    .unwrap_or(0);

                total_nodes = node_count;
                total_edges = edge_count;

                report.native_graph_status = format!(
                    "Ready (CozoDB active, {} nodes, {} edges)",
                    node_count, edge_count
                );
            }
            Err(e) => {
                report.native_graph_status = format!(
                    "Error ({})",
                    e.if_supports_color(Stream::Stdout, |s| s.red())
                );
                findings.push(DoctorFinding::warn(
                    "graph-error",
                    DoctorCategory::Index,
                    format!("Native graph error ({e})"),
                ));
            }
        }
    } else {
        report.native_graph_status = "Not initialized".to_string();
        findings.push(DoctorFinding::info(
            "graph-not-initialized",
            DoctorCategory::Index,
            "Native graph not initialized",
        ));
    }

    // --- Index Health Probes ---
    // 1. Tantivy Search Index
    let index_path = layout.search_index_dir();
    if !index_path.exists() {
        findings.push(DoctorFinding::warn(
            "search-missing",
            DoctorCategory::Index,
            "Search index: Missing (run 'ledgerful index')",
        ));
    } else {
        let engine = crate::search::tantivy_engine::TantivySearchEngine::open_or_create(
            index_path.as_std_path(),
        );
        match engine {
            Ok(e) => {
                if let Err(err) = e.verify_index_integrity(index_path.as_std_path()) {
                    findings.push(DoctorFinding::warn(
                        "search-corrupt",
                        DoctorCategory::Index,
                        format!("Search index: Corrupt ({err}) - run 'ledgerful index --full'"),
                    ));
                } else {
                    let docs = e.document_count();
                    // 0126: pure classify — empty is a state, not OK.
                    match classify_search_document_count(docs) {
                        SearchDocsClassification::Empty => {
                            findings.push(build_search_empty_finding());
                            report
                                .index_health
                                .push(search_empty_index_health_line().to_string());
                        }
                        SearchDocsClassification::Populated { docs } => {
                            report.index_health.push(search_ok_index_health_line(docs));
                        }
                    }
                }
            }
            Err(e) => {
                findings.push(DoctorFinding::warn(
                    "search-load-failed",
                    DoctorCategory::Index,
                    format!("Search index: Load failed ({e})"),
                ));
            }
        }
    }

    // 2. Knowledge Graph Staleness (0133: age first STOP, else content-hash drift)
    // Age path: graph-empty | graph-stale only — do not run content drift (double findings + I/O).
    // Else: one count_content_hash_drift on layout.root (never bare cwd); dirty → content-stale;
    // clean → Current / empty-Cozo hint; Err → graph-drift-check-failed (never Current).
    let age_warning =
        crate::index::staleness::check_index_staleness(&storage, config.index.stale_threshold_days);
    let age_inputs = age_warning.as_ref().map(|w| GraphAgeInputs {
        is_missing: w.is_missing,
        stale_files: w.stale_files,
    });
    let drift_for_classify: Option<Result<ContentHashDriftInputs, String>> = if age_inputs.is_none()
    {
        match crate::index::staleness::count_content_hash_drift(&storage, layout.root.as_path()) {
            Ok(d) => Some(Ok(ContentHashDriftInputs {
                changed_or_unindexed: d.changed_or_unindexed,
            })),
            Err(e) => {
                tracing::debug!("Full graph content-hash drift check error: {e}");
                Some(Err(e.to_string()))
            }
        }
    } else {
        None
    };
    let graph_health = classify_graph_index_health(
        age_inputs.as_ref(),
        drift_for_classify,
        total_nodes,
        total_edges,
    );
    match graph_health {
        GraphIndexHealth::AgeEmpty => {
            findings.push(DoctorFinding::warn(
                "graph-empty",
                DoctorCategory::Index,
                "Graph state: Empty (never indexed)",
            ));
        }
        GraphIndexHealth::AgeStale { stale_files } => {
            findings.push(DoctorFinding::warn(
                "graph-stale",
                DoctorCategory::Index,
                format!(
                    "Graph state: STALE ({stale_files} files affected) - run 'ledgerful index'"
                ),
            ));
        }
        GraphIndexHealth::ContentStale { n } => {
            findings.push(build_graph_content_stale_finding(n));
            report
                .index_health
                .push(graph_content_stale_index_health_line(n));
        }
        GraphIndexHealth::DriftCheckFailed { truncated_err } => {
            findings.push(build_graph_drift_check_failed_finding(&truncated_err));
            report
                .index_health
                .push(graph_drift_check_failed_index_health_line().to_string());
        }
        GraphIndexHealth::CurrentPopulated => {
            report
                .index_health
                .push(graph_current_populated_index_health_line().to_string());
        }
        GraphIndexHealth::CurrentEmptyCozo => {
            report
                .index_health
                .push(graph_current_empty_cozo_index_health_line().to_string());
        }
    }

    // 3. Impact Report Freshness
    if let Ok(repo) = crate::git::repo::open_repo(&current_dir)
        && let Ok((head_hash, branch_name)) = crate::git::repo::get_head_info(&repo)
    {
        let changes = crate::git::status::get_repo_status(&repo).unwrap_or_default();
        let filtered = crate::git::ignore::filter_ignored_changes(
            changes,
            &config.watch.ignore_patterns,
            true,
        )
        .unwrap_or_default();

        let snapshot = crate::git::RepoSnapshot {
            head_hash,
            branch_name,
            is_clean: filtered.is_empty(),
            changes: filtered,
        };

        let freshness = crate::state::reports::check_impact_freshness(&layout, &snapshot);
        match freshness {
            crate::state::reports::ImpactFreshness::Missing => {
                findings.push(DoctorFinding::warn(
                    "impact-missing",
                    DoctorCategory::Index,
                    "Impact report: None (run 'ledgerful scan --impact')",
                ));
            }
            crate::state::reports::ImpactFreshness::CurrentClean => {
                report
                    .index_health
                    .push("Impact report: Current (Clean tree)".to_string());
            }
            crate::state::reports::ImpactFreshness::CurrentDirty => {
                report
                    .index_health
                    .push("Impact report: Current (Dirty tree packet)".to_string());
            }
            crate::state::reports::ImpactFreshness::Stale { reason } => {
                if snapshot.is_clean {
                    tracing::debug!(
                        "Auto-refreshing stale clean-tree impact report for HEAD {:?}",
                        snapshot.head_hash
                    );
                    match write_clean_tree_tombstone(
                        &layout,
                        snapshot.head_hash.clone(),
                        snapshot.branch_name.clone(),
                    ) {
                        Ok(()) => {
                            tracing::debug!("Auto-refreshed impact report successfully");
                            report
                                .index_health
                                .push("Impact report: Current (Clean tree)".to_string());
                        }
                        Err(e) => {
                            tracing::debug!("Failed to auto-refresh impact report: {e}");
                            findings.push(DoctorFinding::warn(
                                "impact-stale",
                                DoctorCategory::Index,
                                format!(
                                    "Impact report: STALE ({reason}) — run 'ledgerful impact' or 'ledgerful scan --impact' to refresh"
                                ),
                            ));
                        }
                    }
                } else {
                    findings.push(DoctorFinding::warn(
                        "impact-stale",
                        DoctorCategory::Index,
                        format!(
                            "Impact report: STALE ({reason}) — run 'ledgerful impact' or 'ledgerful scan --impact' to refresh"
                        ),
                    ));
                }
            }
            crate::state::reports::ImpactFreshness::Corrupt { reason } => {
                // Impact-corrupt stays warn (not block): publish path does not require impact.
                findings.push(DoctorFinding::warn(
                    "impact-corrupt",
                    DoctorCategory::Index,
                    format!("Impact report: Corrupt ({reason})"),
                ));
            }
        }
    }

    // Track 0043: warn on oversized / high-cardinality timing tables.
    for (i, w) in crate::commands::timings::doctor_timing_warnings(storage.get_connection())
        .into_iter()
        .enumerate()
    {
        findings.push(DoctorFinding::warn(
            format!("timings-{i}"),
            DoctorCategory::Other,
            w,
        ));
    }

    // Track 0074: lifecycle integrity block codes.
    let lifecycle = crate::commands::ledger::detect_lifecycle_signals(&layout);
    if lifecycle.promote_orphan {
        findings.push(DoctorFinding::block(
            crate::commands::hook_sidecar::CODE_PROMOTE_ORPHAN,
            DoctorCategory::Lifecycle,
            format!(
                "promote-failed or HEAD-matching orphan retained (tx={}). Recover with: {}",
                lifecycle
                    .promote_orphan_tx_id
                    .as_deref()
                    .unwrap_or("unknown"),
                crate::commands::hook_sidecar::RECOVER_HINT
            ),
        ));
    }
    if lifecycle.head_uncovered && config.gate.is_enforce() {
        findings.push(DoctorFinding::block(
            crate::commands::hook_sidecar::CODE_HEAD_UNCOVERED,
            DoctorCategory::Lifecycle,
            format!(
                "HEAD uncovered via promote-fail/HEAD-matching pending sidecar under enforce (message-hash heuristic; not a full material-HEAD-without-row scan). Recover with: {}",
                crate::commands::hook_sidecar::RECOVER_HINT
            ),
        ));
    }
    if config.gate.is_enforce() && config.intent.required == "never" {
        findings.push(DoctorFinding::block(
            crate::commands::hook_sidecar::CODE_INTENT_NEVER_UNDER_ENFORCE,
            DoctorCategory::Lifecycle,
            "intent.required=never is incompatible with gate mode enforce.",
        ));
    }
    // 0072 M2: enforce without require_signing is block.
    if config.gate.is_enforce() && !config.intent.require_signing {
        findings.push(DoctorFinding::block(
            "sig-require",
            DoctorCategory::Lifecycle,
            "gate.mode=enforce but intent.require_signing=false. Unsigned rows will not fail verify --signatures.",
        ));
    }
    // Soft pin warn when no trusted keys are configured (0125: structured remediation).
    // Path-only keys read — never call get_keys_dir() (creates keys dir).
    if config.intent.trusted_public_keys.is_empty() {
        let pub_hex = crate::ledger::crypto::keys_dir_path()
            .ok()
            .and_then(|keys_dir| {
                crate::ledger::crypto::read_public_key_hex(&keys_dir)
                    .ok()
                    .flatten()
            });
        findings.push(build_sig_pin_finding(pub_hex.as_deref()));
    }
    if config.intent.min_sig_version < 2 {
        // Defensive count (like phantom): omit number on SQL error, still emit remediation.
        let v1_count = count_entries_below_sig_version(storage.get_connection(), 2).ok();
        findings.push(build_sig_version_finding(
            config.intent.min_sig_version,
            v1_count,
        ));
    }
    // Legacy phantom Verified without a bound verification run (forward-only flag).
    if let Ok(count) = count_phantom_verified(storage.get_connection())
        && count > 0
    {
        findings.push(DoctorFinding::warn(
            crate::commands::hook_sidecar::CODE_PHANTOM_PROMOTED_WITHOUT_VERIFY,
            DoctorCategory::Signing,
            format!(
                "{count} committed row(s) have verification_status=Verified with no bound verification_results row (legacy promote phantoms; forward-only)."
            ),
        ));
    }

    // 0094: four-surface legacy-migration residue (warn only — never block).
    // Prefer layout.root (git work root) over cwd for nested directories.
    {
        let mut legacy = collect_legacy_migration_findings(layout.root.as_path(), &layout);
        legacy.sort_by(|a, b| a.code.cmp(&b.code).then(a.message.cmp(&b.message)));
        findings.extend(legacy);
    }

    // 0121: product hook template stamp drift (Info + Gate; never blocks publish).
    findings.extend(
        crate::commands::hook_template::hook_template_stale_findings(layout.root.as_path()),
    );

    // SCIP + sccache → info / optional
    findings.extend(collect_scip_findings(&config));
    if let Some(f) = sccache_hint_finding() {
        findings.push(f);
    }

    // 0119: operator chain-head retention hygiene (info/optional only).
    if let Some(f) = chain_checkpoint_practice_finding(storage.get_connection()) {
        findings.push(f);
    }

    // 0110: light team-sync findings (warn/info only). Disabled sync never blocks publish.
    findings.extend(sync_doctor_findings(&layout, &config));

    // Deterministic ordering for JSON/tests.
    findings.sort_by(|a, b| {
        a.code
            .cmp(&b.code)
            .then(a.message.cmp(&b.message))
            .then(a.severity.as_str().cmp(b.severity.as_str()))
    });

    let counts = summarize(&findings);
    let summary = crate::output::human::DoctorSummaryCounts {
        block: counts.block,
        warn: counts.warn,
        info: counts.info,
    };
    let ready = ready_for_publish(&findings);

    // Persist dashboard signal from the same findings list (before exit).
    if let Err(e) = write_doctor_results(&layout, &findings) {
        tracing::warn!("Failed to write doctor-results.json: {}", e);
    }

    if json {
        let body = json!({
            "schemaVersion": 1u32,
            "readyForPublish": ready,
            "summary": {
                "block": counts.block,
                "warn": counts.warn,
                "info": counts.info,
            },
            "findings": findings,
            "environment": {
                "platform": platform_str,
                "shell": shell_str,
                "workRoot": work_root_str,
                "stateDir": state_dir_str,
                "pathDisplay": path_display,
                "targetTriple": env!("TARGET"),
                // 0137 B5b — agents can read currency without parsing --version text.
                "binaryVersion": env!("CARGO_PKG_VERSION"),
                "buildSha": env!("LEDGERFUL_GIT_SHA"),
            },
        });
        let pretty = serde_json::to_string_pretty(&body).into_diagnostic()?;
        println!("{pretty}");
    } else {
        print_doctor_report(&report, &summary, &findings);
        print_vram_section();
    }

    // Complete side effects before process::exit on block (§3.9).
    drop(storage);

    if counts.block > 0 {
        if !json {
            eprintln!(
                "\n{} {} block finding(s). Exit 1.",
                "Doctor:".if_supports_color(Stream::Stderr, |s| s.style(Style::new().red().bold())),
                counts.block
            );
        } else {
            eprintln!("Doctor: {} block finding(s). Exit 1.", counts.block);
        }
        std::process::exit(1);
    }

    Ok(())
}

/// Four-surface legacy migration checks (0094 DoD-6). Structured findings with
/// remediation in messages. Empty on a fully migrated repo.
fn collect_legacy_migration_findings(
    root: &camino::Utf8Path,
    layout: &Layout,
) -> Vec<DoctorFinding> {
    let mut findings = Vec::new();

    // 1. Legacy state directory still present (report only — never merge/delete).
    let legacy_dir = root.join(crate::state::layout::LEGACY_STATE_DIR);
    if legacy_dir.is_dir() {
        let ledger_db = legacy_dir.join("state").join("ledger.db");
        if ledger_db.is_file() {
            findings.push(DoctorFinding::warn(
                "legacy-state",
                DoctorCategory::Migration,
                format!(
                    "retired state directory `{legacy_dir}` still present and contains ledger.db (not merged automatically). Current state is `{}`. After verifying the active ledger, remove the legacy directory manually if unused.",
                    layout.state_dir
                ),
            ));
        } else {
            findings.push(DoctorFinding::warn(
                "legacy-state",
                DoctorCategory::Migration,
                format!(
                    "retired state directory `{legacy_dir}` still present (empty or no ledger.db). Safe to remove manually after confirming `{}` is current.",
                    layout.state_dir
                ),
            ));
        }
    }

    // 2. Hooks: legacy invocations / markers / duplicates / RT-H5 inert gate.
    findings.extend(crate::commands::hook_repair::doctor_legacy_hook_findings(
        root,
    ));

    // 3. .gitignore names only the legacy path (not .ledgerful/).
    findings.extend(doctor_gitignore_legacy_findings(root));

    // 4. Config staleness / unknown keys (serde_ignored; no deny_unknown_fields).
    findings.extend(crate::config::load::doctor_config_findings(layout));

    findings.sort_by(|a, b| a.code.cmp(&b.code).then(a.message.cmp(&b.message)));
    findings.dedup();
    findings
}

/// Warn when `.gitignore` mentions the retired state path but not `.ledgerful/`.
fn doctor_gitignore_legacy_findings(root: &camino::Utf8Path) -> Vec<DoctorFinding> {
    let gi_path = root.join(".gitignore");
    if !gi_path.is_file() {
        return Vec::new();
    }
    let Ok(content) = std::fs::read_to_string(gi_path.as_std_path()) else {
        return Vec::new();
    };
    let legacy_name = crate::state::layout::LEGACY_STATE_DIR;
    let has_legacy = content.lines().any(|l| {
        let t = l.trim();
        t == legacy_name
            || t == format!("{legacy_name}/")
            || t == format!("/{legacy_name}")
            || t == format!("/{legacy_name}/")
            || t.starts_with(&format!("{legacy_name}/"))
            || t.starts_with(&format!("/{legacy_name}/"))
    });
    let has_current = content
        .lines()
        .any(|l| crate::git::ignore::gitignore_patterns_equivalent(l, ".ledgerful/"));
    if has_legacy && !has_current {
        vec![DoctorFinding::warn(
            "legacy-gitignore",
            DoctorCategory::Migration,
            "`.gitignore` names the retired state path but not `.ledgerful/`. Run `ledgerful init` (ensures `.ledgerful/` is gitignored) or add `.ledgerful/` to `.gitignore` manually.",
        )]
    } else {
        Vec::new()
    }
}

/// Count committed ledger entries marked Verified without a verification_results row.
fn count_phantom_verified(conn: &rusqlite::Connection) -> Result<i64> {
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM ledger_entries le
             WHERE le.verification_status = 'verified'
               AND NOT EXISTS (
                   SELECT 1 FROM verification_results vr WHERE vr.tx_id = le.tx_id
               )",
            [],
            |row| row.get(0),
        )
        .into_diagnostic()?;
    Ok(count)
}

/// Count LOCAL committed ledger rows with `sig_version < below`.
///
/// Defensive: returns `Err` when the table/column is missing (fresh repos).
/// Callers should use `if let Ok(count) = …` and omit the count on error.
fn count_entries_below_sig_version(conn: &rusqlite::Connection, below: u32) -> Result<i64> {
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM ledger_entries
             WHERE origin = 'LOCAL' AND sig_version < ?1",
            [below as i64],
            |row| row.get(0),
        )
        .into_diagnostic()?;
    Ok(count)
}

/// Optional informational hint: suggest sccache for cold/CI builds when no
/// `RUSTC_WRAPPER` is set. Severity **info** / category **optional** (0109).
fn sccache_hint_finding() -> Option<DoctorFinding> {
    if std::env::var("RUSTC_WRAPPER").is_err() {
        Some(DoctorFinding::info(
            "sccache-hint",
            DoctorCategory::Optional,
            "Cold or CI builds may benefit from sccache 0.17.0+. Set RUSTC_WRAPPER=sccache and CARGO_INCREMENTAL=0. Note: do not combine with CARGO_INCREMENTAL=1; use one or the other.",
        ))
    } else {
        None
    }
}

/// 0119: when a signed chain head exists, remind operators to retain checkpoints
/// off-machine. Info + Optional — never blocks readyForPublish / dashboard_failures.
/// No head or unsigned head → no finding.
fn chain_checkpoint_practice_finding(conn: &rusqlite::Connection) -> Option<DoctorFinding> {
    let db = crate::ledger::db::LedgerDb::new(conn);
    let head = db.get_chain_head().ok()??;
    let sig = head.head_signature.as_deref().unwrap_or("");
    let pub_key = head.head_public_key.as_deref().unwrap_or("");
    if sig.is_empty() || pub_key.is_empty() {
        return None;
    }
    Some(DoctorFinding::info(
        "chain-checkpoint-practice",
        DoctorCategory::Optional,
        "Signed chain head present. Periodically run `ledgerful export head`, retain the file outside this machine and outside `.ledgerful/`, then `ledgerful verify --signatures --against-export <path>` (checkpoint: live must extend or equal the retained head). See docs/chain-checkpoint.md.",
    ))
}

/// 0110: light team-sync honesty findings.
///
/// Only emit when `[sync].enabled = true` and init/target are incomplete.
/// Severity is **warn** / category **optional** so sync-off never sole-blocks
/// `readyForPublish`. See `docs/team-sync.md`.
fn sync_doctor_findings(
    layout: &Layout,
    config: &crate::config::model::Config,
) -> Vec<DoctorFinding> {
    if !config.sync.enabled {
        return Vec::new();
    }

    let mut findings = Vec::new();
    let key_path = layout.state_dir.join("sync").join("device.key");
    let pub_path = layout.state_dir.join("sync").join("device.pub");
    let keys_ok = key_path.exists() && pub_path.exists();

    // SoT: non-empty sync_state.device_id (row id=1). Missing DB is treated as uninitialized.
    let sot_ok = (|| {
        let storage = crate::state::storage::StorageManager::init_with_layout(layout).ok()?;
        let conn = storage.get_connection();
        let id: Option<String> = conn
            .query_row("SELECT device_id FROM sync_state WHERE id = 1", [], |row| {
                row.get(0)
            })
            .ok();
        id.filter(|s| !s.trim().is_empty() && s != "unknown")
    })()
    .is_some();

    if !keys_ok || !sot_ok {
        findings.push(DoctorFinding::warn(
            "sync-enabled-not-initialized",
            DoctorCategory::Optional,
            "[sync].enabled=true but team sync is not fully initialized (need device.key + device.pub + sync_state.device_id). Run `ledgerful sync init` or set enabled=false. Run `ledgerful sync setup` for a readiness checklist. See docs/team-sync.md.",
        ));
    }
    if config.sync.target.trim().is_empty() {
        findings.push(DoctorFinding::warn(
            "sync-enabled-empty-target",
            DoctorCategory::Optional,
            "[sync].enabled=true but [sync].target is empty. Set a shared-folder target (e.g. dir:///path) or disable sync. Run `ledgerful sync setup` for a readiness checklist. See docs/team-sync.md.",
        ));
    }
    // 0111: enabled with zero trusted peers — actionable next step; never sole-blocks publish.
    // Do not treat list IO errors as zero peers (honesty — same class as status R1-04).
    #[cfg(feature = "sync")]
    if keys_ok && sot_ok {
        let sync_dir = layout.state_dir.join("sync");
        match crate::sync::peers::list_peers(sync_dir.as_std_path()) {
            Ok(peers) if peers.is_empty() => {
                findings.push(DoctorFinding::warn(
                    "sync-enabled-no-peers",
                    DoctorCategory::Optional,
                    "[sync].enabled=true but no trusted peers under sync/peers/. Exchange LF-PAIR-1 invites with `ledgerful sync pair` (mutual accept) or disable sync. Run `ledgerful sync setup` for a readiness checklist. See docs/team-sync.md.",
                ));
            }
            Ok(_) => {}
            Err(e) => {
                findings.push(DoctorFinding::warn(
                    "sync-peers-list-error",
                    DoctorCategory::Optional,
                    format!(
                        "[sync].enabled=true but trusted peer list could not be read: {e}. Check permissions on sync/peers/. Run `ledgerful sync setup` for a readiness checklist. See docs/team-sync.md."
                    ),
                ));
            }
        }
    }
    findings
}

/// Per-language SCIP capability + process-policy report for doctor (0095/0109).
///
/// Structured findings with new `scip-*` codes; severity Info, category Optional.
/// Go note is always included. Never blocks publish readiness or dashboard failures.
fn collect_scip_findings(config: &crate::config::model::Config) -> Vec<DoctorFinding> {
    use crate::platform::process_policy::check_policy;
    use crate::scip::ScipToolchain;

    let policy = config.verify.effective_process_policy();
    let mut findings = Vec::new();
    for (tool, available) in ScipToolchain::probe_all_languages() {
        let lang = tool.language_label().to_ascii_lowercase();
        if available {
            match check_policy(tool.exe_name(), &policy) {
                Ok(()) => findings.push(DoctorFinding::info(
                    format!("scip-{lang}-available"),
                    DoctorCategory::Optional,
                    format!(
                        "SCIP {}: {} available — `ledgerful index --auto-scip` can add reference edges on native symbols",
                        tool.language_label(),
                        tool.exe_name()
                    ),
                )),
                Err(e) => findings.push(DoctorFinding::info(
                    format!("scip-{lang}-policy-blocked"),
                    DoctorCategory::Optional,
                    format!(
                        "SCIP {}: {} present but blocked by process policy ({e}) — adjust verify.allowed_commands / denied_commands or install is not enough for --auto-scip",
                        tool.language_label(),
                        tool.exe_name()
                    ),
                )),
            }
        } else {
            findings.push(DoctorFinding::info(
                format!("scip-{lang}-missing"),
                DoctorCategory::Optional,
                format!(
                    "SCIP {}: {} not available (capability probe). Install with `{}` to enable cross-file references via --auto-scip",
                    tool.language_label(),
                    tool.exe_name(),
                    tool.install_hint()
                ),
            ));
        }
    }
    // Go: upstream indexer exists, not wired in this track (spec §2.11 / §4)
    findings.push(DoctorFinding::info(
        "scip-go-not-wired",
        DoctorCategory::Optional,
        "SCIP Go: upstream scip-go exists, not wired here — native Go tree-sitter path only",
    ));
    findings.sort_by(|a, b| a.code.cmp(&b.code).then(a.message.cmp(&b.message)));
    findings
}

/// Map embedding backend availability to an optional finding (0109).
fn embedding_finding(
    config: &crate::config::model::LocalModelConfig,
    avail: &BackendAvailabilityReport,
) -> Option<DoctorFinding> {
    use crate::embed::client::is_embedding_backend_configured;
    use crate::semantic::BackendStatus;

    match avail.status {
        BackendStatus::Ready => None,
        BackendStatus::NotConfigured => {
            // Partial: model name set without URL → warn; fully empty → info.
            let partial = !config.embedding_model.trim().is_empty()
                && !is_embedding_backend_configured(config);
            if partial {
                Some(DoctorFinding::warn(
                    "embed-partial-config",
                    DoctorCategory::Optional,
                    "Embedding model partially configured (model name set without URL) — not healthy Ready",
                ))
            } else {
                Some(DoctorFinding::info(
                    "embed-not-configured",
                    DoctorCategory::Optional,
                    "Embedding model not configured",
                ))
            }
        }
        BackendStatus::Unreachable => Some(DoctorFinding::warn(
            "embed-unreachable",
            DoctorCategory::Optional,
            "Embedding model unreachable",
        )),
    }
}

/// Result of a doctor availability probe for an optional/advertised backend.
///
/// **DoD-11 seam for 0095/0109:** SCIP and other optional toolchains reuse this
/// shape. `is_failure` means the backend is **not Ready** for display honesty
/// (0096 partial-config); severity lives on structured [`DoctorFinding`]s
/// (optional category — never blocks publish, never dashboard failures alone).
#[derive(Debug, Clone)]
pub struct BackendAvailabilityReport {
    /// Colored/human display string for the doctor line.
    pub display: String,
    /// Whether the backend is not Ready (display honesty; not soft-fail count).
    pub is_failure: bool,
    /// Orthogonal backend axis (mirrors semantic readiness).
    pub status: crate::semantic::BackendStatus,
    /// Full error detail for debug logging (not shown to user).
    pub debug_detail: Option<String>,
}

/// Format embedding-backend availability for doctor (DoD-6, DoD-11).
///
/// Gates on URL emptiness via `is_embedding_backend_configured` — **not**
/// on `embedding_model.is_empty()` alone — so partial config (model name
/// set, no URL) reports "Not configured" rather than a healthy
/// `(0 dims) @ `.
fn format_embedding_backend_availability(
    display_config: &crate::config::model::LocalModelConfig,
    probe_config: &crate::config::model::LocalModelConfig,
) -> BackendAvailabilityReport {
    use crate::embed::client::is_embedding_backend_configured;
    use crate::semantic::BackendStatus;
    use owo_colors::{OwoColorize, Stream};

    if !is_embedding_backend_configured(display_config) {
        return BackendAvailabilityReport {
            display: "Not configured"
                .if_supports_color(Stream::Stdout, |s| s.yellow())
                .to_string(),
            is_failure: true,
            status: BackendStatus::NotConfigured,
            debug_detail: None,
        };
    }

    let endpoint = display_config
        .embedding_url
        .as_deref()
        .unwrap_or(&display_config.base_url);

    match probe_with_retry(|| crate::embed::client::check_local_model(probe_config)) {
        ProbeResult::Healthy(dims) if dims.active => BackendAvailabilityReport {
            display: format!(
                "{} ({} dims) @ {}",
                if display_config.embedding_model.is_empty() {
                    dims.model_name.as_str()
                } else {
                    display_config.embedding_model.as_str()
                },
                dims.dimensions,
                endpoint
            ),
            is_failure: false,
            status: BackendStatus::Ready,
            debug_detail: None,
        },
        ProbeResult::Healthy(_dims) => {
            // URL set but probe returned inactive (0 dims) — treat as not ready.
            BackendAvailabilityReport {
                display: "Not configured"
                    .if_supports_color(Stream::Stdout, |s| s.yellow())
                    .to_string(),
                is_failure: true,
                status: BackendStatus::NotConfigured,
                debug_detail: Some(
                    "Probe returned inactive dimensions despite URL being set".to_string(),
                ),
            }
        }
        ProbeResult::ReachableAfterRetry { val: dims, retries } if dims.active => {
            BackendAvailabilityReport {
                display: format!(
                    "{} ({} dims) @ {} (reachable after retry: flaky/transient - {})",
                    if display_config.embedding_model.is_empty() {
                        dims.model_name.as_str()
                    } else {
                        display_config.embedding_model.as_str()
                    },
                    dims.dimensions,
                    endpoint,
                    format!(
                        "{} {}",
                        retries,
                        if retries == 1 { "retry" } else { "retries" }
                    )
                    .if_supports_color(Stream::Stdout, |s| s.green())
                ),
                is_failure: false,
                status: BackendStatus::Ready,
                debug_detail: None,
            }
        }
        ProbeResult::ReachableAfterRetry { .. } => BackendAvailabilityReport {
            display: "Not configured"
                .if_supports_color(Stream::Stdout, |s| s.yellow())
                .to_string(),
            is_failure: true,
            status: BackendStatus::NotConfigured,
            debug_detail: None,
        },
        ProbeResult::Unreachable { err, retries } => {
            let retry_suffix = if retries > 0 {
                format!(" after {} retries", retries)
            } else {
                String::new()
            };
            let truncated: String = err.chars().take(80).collect();
            let detail_hint = if err.chars().count() > 80 {
                " [set RUST_LOG=debug for details]"
            } else {
                ""
            };
            BackendAvailabilityReport {
                display: format!(
                    "unreachable ({}{}){}",
                    truncated.if_supports_color(Stream::Stdout, |s| s.yellow()),
                    retry_suffix,
                    detail_hint
                ),
                is_failure: true,
                status: BackendStatus::Unreachable,
                debug_detail: Some(err),
            }
        }
    }
}

/// Persist `doctor-results.json` for the web dashboard health score and
/// change-context `doctor.topFindings` (0129).
///
/// Schema (0109 + 0129 additive):
/// ```json
/// {
///   "failures": N,
///   "timestamp": "RFC3339",
///   "readyForPublish": bool,
///   "block": u64,
///   "warn": u64,
///   "info": u64,
///   "findings": [
///     { "code", "severity", "message", "remediation"? }
///   ]
/// }
/// ```
///
/// **`failures`** = [`dashboard_failures`] — `count(block) + count(warn where
/// category != optional)`. Optional backends never contribute. Dashboard/health
/// readers still use only `failures` / counts; unknown `findings` is ignored.
///
/// **`findings`** (0129): top-N block/warn for agent packets — **no category
/// filter** (optional-category warns appear). Independent severity-first re-sort
/// (block before warn, then code, then message) before cap 5. Info excluded.
/// Optional `remediation` when present (never null).
///
/// Legacy `results: [{passed}]` array shape is accepted on read only (writers
/// no longer emit it).
///
/// Returns `Err` on I/O failure; the caller logs a warning and continues.
fn write_doctor_results(layout: &Layout, findings: &[DoctorFinding]) -> Result<()> {
    let counts = summarize(findings);
    let failures = dashboard_failures(findings);
    let top = select_sidecar_top_findings(findings);
    let findings_json: Vec<serde_json::Value> = top
        .into_iter()
        .map(|f| {
            let mut obj = serde_json::Map::new();
            obj.insert("code".into(), json!(f.code));
            obj.insert("severity".into(), json!(f.severity.as_str()));
            obj.insert("message".into(), json!(f.message));
            if let Some(ref rem) = f.remediation {
                obj.insert("remediation".into(), json!(rem));
            }
            serde_json::Value::Object(obj)
        })
        .collect();
    let body = json!({
        "failures": failures,
        "timestamp": Utc::now().to_rfc3339(),
        "readyForPublish": ready_for_publish(findings),
        "block": counts.block,
        "warn": counts.warn,
        "info": counts.info,
        "findings": findings_json,
    });
    let path = layout.state_subdir().join("doctor-results.json");
    std::fs::write(
        path.as_std_path(),
        serde_json::to_vec_pretty(&body).into_diagnostic()?,
    )
    .into_diagnostic()?;
    Ok(())
}

/// Select top-N block/warn findings for the doctor sidecar (0129).
///
/// Filter: severity `block` or `warn` only — **no category filter**.
/// Sort: block before warn, then code, then message — **before** take(5).
/// Distinct from [`dashboard_failures`] (which excludes optional-category warns).
fn select_sidecar_top_findings(findings: &[DoctorFinding]) -> Vec<&DoctorFinding> {
    let mut selected: Vec<&DoctorFinding> = findings
        .iter()
        .filter(|f| matches!(f.severity, DoctorSeverity::Block | DoctorSeverity::Warn))
        .collect();
    selected.sort_by(|a, b| {
        sidecar_severity_rank(a.severity)
            .cmp(&sidecar_severity_rank(b.severity))
            .then_with(|| a.code.cmp(&b.code))
            .then_with(|| a.message.cmp(&b.message))
    });
    selected.into_iter().take(5).collect()
}

fn sidecar_severity_rank(severity: DoctorSeverity) -> u8 {
    match severity {
        DoctorSeverity::Block => 0,
        DoctorSeverity::Warn => 1,
        DoctorSeverity::Info => 2,
    }
}

#[derive(Debug)]
enum ProbeResult<T> {
    Healthy(T),
    ReachableAfterRetry { val: T, retries: u32 },
    Unreachable { err: String, retries: u32 },
}

fn is_transient_error(err: &str) -> bool {
    let err_lower = err.to_lowercase();
    if err_lower.contains("unreachable")
        || err_lower.contains("timed out")
        || err_lower.contains("timeout")
    {
        return true;
    }
    if err_lower.contains("502") || err_lower.contains("503") || err_lower.contains("504") {
        return true;
    }
    false
}

/// Total wall-clock time `probe_with_retry` is allowed to spend sleeping
/// between retries, per probe. `doctor` is a session-start health check
/// (see `conductor/trackCG-F32/spec.md` requirement #4: "Keep doctor
/// read-only and concise"), so this is intentionally small: 1.5s is
/// enough for a couple of quick retries to catch a genuine flap (a
/// service that comes back up after one or two blips) without letting a
/// fully-down endpoint turn a "fast health check" into a multi-second
/// stall. This budget bounds only the *sleep* time between retries, not
/// the per-attempt network timeout (`model_config.timeout_secs`).
const RETRY_BUDGET: std::time::Duration = std::time::Duration::from_millis(1500);

/// Delay between retry attempts. Kept short relative to `RETRY_BUDGET` so
/// multiple retries can still fit inside the budget.
const RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(200);

fn probe_with_retry<T, F>(probe_fn: F) -> ProbeResult<T>
where
    T: std::marker::Send + 'static,
    F: FnMut() -> Result<T, String> + std::marker::Send,
{
    probe_with_retry_budgeted(probe_fn, RETRY_BUDGET, RETRY_DELAY)
}

/// Core retry loop, parameterized by retry budget and inter-retry delay
/// so tests can exercise the deadline logic with tiny durations instead
/// of waiting through the real (small but nonzero) production budget.
///
/// Retries on transient errors (per `is_transient_error`) continue only
/// while the elapsed wall-clock time spent in this call is still under
/// `budget`; once the budget is exhausted, the probe returns
/// `Unreachable` immediately with however many retries were actually
/// attempted, rather than sleeping/retrying further. Non-transient
/// ("semantic") errors always fail immediately with zero retries.
fn probe_with_retry_budgeted<T, F>(
    mut probe_fn: F,
    budget: std::time::Duration,
    delay: std::time::Duration,
) -> ProbeResult<T>
where
    T: std::marker::Send + 'static,
    F: FnMut() -> Result<T, String> + std::marker::Send,
{
    let start = std::time::Instant::now();
    let mut retries = 0;
    // TA15 R4: Per-attempt hard deadline so DNS-level hangs cannot stall
    // doctor indefinitely. The inner ureq timeouts (timeout_connect +
    // timeout_read) fire first when possible; this thread-based deadline
    // covers the entire request lifecycle including DNS resolution.
    let per_attempt_deadline = std::time::Duration::from_secs(10);

    loop {
        // Wrap the probe call in a thread + recv_timeout so a hung DNS
        // resolution or TCP connect cannot stall doctor indefinitely.
        let (tx, rx) = std::sync::mpsc::channel::<Result<T, String>>();
        std::thread::scope(|s| {
            s.spawn(|| {
                let _ = tx.send(probe_fn());
            });
        });

        let probe_result = match rx.recv_timeout(per_attempt_deadline) {
            Ok(result) => result,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => Err(format!(
                "probe timed out after {}s",
                per_attempt_deadline.as_secs()
            )),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                Err("probe thread panicked".to_string())
            }
        };

        match probe_result {
            Ok(val) => {
                if retries > 0 {
                    return ProbeResult::ReachableAfterRetry { val, retries };
                } else {
                    return ProbeResult::Healthy(val);
                }
            }
            Err(err) => {
                let elapsed = start.elapsed();
                if is_transient_error(&err) && elapsed + delay <= budget {
                    retries += 1;
                    std::thread::sleep(delay);
                    continue;
                }
                return ProbeResult::Unreachable { err, retries };
            }
        }
    }
}

fn format_active_ask_backend(config: &crate::config::model::Config) -> String {
    format_active_ask_backend_with(config, &|name| std::env::var(name).ok(), &|name| {
        crate::config::model::read_env_key(name)
    })
}

fn format_active_ask_backend_with(
    config: &crate::config::model::Config,
    env_reader: &dyn Fn(&str) -> Option<String>,
    dotenv_reader: &dyn Fn(&str) -> Option<String>,
) -> String {
    // If user configured a provider priority list, show the full chain
    // with model names (TA14 R6). Uses resolve_provider_entries so env var
    // overrides (LEDGERFUL_ASK_MODEL_N) are reflected in the display.
    if !config.ask.providers.priority.is_empty()
        && let Ok(entries) = crate::commands::ask::resolve_provider_entries(config, None)
        && !entries.is_empty()
    {
        let names: Vec<String> = entries
            .iter()
            .map(|e| {
                let model = e.model.as_deref().unwrap_or("");
                if model.is_empty() {
                    e.backend.display_name().to_string()
                } else {
                    format!("{} ({})", e.backend.display_name(), model)
                }
            })
            .collect();
        return names.join(" → ");
    }

    // Legacy display when no provider priority list is configured.
    use crate::commands::ask::{Backend, resolve_backend_with};
    let resolved = resolve_backend_with(config, None, env_reader, dotenv_reader);
    match resolved {
        Backend::Gemini => "Gemini (Cloud)".to_string(),
        Backend::Local | Backend::OllamaCloud | Backend::OpenRouter => {
            let base_url = config
                .local_model
                .generation_url
                .as_deref()
                .unwrap_or(&config.local_model.base_url);
            if base_url.is_empty() {
                "Local (127.0.0.1)".to_string()
            } else {
                let host = parse_url_host(base_url).unwrap_or_else(|| "127.0.0.1".to_string());
                format!("Local ({})", host)
            }
        }
    }
}

enum GateModeOutcome {
    Ok(String),
    NoHistory(String),
    Mismatch {
        display: String,
        finding: DoctorFinding,
    },
}

/// Gate mode vs ledger history. Mismatch → warn finding; ok/no-history omit finding.
fn gate_mode_status(
    layout: &crate::state::layout::Layout,
    config: &crate::config::model::Config,
) -> GateModeOutcome {
    let effective_mode = config.gate.mode.clone();
    let ledger_mode = crate::ledger::mode_history::current_mode_from_ledger(layout);

    match ledger_mode {
        Some(ledger_mode) if ledger_mode == effective_mode => GateModeOutcome::Ok(format!(
            "Gate mode: {} (matches ledger history)",
            effective_mode
        )),
        Some(ledger_mode) => {
            let message = format!(
                "Gate mode: {effective_mode} (ledger history shows {ledger_mode}; run `ledgerful gate mode {ledger_mode}`)"
            );
            GateModeOutcome::Mismatch {
                display: message
                    .clone()
                    .if_supports_color(Stream::Stdout, |s| s.yellow())
                    .to_string(),
                finding: DoctorFinding::warn("gate-mode-mismatch", DoctorCategory::Gate, message),
            }
        }
        None => GateModeOutcome::NoHistory(format!(
            "Gate mode: {} (no ledger transition history yet)",
            effective_mode
        )),
    }
}

fn parse_url_host(url: &str) -> Option<String> {
    let trimmed = url.trim();
    let without_scheme = trimmed
        .strip_prefix("http://")
        .or_else(|| trimmed.strip_prefix("https://"))?;
    let authority = without_scheme.split('/').next()?;
    let host = authority.split(':').next()?;
    if host.is_empty() {
        None
    } else {
        Some(host.to_string())
    }
}

/// WARN when a worktree-local `ledger.db` exists and is not the resolved shared DB.
/// Detection only — never deletes or merges orphan state (0108 DoD-7).
pub(crate) fn split_brain_ledger_finding(layout: &Layout) -> Option<DoctorFinding> {
    use crate::state::layout::{STATE_DIR, STATE_SUBDIR};

    let local_db = layout
        .root
        .join(STATE_DIR)
        .join(STATE_SUBDIR)
        .join("ledger.db");
    if !local_db.is_file() {
        return None;
    }
    let shared_db = layout.state_subdir().join("ledger.db");
    let local_canon = dunce::canonicalize(local_db.as_std_path()).ok();
    let shared_canon = dunce::canonicalize(shared_db.as_std_path()).ok();
    match (local_canon, shared_canon) {
        (Some(local), Some(shared)) if local == shared => None,
        _ => Some(DoctorFinding::warn(
            "worktree-split-brain",
            DoctorCategory::Layout,
            format!(
                "local ledger.db at {local_db} exists and differs from resolved shared state \
                 {shared_db}; linked worktrees share the main worktree's `.ledgerful` \
                 — remove the orphan local state only after confirming it is unused"
            ),
        )),
    }
}

/// Back-compat alias used by 0108 tests that assert message content.
#[cfg(test)]
pub(crate) fn split_brain_ledger_warning(layout: &Layout) -> Option<String> {
    split_brain_ledger_finding(layout).map(|f| format!("Warning [{}]: {}", f.code, f.message))
}

fn print_vram_section() {
    #[cfg(target_os = "windows")]
    {
        use crate::platform::gpu::{VramPressure, classify, query_vram_usage};
        match query_vram_usage() {
            Ok(info) => {
                let usage_gb = info.current_usage as f64 / 1_073_741_824.0;
                let budget_gb = info.budget_bytes as f64 / 1_073_741_824.0;
                let pressure = classify(&info);

                let is_arc = info.adapter_name.to_lowercase().contains("arc");
                let note = if is_arc && info.current_usage == 0 {
                    " (Driver limitation: zero-usage reporting on Intel Arc)"
                        .if_supports_color(Stream::Stdout, |s| s.yellow())
                        .to_string()
                } else {
                    "".to_string()
                };

                let usage_str = format!("{:.1}", usage_gb);
                let color_usage = match pressure {
                    VramPressure::Ok => usage_str
                        .if_supports_color(Stream::Stdout, |s| s.white())
                        .to_string(),
                    VramPressure::High => usage_str
                        .if_supports_color(Stream::Stdout, |s| {
                            s.style(Style::new().yellow().bold())
                        })
                        .to_string(),
                    VramPressure::Critical => usage_str
                        .if_supports_color(Stream::Stdout, |s| s.style(Style::new().red().bold()))
                        .to_string(),
                };
                println!(
                    "{:<20} {} GB / {:.1} GB{}",
                    "GPU VRAM:".if_supports_color(Stream::Stdout, |s| s.bold()),
                    color_usage,
                    budget_gb,
                    note
                );
            }
            Err(e) => println!(
                "{:<20} unavailable ({})",
                "GPU VRAM:".if_supports_color(Stream::Stdout, |s| s.bold()),
                e.if_supports_color(Stream::Stdout, |s| s.yellow())
            ),
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        println!("{:<20} n/a (Windows-only monitoring)", "GPU VRAM:");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::human::DoctorReport;
    use crate::platform::env::ExecutableStatus;
    use camino::{Utf8Path, Utf8PathBuf};
    use std::path::PathBuf;

    fn sample_report<'a>(tools: &'a Vec<(String, ExecutableStatus)>) -> DoctorReport<'a> {
        DoctorReport {
            platform: "test",
            shell: "test",
            tools,
            path_display: "test",
            path_kind: "test",
            work_root: "test",
            state_dir: "test/.ledgerful",
            is_wsl_mounted: false,
            embedding_model_status: "OK".to_string(),
            embedding_model_failed: false,
            completion_model_status: "OK".to_string(),
            native_graph_status: "Ready (CozoDB active)".to_string(),
            active_ask_backend: "Gemini (Cloud)".to_string(),
            // 0126: never embed healthy OK-with-zero; fixture uses positive N.
            index_health: vec!["Search index: OK (12 documents)".to_string()],
            target_triple: "test",
        }
    }

    #[test]
    fn doctor_json_plus_apply_hook_refresh_rejected() {
        use crate::cli::Cli;
        use clap::Parser;
        // Rejected at execute_doctor entry (also covered by clap path when wired).
        let err = execute_doctor(true, true, false).unwrap_err();
        let msg = format!("{err:?}");
        assert!(
            msg.contains("doctor --json cannot be combined with --apply-hook-refresh"),
            "got {msg}"
        );
        // clap accepts the flags; rejection is in execute_doctor.
        let parsed = Cli::try_parse_from(["ledgerful", "doctor", "--json", "--apply-hook-refresh"]);
        assert!(parsed.is_ok(), "flags are parseable; combo rejected later");
    }

    #[test]
    fn doctor_summary_four_way_priority() {
        use crate::output::human::format_doctor_summary_text;
        assert_eq!(
            format_doctor_summary_text(2, 5, 3),
            "✗ Doctor: 2 block issue(s)"
        );
        assert_eq!(
            format_doctor_summary_text(0, 3, 2),
            "✓ Doctor: ready for publish env · 3 warning(s)"
        );
        assert_eq!(
            format_doctor_summary_text(0, 0, 4),
            "✓ Doctor: ready for publish env · 4 hint(s)"
        );
        assert_eq!(
            format_doctor_summary_text(0, 0, 0),
            "✓ Doctor: all checks passed"
        );
        // Block wins; warn never uses red soft-fail "issue(s) found".
        assert!(!format_doctor_summary_text(0, 1, 9).contains("issue(s) found"));
        assert!(format_doctor_summary_text(0, 1, 9).contains("ready for publish"));
    }

    #[test]
    fn chain_checkpoint_practice_finding_signed_info_never_blocks() {
        let finding = DoctorFinding::info(
            "chain-checkpoint-practice",
            DoctorCategory::Optional,
            "Signed chain head present. Periodically run `ledgerful export head`...",
        );
        assert_eq!(finding.code, "chain-checkpoint-practice");
        assert_eq!(finding.severity, DoctorSeverity::Info);
        assert_eq!(finding.category, DoctorCategory::Optional);
        assert_eq!(dashboard_failures(std::slice::from_ref(&finding)), 0);
        assert!(ready_for_publish(std::slice::from_ref(&finding)));
    }

    #[test]
    fn chain_checkpoint_practice_finding_none_without_signed_head() {
        // Empty in-memory DB: no chain_head row → no finding.
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("ledger.db");
        let conn = rusqlite::Connection::open(&db_path).expect("open");
        conn.execute_batch(
            "CREATE TABLE chain_head (
                id INTEGER PRIMARY KEY CHECK (id = 1),
                latest_entry_hash TEXT NOT NULL,
                genesis TEXT NOT NULL,
                length INTEGER NOT NULL,
                head_signature TEXT,
                head_public_key TEXT,
                updated_at TEXT NOT NULL
            );",
        )
        .expect("schema");
        assert!(chain_checkpoint_practice_finding(&conn).is_none());

        conn.execute(
            "INSERT INTO chain_head (id, latest_entry_hash, genesis, length, head_signature, head_public_key, updated_at)
             VALUES (1, 'h', 'g', 1, NULL, NULL, '2026-01-01T00:00:00Z')",
            [],
        )
        .expect("unsigned head");
        assert!(
            chain_checkpoint_practice_finding(&conn).is_none(),
            "unsigned head must not emit practice finding"
        );

        conn.execute(
            "UPDATE chain_head SET head_signature = 'sig', head_public_key = 'pk'",
            [],
        )
        .expect("sign");
        let f = chain_checkpoint_practice_finding(&conn).expect("signed head finding");
        assert_eq!(f.code, "chain-checkpoint-practice");
        assert_eq!(f.severity, DoctorSeverity::Info);
        assert_eq!(f.category, DoctorCategory::Optional);
        assert!(f.message.contains("export head"));
        assert!(f.message.contains("against-export"));
        assert_eq!(dashboard_failures(std::slice::from_ref(&f)), 0);
        assert!(ready_for_publish(std::slice::from_ref(&f)));
    }

    #[test]
    fn classify_optional_warn_excluded_from_dashboard_failures() {
        let findings = vec![
            DoctorFinding::warn("sig-pin", DoctorCategory::Signing, SIG_PIN_WARNING),
            DoctorFinding::info("sccache-hint", DoctorCategory::Optional, "sccache hint"),
            DoctorFinding::info(
                "scip-go-not-wired",
                DoctorCategory::Optional,
                "go not wired",
            ),
            DoctorFinding::warn("embed-unreachable", DoctorCategory::Optional, "embed down"),
        ];
        assert_eq!(dashboard_failures(&findings), 1); // sig-pin only
        assert!(ready_for_publish(&findings));
        let s = summarize(&findings);
        assert_eq!(s.warn, 2);
        assert_eq!(s.info, 2);
        assert_eq!(s.block, 0);
    }

    /// 0100 DoD-3 / F-002: verify + doctor share unknown-key / pin / trusted terms.
    /// 0125: builder remediation carries hex + PowerShell-safe outer single quotes.
    #[test]
    fn dod3_unknown_key_vocabulary_shared_across_verify_and_doctor() {
        use crate::ledger::crypto::SignatureTrustStatus;

        let verify_status = SignatureTrustStatus::ValidUnknownKey.as_str();
        assert!(
            verify_status.to_ascii_lowercase().contains("unknown key"),
            "ValidUnknownKey must contain 'unknown key': {verify_status}"
        );

        let doctor = SIG_PIN_WARNING;
        let doctor_lc = doctor.to_ascii_lowercase();
        assert!(
            doctor_lc.contains("unknown key"),
            "doctor sig-pin must contain 'unknown key': {doctor}"
        );
        assert!(
            doctor_lc.contains("pin") || doctor.contains("Pin"),
            "doctor sig-pin must mention pin: {doctor}"
        );
        assert!(
            doctor_lc.contains("trusted") || doctor_lc.contains("trusted_public_keys"),
            "doctor sig-pin must mention trusted keys: {doctor}"
        );

        // Builder finding keeps vocabulary and adds structured remediation.
        let hex = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let finding = build_sig_pin_finding(Some(hex));
        let msg_lc = finding.message.to_ascii_lowercase();
        assert!(msg_lc.contains("unknown key"));
        assert!(msg_lc.contains("pin") || finding.message.contains("Pin"));
        assert!(msg_lc.contains("trusted"));
        let rem = finding.remediation.expect("remediation Some");
        assert!(
            rem.contains(&format!("'intent.trusted_public_keys=[\"{hex}\"]'")),
            "outer single quotes + hex: {rem}"
        );
    }

    #[test]
    fn test_dashboard_failures_clean() {
        let findings: Vec<DoctorFinding> = Vec::new();
        assert_eq!(dashboard_failures(&findings), 0);
        assert!(ready_for_publish(&findings));
    }

    #[test]
    fn test_dashboard_failures_formula_samples() {
        // Optional backends (info/warn) excluded; index warn + block included.
        let findings = vec![
            DoctorFinding::block("tool-git", DoctorCategory::Tools, "git missing"),
            DoctorFinding::warn(
                "search-corrupt",
                DoctorCategory::Index,
                "Search index corrupt",
            ),
            DoctorFinding::warn("graph-stale", DoctorCategory::Index, "Graph STALE"),
            DoctorFinding::info(
                "embed-not-configured",
                DoctorCategory::Optional,
                "embed not configured",
            ),
            DoctorFinding::warn(
                "completion-unreachable",
                DoctorCategory::Optional,
                "completion down",
            ),
            DoctorFinding::info(
                "graph-not-initialized",
                DoctorCategory::Index,
                "graph not init",
            ),
            DoctorFinding::warn("impact-corrupt", DoctorCategory::Index, "impact corrupt"),
        ];
        // block + search-corrupt + graph-stale + impact-corrupt = 4
        assert_eq!(dashboard_failures(&findings), 4);
        assert!(!ready_for_publish(&findings));
    }

    #[test]
    fn test_optional_not_configured_not_dashboard_failure() {
        let findings = vec![
            DoctorFinding::info(
                "embed-not-configured",
                DoctorCategory::Optional,
                "not configured",
            ),
            DoctorFinding::info(
                "completion-not-configured",
                DoctorCategory::Optional,
                "not configured",
            ),
            DoctorFinding::info("tool-gemini", DoctorCategory::Optional, "gemini missing"),
        ];
        assert_eq!(dashboard_failures(&findings), 0);
        assert!(ready_for_publish(&findings));
    }

    /// DoD-6: partial config (model name set, base_url empty) is Not configured
    /// and counted as a failure — not a healthy `(0 dims) @ `.
    #[test]
    fn format_embedding_partial_config_is_not_configured_failure() {
        use crate::config::model::LocalModelConfig;
        use crate::semantic::BackendStatus;

        let config = LocalModelConfig {
            embedding_model: "nomic-embed-text".to_string(),
            base_url: String::new(),
            embedding_url: None,
            dimensions: 768,
            ..Default::default()
        };
        let report = format_embedding_backend_availability(&config, &config);
        assert_eq!(report.status, BackendStatus::NotConfigured);
        assert!(report.is_failure);
        assert!(
            report.display.contains("Not configured"),
            "partial config must not look healthy, got: {}",
            report.display
        );
        assert!(
            !report.display.contains("0 dims"),
            "must not print healthy-looking (0 dims) @ for partial config, got: {}",
            report.display
        );

        // Partial config → optional warn finding; never dashboard failures / never block.
        let finding = embedding_finding(&config, &report).expect("partial finding");
        assert_eq!(finding.code, "embed-partial-config");
        assert_eq!(finding.severity, DoctorSeverity::Warn);
        assert_eq!(finding.category, DoctorCategory::Optional);
        assert_eq!(dashboard_failures(std::slice::from_ref(&finding)), 0);
        assert!(ready_for_publish(std::slice::from_ref(&finding)));
    }

    /// DoD-6: fully empty config (default install) is also Not configured.
    #[test]
    fn format_embedding_default_config_is_not_configured() {
        use crate::config::model::LocalModelConfig;
        use crate::semantic::BackendStatus;

        let config = LocalModelConfig::default();
        let report = format_embedding_backend_availability(&config, &config);
        assert_eq!(report.status, BackendStatus::NotConfigured);
        assert!(report.is_failure);
        assert!(
            report.display.contains("Not configured"),
            "got: {}",
            report.display
        );
    }

    /// 0095 DoD-13 / 0109: SCIP findings are info/optional; never block or dashboard.
    #[test]
    fn scip_findings_sorted_and_mention_go_unwired() {
        let config = crate::config::model::Config::default();
        let findings = collect_scip_findings(&config);
        assert!(!findings.is_empty(), "expected at least Go unwired line");
        let mut sorted = findings.clone();
        sorted.sort_by(|a, b| a.code.cmp(&b.code).then(a.message.cmp(&b.message)));
        assert_eq!(findings, sorted, "findings must be sorted");
        assert!(
            findings
                .iter()
                .any(|f| f.code == "scip-go-not-wired" || f.message.contains("not wired")),
            "must report Go as upstream/not wired: {findings:?}"
        );
        for f in &findings {
            assert_eq!(f.severity, DoctorSeverity::Info);
            assert_eq!(f.category, DoctorCategory::Optional);
        }
        assert_eq!(dashboard_failures(&findings), 0);
        assert!(ready_for_publish(&findings));
    }

    /// Doctor must not advertise SCIP as runnable when process policy denies it.
    #[test]
    fn scip_findings_report_policy_block_when_denied() {
        let mut config = crate::config::model::Config::default();
        config.verify.denied_commands = vec!["rust-analyzer".to_string()];
        let findings = collect_scip_findings(&config);
        for f in &findings {
            if f.message.contains("rust-analyzer") || f.message.contains("Rust") {
                assert!(
                    !f.message.contains("available —"),
                    "denied rust-analyzer must not look freely available: {}",
                    f.message
                );
            }
        }
        assert!(
            findings
                .iter()
                .any(|f| f.message.contains("blocked by process policy")
                    || f.message.contains("not available")
                    || f.code == "scip-go-not-wired"),
            "expected policy or probe messaging: {findings:?}"
        );
    }

    #[test]
    fn doctor_zero_dashboard_failures_without_scip_indexers_in_tools() {
        // Indexers must not appear in DoctorReport.tools.
        let tools = vec![(
            "git".to_string(),
            ExecutableStatus::Found(PathBuf::from("git")),
        )];
        let _report = sample_report(&tools);
        assert!(
            !tools
                .iter()
                .any(|(n, _)| n.contains("scip") || n.contains("rust-analyzer"))
        );
        // SCIP absence alone is not a dashboard failure.
        let scip = collect_scip_findings(&crate::config::model::Config::default());
        assert_eq!(dashboard_failures(&scip), 0);
    }

    #[test]
    fn graph_current_status_is_not_a_finding() {
        // Healthy graph status lines are display-only (not findings).
        let findings: Vec<DoctorFinding> = Vec::new();
        assert_eq!(dashboard_failures(&findings), 0);
    }

    #[test]
    fn format_active_ask_backend_prefers_gemini_when_configured() {
        let mut config = crate::config::model::Config::default();
        config.gemini.api_key = Some("AIzaTestKey".to_string());
        config.local_model.base_url = "http://127.0.0.1:8081".to_string();
        // Hermetic readers: Gemini wins via explicit api_key regardless of env.
        assert_eq!(
            format_active_ask_backend_with(&config, &|_| None, &|_| None),
            "Gemini (Cloud)"
        );
    }

    #[test]
    fn format_active_ask_backend_prefers_local_when_configured() {
        let mut config = crate::config::model::Config::default();
        config.local_model.base_url = "http://127.0.0.1:8081".to_string();
        config.local_model.generation_model = "test-model".to_string();
        // Hermetic readers returning None so no ambient GEMINI_API_KEY leaks in.
        assert_eq!(
            format_active_ask_backend_with(&config, &|_| None, &|_| None),
            "Local (127.0.0.1)"
        );
    }

    #[test]
    fn format_active_ask_backend_uses_generation_url_host() {
        let mut config = crate::config::model::Config::default();
        config.local_model.generation_url = Some("https://example.com:8080/v1".to_string());
        config.local_model.generation_model = "test-model".to_string();
        assert_eq!(
            format_active_ask_backend_with(&config, &|_| None, &|_| None),
            "Local (example.com)"
        );
    }

    #[test]
    fn parse_url_host_extracts_host_from_http_and_https() {
        assert_eq!(
            parse_url_host("http://127.0.0.1:8081/v1"),
            Some("127.0.0.1".to_string())
        );
        assert_eq!(
            parse_url_host("https://example.com:8080/path"),
            Some("example.com".to_string())
        );
        assert_eq!(parse_url_host("not-a-url"), None);
        assert_eq!(parse_url_host(""), None);
    }

    #[test]
    fn test_write_doctor_results_writes_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = Utf8Path::from_path(tmp.path()).expect("utf8 path");
        let layout = Layout::new(root);
        layout.ensure_state_dir().expect("ensure_state_dir");

        let findings = vec![
            DoctorFinding::block("tool-git", DoctorCategory::Tools, "git missing"),
            DoctorFinding::info(
                "embed-not-configured",
                DoctorCategory::Optional,
                "embed not configured",
            ),
            DoctorFinding::warn("sig-pin", DoctorCategory::Signing, SIG_PIN_WARNING),
        ];
        write_doctor_results(&layout, &findings).expect("write_doctor_results");

        let path = layout.state_subdir().join("doctor-results.json");
        let body = std::fs::read_to_string(path.as_std_path()).expect("read back");
        let json: serde_json::Value = serde_json::from_str(&body).expect("parse");
        // failures = block(1) + non-optional warn sig-pin(1) = 2; optional excluded
        assert_eq!(json["failures"].as_u64(), Some(2));
        assert_eq!(json["readyForPublish"], false);
        assert_eq!(json["block"].as_u64(), Some(1));
        assert_eq!(json["warn"].as_u64(), Some(1));
        assert_eq!(json["info"].as_u64(), Some(1));
        assert!(json["timestamp"].as_str().is_some());
        assert!(json.get("readyForPublishDefinition").is_none());
        // 0129: findings top-N — block+warn only, block first, info excluded
        let findings_arr = json["findings"].as_array().expect("findings array present");
        assert_eq!(findings_arr.len(), 2);
        assert_eq!(findings_arr[0]["code"], "tool-git");
        assert_eq!(findings_arr[0]["severity"], "block");
        assert_eq!(findings_arr[1]["code"], "sig-pin");
        assert_eq!(findings_arr[1]["severity"], "warn");
        assert!(
            findings_arr
                .iter()
                .all(|f| f.get("severity").and_then(|s| s.as_str()) != Some("info")),
            "info must be excluded from findings"
        );
    }

    #[test]
    fn write_doctor_results_optional_only_zero_failures_ready() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = Utf8Path::from_path(tmp.path()).expect("utf8 path");
        let layout = Layout::new(root);
        layout.ensure_state_dir().expect("ensure_state_dir");

        let findings = vec![
            DoctorFinding::info("embed-not-configured", DoctorCategory::Optional, "embed"),
            DoctorFinding::warn(
                "completion-unreachable",
                DoctorCategory::Optional,
                "completion",
            ),
            DoctorFinding::info("tool-gemini", DoctorCategory::Optional, "gemini"),
        ];
        write_doctor_results(&layout, &findings).expect("write");
        let path = layout.state_subdir().join("doctor-results.json");
        let json: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(path.as_std_path()).unwrap()).unwrap();
        assert_eq!(json["failures"].as_u64(), Some(0));
        assert_eq!(json["readyForPublish"], true);
        // 0129: optional-category warn appears in findings even when failures==0
        let findings_arr = json["findings"].as_array().expect("findings array present");
        assert!(
            !findings_arr.is_empty(),
            "optional warn must appear in findings: {findings_arr:?}"
        );
        assert_eq!(findings_arr[0]["code"], "completion-unreachable");
        assert_eq!(findings_arr[0]["severity"], "warn");
        // info excluded
        assert!(
            findings_arr
                .iter()
                .all(|f| f.get("severity").and_then(|s| s.as_str()) != Some("info")),
            "info must be excluded from findings"
        );
    }

    #[test]
    fn write_doctor_results_block_before_warn_under_reverse_alpha_codes() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = Utf8Path::from_path(tmp.path()).expect("utf8 path");
        let layout = Layout::new(root);
        layout.ensure_state_dir().expect("ensure_state_dir");

        // Input order / alpha would put warn "aaa-warn" before block "zzz-block".
        // Severity-first re-sort must place block first before take(5).
        let findings = vec![
            DoctorFinding::warn("aaa-warn", DoctorCategory::Index, "early alpha warn"),
            DoctorFinding::warn("bbb-warn", DoctorCategory::Signing, "mid warn"),
            DoctorFinding::block("zzz-block", DoctorCategory::Tools, "late alpha block"),
            DoctorFinding::info("ccc-info", DoctorCategory::Optional, "info excluded"),
        ];
        write_doctor_results(&layout, &findings).expect("write");
        let path = layout.state_subdir().join("doctor-results.json");
        let json: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(path.as_std_path()).unwrap()).unwrap();
        let findings_arr = json["findings"].as_array().expect("findings");
        assert_eq!(findings_arr.len(), 3, "info excluded → 3 entries");
        assert_eq!(findings_arr[0]["code"], "zzz-block");
        assert_eq!(findings_arr[0]["severity"], "block");
        assert_eq!(findings_arr[1]["code"], "aaa-warn");
        assert_eq!(findings_arr[1]["severity"], "warn");
        assert_eq!(findings_arr[2]["code"], "bbb-warn");
    }

    #[test]
    fn write_doctor_results_remediation_round_trip() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = Utf8Path::from_path(tmp.path()).expect("utf8 path");
        let layout = Layout::new(root);
        layout.ensure_state_dir().expect("ensure_state_dir");

        let findings = vec![
            DoctorFinding::warn("sig-pin", DoctorCategory::Signing, "pin missing")
                .with_remediation("ledgerful config set intent.trusted_public_keys '[\"abc\"]'"),
            DoctorFinding::warn("graph-stale", DoctorCategory::Index, "graph stale"),
        ];
        write_doctor_results(&layout, &findings).expect("write");
        let path = layout.state_subdir().join("doctor-results.json");
        let json: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(path.as_std_path()).unwrap()).unwrap();
        let findings_arr = json["findings"].as_array().expect("findings");
        assert_eq!(findings_arr.len(), 2);
        let pin = findings_arr
            .iter()
            .find(|f| f["code"] == "sig-pin")
            .expect("sig-pin present");
        assert_eq!(
            pin["remediation"].as_str(),
            Some("ledgerful config set intent.trusted_public_keys '[\"abc\"]'")
        );
        let stale = findings_arr
            .iter()
            .find(|f| f["code"] == "graph-stale")
            .expect("graph-stale present");
        assert!(
            stale.get("remediation").is_none(),
            "must omit remediation key when None, not emit null: {stale}"
        );
    }

    #[test]
    fn write_doctor_results_findings_cap_five() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = Utf8Path::from_path(tmp.path()).expect("utf8 path");
        let layout = Layout::new(root);
        layout.ensure_state_dir().expect("ensure_state_dir");

        let mut findings = Vec::new();
        for i in 0..4 {
            findings.push(DoctorFinding::block(
                format!("block-{i}"),
                DoctorCategory::Tools,
                format!("block msg {i}"),
            ));
        }
        for i in 0..4 {
            findings.push(DoctorFinding::warn(
                format!("warn-{i}"),
                DoctorCategory::Index,
                format!("warn msg {i}"),
            ));
        }
        write_doctor_results(&layout, &findings).expect("write");
        let path = layout.state_subdir().join("doctor-results.json");
        let json: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(path.as_std_path()).unwrap()).unwrap();
        let findings_arr = json["findings"].as_array().expect("findings");
        assert_eq!(findings_arr.len(), 5);
        // All 4 blocks come first, then first warn alphabetically
        assert!(
            findings_arr
                .iter()
                .take(4)
                .all(|f| f["severity"] == "block"),
            "first 4 must be blocks: {findings_arr:?}"
        );
        assert_eq!(findings_arr[4]["severity"], "warn");
    }

    #[test]
    fn select_sidecar_top_findings_excludes_info() {
        let findings = vec![
            DoctorFinding::info("i1", DoctorCategory::Optional, "info"),
            DoctorFinding::warn("w1", DoctorCategory::Signing, "warn"),
            DoctorFinding::block("b1", DoctorCategory::Tools, "block"),
        ];
        let top = select_sidecar_top_findings(&findings);
        assert_eq!(top.len(), 2);
        assert_eq!(top[0].code, "b1");
        assert_eq!(top[1].code, "w1");
    }

    #[test]
    fn test_is_transient_error() {
        assert!(is_transient_error("unreachable (connection refused)"));
        assert!(is_transient_error("timed out after 2s"));
        assert!(is_transient_error("503 server error (Service Unavailable)"));
        assert!(is_transient_error("502 Bad Gateway"));
        assert!(is_transient_error("504 Gateway Timeout"));

        // Semantic errors should not be transient
        assert!(!is_transient_error("400 server error (pooling type none)"));
        assert!(!is_transient_error("401 server error (Unauthorized)"));
        assert!(!is_transient_error("404 server error (Not Found)"));
        assert!(!is_transient_error("some custom error"));
    }

    #[test]
    fn test_probe_with_retry_healthy() {
        let mut count = 0;
        let res = probe_with_retry(|| {
            count += 1;
            Ok("success")
        });
        assert!(matches!(res, ProbeResult::Healthy("success")));
        assert_eq!(count, 1);
    }

    #[test]
    fn test_probe_with_retry_flaky_success() {
        // Tiny budget, but generous enough relative to the tiny test delay
        // for 2 quick retries to land before the budget is exhausted.
        let budget = std::time::Duration::from_millis(50);
        let delay = std::time::Duration::from_millis(1);
        let mut count = 0;
        let res = probe_with_retry_budgeted(
            || {
                count += 1;
                if count < 3 {
                    Err("unreachable (connection refused)".to_string())
                } else {
                    Ok("success")
                }
            },
            budget,
            delay,
        );
        assert!(matches!(
            res,
            ProbeResult::ReachableAfterRetry {
                val: "success",
                retries: 2
            }
        ));
        assert_eq!(count, 3);
    }

    #[test]
    fn test_probe_with_retry_hard_unreachable() {
        // A probe that always fails transiently must eventually stop
        // retrying once the (tiny, test-only) budget is exhausted, rather
        // than retrying forever. We don't assert an exact retry count
        // since that's now a function of timing, not a fixed counter;
        // instead assert the qualitative bound: at least one attempt, a
        // small number of retries, and the error is preserved verbatim.
        let budget = std::time::Duration::from_millis(20);
        let delay = std::time::Duration::from_millis(5);
        let mut count = 0;
        let res: ProbeResult<()> = probe_with_retry_budgeted(
            || {
                count += 1;
                Err("unreachable (connection refused)".to_string())
            },
            budget,
            delay,
        );
        match res {
            ProbeResult::Unreachable { err, retries } => {
                assert_eq!(err, "unreachable (connection refused)");
                // Budget is small relative to delay, so retries must be bounded.
                assert!(retries <= 10, "retries should stay small: {retries}");
                assert_eq!(
                    count,
                    retries + 1,
                    "count is always retries + 1 initial attempt"
                );
            }
            other => panic!("expected Unreachable, got {other:?}"),
        }
    }

    #[test]
    fn test_probe_with_retry_budget_exhausted_stops_retrying() {
        // With a zero retry budget, a transient failure must return
        // Unreachable after exactly the first attempt with zero retries -
        // i.e. the budget check itself (not just is_transient_error) gates
        // whether a retry happens at all.
        let budget = std::time::Duration::from_millis(0);
        let delay = std::time::Duration::from_millis(1);
        let mut count = 0;
        let res: ProbeResult<()> = probe_with_retry_budgeted(
            || {
                count += 1;
                Err("unreachable (connection refused)".to_string())
            },
            budget,
            delay,
        );
        assert!(
            matches!(res, ProbeResult::Unreachable { ref err, retries: 0 } if err == "unreachable (connection refused)")
        );
        assert_eq!(count, 1);
    }

    #[test]
    fn test_probe_with_retry_wall_clock_bounded() {
        // Regression test for the latency-regression finding: a probe that
        // always fails transiently must not cause probe_with_retry to
        // spend more than a small, bounded amount of wall-clock time
        // sleeping between retries. Uses the tiny test budget (not the
        // real RETRY_BUDGET) so the test itself stays fast; the ceiling
        // is generous relative to that budget to avoid flakiness on a
        // loaded CI machine, while still catching an unbounded-retry
        // regression (which would blow well past it).
        let budget = std::time::Duration::from_millis(50);
        let delay = std::time::Duration::from_millis(5);
        let start = std::time::Instant::now();
        let res: ProbeResult<()> = probe_with_retry_budgeted(
            || Err("unreachable (connection refused)".to_string()),
            budget,
            delay,
        );
        let elapsed = start.elapsed();
        assert!(matches!(res, ProbeResult::Unreachable { .. }));
        assert!(
            elapsed < std::time::Duration::from_millis(500),
            "probe_with_retry_budgeted took {elapsed:?}, expected well under 500ms for a {budget:?} budget"
        );
    }

    #[test]
    fn test_probe_with_retry_semantic_fail_no_retry() {
        let mut count = 0;
        let res: ProbeResult<()> = probe_with_retry(|| {
            count += 1;
            Err("401 server error (Unauthorized)".to_string())
        });
        assert!(
            matches!(res, ProbeResult::Unreachable { ref err, retries: 0 } if err == "401 server error (Unauthorized)")
        );
        assert_eq!(count, 1); // Fail immediately, no retry
    }

    /// DoD-6 / R5: clean repo produces zero legacy-migration findings.
    #[test]
    fn legacy_findings_silent_on_clean_repo() {
        let tmp = tempfile::tempdir().unwrap();
        let root = camino::Utf8Path::from_path(tmp.path()).unwrap();
        let layout = Layout::new(root);
        layout.ensure_state_dir().unwrap();
        std::fs::write(root.join(".gitignore"), "target/\n.ledgerful/\n").unwrap();
        std::fs::create_dir_all(root.join(".git").join("hooks")).unwrap();
        std::fs::write(
            root.join(".git").join("hooks").join("pre-commit"),
            "#!/bin/sh\necho ok\n",
        )
        .unwrap();

        let findings = collect_legacy_migration_findings(root, &layout);
        assert!(
            findings.is_empty(),
            "clean repo must be silent: {findings:?}"
        );
    }

    #[test]
    fn git_missing_is_block_gemini_missing_is_info() {
        // Classification table sample (tools loop identity).
        let git = DoctorFinding::block("tool-git", DoctorCategory::Tools, "git NOT FOUND");
        let gemini =
            DoctorFinding::info("tool-gemini", DoctorCategory::Optional, "gemini NOT FOUND");
        assert_eq!(git.severity, DoctorSeverity::Block);
        assert_eq!(gemini.severity, DoctorSeverity::Info);
        assert!(!ready_for_publish(std::slice::from_ref(&git)));
        assert!(ready_for_publish(std::slice::from_ref(&gemini)));
        assert_eq!(dashboard_failures(&[git, gemini]), 1);
    }

    /// DoD-6: Design-shaped residue produces expected finding categories.
    #[test]
    fn legacy_findings_report_four_surfaces() {
        let tmp = tempfile::tempdir().unwrap();
        let root = camino::Utf8Path::from_path(tmp.path()).unwrap();
        let layout = Layout::new(root);
        // Both dirs present (no-merge case) so legacy state is still visible.
        layout.ensure_state_dir().unwrap();
        let legacy = root.join(crate::state::layout::LEGACY_STATE_DIR);
        std::fs::create_dir_all(legacy.join("state")).unwrap();
        std::fs::write(legacy.join("state").join("ledger.db"), b"x").unwrap();

        // Gitignore only names the legacy path.
        std::fs::write(
            root.join(".gitignore"),
            format!("{}/\n", crate::state::layout::LEGACY_STATE_DIR),
        )
        .unwrap();

        // Legacy hook marker + invocation.
        std::fs::create_dir_all(root.join(".git").join("hooks")).unwrap();
        let brand = crate::state::layout::LEGACY_STATE_DIR.trim_start_matches('.');
        std::fs::write(
            root.join(".git").join("hooks").join("pre-commit"),
            format!(
                "#!/bin/sh\n# {brand}-ledger-gate: x\nif command -v {brand} &>/dev/null; then\n  {brand} ledger status\nfi\n"
            ),
        )
        .unwrap();

        // Unknown config keys.
        std::fs::write(
            layout.config_file(),
            "[core]\nstrict = false\n[totally_unknown_section]\nx = 1\n",
        )
        .unwrap();

        let findings = collect_legacy_migration_findings(root, &layout);
        assert!(
            findings.iter().any(|f| f.code == "legacy-state"),
            "state: {findings:?}"
        );
        assert!(
            findings.iter().any(|f| f.code == "legacy-hooks"),
            "hooks: {findings:?}"
        );
        assert!(
            findings.iter().any(|f| f.code == "legacy-gitignore"),
            "gitignore: {findings:?}"
        );
        assert!(
            findings.iter().any(|f| f.code == "legacy-config"),
            "config: {findings:?}"
        );
        for f in &findings {
            assert_eq!(f.severity, DoctorSeverity::Warn);
            assert_eq!(f.category, DoctorCategory::Migration);
        }
        // Deterministic sort.
        let mut sorted = findings.clone();
        sorted.sort_by(|a, b| a.code.cmp(&b.code).then(a.message.cmp(&b.message)));
        assert_eq!(findings, sorted);
        // Remediation commands named.
        assert!(
            findings
                .iter()
                .any(|f| f.message.contains("update --repair-hooks")),
            "must name repair command: {findings:?}"
        );
    }

    /// DoD-12: documented sequence auto-clears state/hooks/gitignore; config
    /// residual may remain as WARNING with named remediation (spec §4 forbids
    /// auto-rewriting user config). Not a "fully clean doctor" claim.
    #[test]
    fn e2e_four_surface_stale_auto_surfaces_clean_config_may_remain() {
        let tmp = tempfile::tempdir().unwrap();
        let root = camino::Utf8Path::from_path(tmp.path()).unwrap();

        // Surface 1: legacy state dir only (will migrate on load_startup_config).
        let legacy = root.join(crate::state::layout::LEGACY_STATE_DIR);
        std::fs::create_dir_all(legacy.join("state")).unwrap();
        std::fs::write(legacy.join("state").join("marker"), "x").unwrap();
        std::fs::write(
            legacy.join("config.toml"),
            "[core]\nstrict = false\n[totally_unknown_section]\nx = 1\n",
        )
        .unwrap();

        // Surface 3: gitignore only legacy.
        std::fs::write(
            root.join(".gitignore"),
            format!("target/\n{}/\n", crate::state::layout::LEGACY_STATE_DIR),
        )
        .unwrap();

        // Surface 2: legacy hooks.
        std::fs::create_dir_all(root.join(".git").join("hooks")).unwrap();
        let brand = crate::state::layout::LEGACY_STATE_DIR.trim_start_matches('.');
        std::fs::write(
            root.join(".git").join("hooks").join("pre-commit"),
            format!(
                "#!/bin/sh\n# {brand}-ledger-gate: auto-installed by `{brand} init`\nif command -v {brand} &>/dev/null; then\n    if ! {brand} ledger status --compact --exit-code 2>/dev/null; then\n        exit 1\n    fi\nfi\n"
            ),
        )
        .unwrap();

        let layout = Layout::new(root);

        // Documented sequence step 1: repo-scoped command → migrate + gitignore
        // side-effect on successful rename (emulate load_startup_config).
        let renamed = layout.migrate_legacy_state_dir().unwrap();
        assert!(renamed);
        crate::git::ignore::add_to_gitignore(root, ".ledgerful/").unwrap();

        // Documented sequence step 2: update --repair-hooks.
        let report = crate::commands::hook_repair::repair_hooks_at(root, false).unwrap();
        assert!(
            report.residual_invocations.is_empty(),
            "hooks must be fully repaired: {report:?}"
        );

        // After steps 1–2: auto-fixed surfaces must be clean.
        // Unknown config keys may still warn until the user edits config — that
        // is reported with remediation, not auto-rewritten (spec §4).
        let findings = collect_legacy_migration_findings(root, &layout);
        assert!(
            !findings.iter().any(|f| f.code == "legacy-hooks"),
            "hooks clean after repair: {findings:?}"
        );
        assert!(
            !findings.iter().any(|f| f.code == "legacy-gitignore"),
            "gitignore has .ledgerful/ after migrate: {findings:?}"
        );
        assert!(
            !findings.iter().any(|f| f.code == "legacy-state"),
            "legacy dir renamed away: {findings:?}"
        );
        // Config residual is allowed and must name explicit remediation
        // (review/init) — never silent auto-rewrite.
        if let Some(cfg_f) = findings.iter().find(|f| f.code == "legacy-config") {
            assert!(
                cfg_f.message.contains("init") || cfg_f.message.contains("Review"),
                "config finding must name remediation: {cfg_f:?}"
            );
        }
    }

    #[test]
    fn split_brain_warns_when_local_and_shared_db_differ() {
        let tmp = tempfile::tempdir().unwrap();
        let work = Utf8PathBuf::from_path_buf(tmp.path().join("linked")).unwrap();
        let main_state =
            Utf8PathBuf::from_path_buf(tmp.path().join("main").join(".ledgerful")).unwrap();
        std::fs::create_dir_all(work.join(".ledgerful").join("state").as_std_path()).unwrap();
        std::fs::create_dir_all(main_state.join("state").as_std_path()).unwrap();
        std::fs::write(
            work.join(".ledgerful")
                .join("state")
                .join("ledger.db")
                .as_std_path(),
            b"local",
        )
        .unwrap();
        std::fs::write(
            main_state.join("state").join("ledger.db").as_std_path(),
            b"shared",
        )
        .unwrap();

        let layout = Layout::from_roots(&work, &main_state);
        let warn = split_brain_ledger_warning(&layout);
        assert!(warn.is_some(), "must warn when local != shared");
        assert!(
            warn.unwrap().contains("worktree-split-brain"),
            "expected split-brain tag"
        );
    }

    #[test]
    fn split_brain_silent_when_paths_are_same_file() {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        let layout = Layout::new(&root);
        layout.ensure_state_dir().unwrap();
        std::fs::write(layout.state_subdir().join("ledger.db").as_std_path(), b"db").unwrap();
        assert!(
            split_brain_ledger_warning(&layout).is_none(),
            "single-tree layout must not warn"
        );
    }
}
