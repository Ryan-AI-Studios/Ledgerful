//! `ledgerful verify` execution path (0251 extract from `verify/mod.rs`).

use crate::output::human::print_verify_plan;
use crate::output::verification::{
    VerificationReporter, dry_run_scope_line, format_assurance_line, print_dry_run_human,
    should_print_suggested_actions,
};
use crate::state::storage::StorageManager;
use crate::verify::assurance::{AssuranceInputs, build_execution_assurance};
use crate::verify::engine::{VerificationContext, VerifyEngine};
use crate::verify::plan::{VerificationStep, VerifyScope, build_plan_from_config};
use crate::verify::predictor::OutcomePredictor;
use crate::verify::results::VerificationReport;
use crate::verify::suggestions::{generate_suggestions, query_ledger_status};
use crate::verify::timeouts::{cap_auto_step_timeout, manual_timeout, manual_timeout_from_cli};
use miette::Result;
use owo_colors::{OwoColorize, Stream, Style};
use std::env;
use tracing::{debug, warn};

use super::diagnostic::{
    VerifyDryRunJson, VerifyDryRunStepJson, diagnostic_schema_version,
    refuse_mixed_verify_diagnostics, verify_dry_run_kind,
};
use super::dto::VerifyCliJson;
use super::health::{emit_verify_health_json, execute_verify_health};
use super::mapping::{TestMappingState, explain_test_mappings, step_relevant_to_entity};

/// Named options for [`execute_verify`] (0191). Signature flags stay in dispatch.
#[derive(Debug, Clone)]
pub struct ExecuteVerifyOpts {
    pub command: Option<String>,
    pub tx_id: Option<String>,
    pub timeout_secs: Option<u64>,
    pub no_predict: bool,
    pub explain: bool,
    pub entity: Option<String>,
    pub health: bool,
    pub dry_run: bool,
    pub scope: VerifyScope,
    pub auto_index: bool,
    pub allow_full_fallback: bool,
    pub json: bool,
    pub verbose: bool,
}

pub fn execute_verify(opts: ExecuteVerifyOpts) -> Result<()> {
    let ExecuteVerifyOpts {
        command: command_str,
        tx_id,
        timeout_secs,
        no_predict,
        explain,
        entity,
        health,
        dry_run,
        scope,
        auto_index,
        allow_full_fallback,
        json,
        verbose,
    } = opts;
    let current_dir = env::current_dir()
        .map_err(|e| miette::miette!("Failed to get current directory: {}", e))?;
    let layout = crate::commands::helpers::get_layout_or_cwd_if_not_git()?;
    let manual_requested = command_str.is_some();

    // 1. Initialize Context
    let config = crate::config::load::load_config(&layout).unwrap_or_else(|e| {
        warn!("Config load failed: {e}. Using defaults.");
        crate::config::model::Config::default()
    });

    // Deferred `tx_id` resolution until after short-circuits.

    let mut ctx = VerificationContext::new(
        layout.clone(),
        current_dir.clone(),
        config.clone(),
        no_predict,
        explain,
        health,
    );
    // Keep per-step SUCCESS/FAILURE println! off stdout when emitting JSON.
    // Quiet success is orthogonal: never set suppress from `!verbose`.
    ctx.suppress_human_output = json;
    ctx.verbose = verbose;

    // 2. Load Storage and Packet
    ctx.storage = match StorageManager::open_read_only(&layout) {
        Ok(storage) => Some(storage),
        Err(err) => {
            if !no_predict {
                let warning =
                    format!("Prediction disabled: failed to initialize SQLite storage: {err}");
                warn!("{warning}");
                ctx.add_warning(warning);
            }
            None
        }
    };

    if let Some(storage) = &ctx.storage {
        ctx.packet = match storage.get_latest_packet() {
            Ok(packet) => packet,
            Err(err) => {
                if !no_predict {
                    let warning =
                        format!("Prediction disabled: failed to load latest packet: {err}");
                    warn!("{warning}");
                    ctx.add_warning(warning);
                }
                None
            }
        };
    }

    // CG-F35 review fix: there are actually three plan-building paths, not
    // two. Besides the manual-command path (`command_str` is `Some`), a
    // config-defined plan (`[[verify.steps]]` present) takes priority over
    // `OutcomePredictor::predict` below and, like the manual path, never
    // consults `ctx.packet` at all -- `build_plan_from_config` just maps the
    // configured steps verbatim. Compute it once, here, so we can both gate
    // the staleness warning on whether prediction will actually run *and*
    // reuse this same value in the plan-building match below instead of
    // calling `build_plan_from_config` a second time.
    let config_plan = build_plan_from_config(&config.verify);

    // CG-F35 (requirement #1, #6): the packet just loaded above feeds
    // `OutcomePredictor` and the plan-reordering heuristics below. If it's
    // stale or corrupt relative to the current HEAD/working tree, those
    // predictions are quietly built on outdated data. Reuse the same
    // `ctx.add_warning` path the storage-init failure above already uses so
    // this surfaces through `VerificationReporter::report`'s warnings
    // section rather than being silent.
    //
    // Gated on `command_str.is_none() && config_plan.is_none()`: those are
    // exactly the conditions under which the plan-building match below falls
    // through to `OutcomePredictor::predict`. Both the manual-command branch
    // (`command_str` is `Some`) and the config-defined-plan branch
    // (`config_plan` is `Some`) build their plan without consulting
    // `ctx.packet` at all, so warning about stale predictions in either of
    // those paths would be inaccurate, since no prediction happens there.
    if command_str.is_none()
        && config_plan.is_none()
        && ctx.packet.is_some()
        && let Some(reason) = crate::state::reports::warn_if_impact_stale(&layout, &config)
    {
        ctx.add_warning(format!(
            "Verification predictions are based on data where the {reason} — plan ordering may not reflect the current working tree."
        ));
    }

    // Health mode early exit — skip OutcomePredictor::predict and full plan building
    if health {
        refuse_mixed_verify_diagnostics(true, dry_run, false, false, false)?;
        if json {
            return emit_verify_health_json(&layout, &config);
        }
        return execute_verify_health(&layout, &config);
    }

    // Bayesian apply hit count for honesty log + VerifyCliJson.matchedSteps (0140).
    // None = ordering not attempted; Some(n) = extract_dataset succeeded.
    let mut bayesian_matched_steps: Option<usize> = None;
    // Probability map size when extract_dataset succeeds (0144 dry-run stdout).
    let mut bayesian_dataset_keys: Option<usize> = None;

    // One material snapshot feeds prediction, overlay, and JSON. Nested Rust
    // discovery is only needed here if strict and legacy materiality differ.
    let detect_rust = scope.is_fast() && !manual_requested && config_plan.is_none();
    let (working_material, rust_crates) =
        classify_working_tree_material(layout.root.as_std_path(), detect_rust);

    // 3. Build Plan
    let (plan, steps) = match command_str {
        Some(ref cmd) => (
            None,
            vec![manual_step(
                cmd.clone(),
                manual_timeout(manual_timeout_from_cli(timeout_secs)),
            )],
        ),
        None => {
            if let Some(config_plan) = config_plan {
                // Plan banner only under --verbose live path (0121 quiet success).
                // Never on --dry-run: descriptions are pipe-merged walls (0144).
                if verbose && !json && !dry_run {
                    print_verify_plan(&config_plan);
                }
                (Some(config_plan.clone()), config_plan.steps)
            } else {
                // 0288: fully-clean auto-policy dry-run skips OutcomePredictor
                // (stale Temporal dump / optional-embed 10060). Do not set
                // ctx.no_predict — that also gates storage/packet warnings.
                // Manual --command and [[verify.steps]] never reach here.
                let prediction = if dry_run && !working_tree_has_material_changes(working_material)
                {
                    crate::verify::predict::PredictionResult::default()
                } else {
                    OutcomePredictor::predict(&mut ctx)?
                };
                let rules = crate::policy::load::load_rules(&layout)?;

                let mut plan = match &ctx.packet {
                    Some(packet) => {
                        let profile = crate::platform::repository::detect_repository(
                            layout.root.as_std_path(),
                        );
                        // 0145 B1: live-clean working tree → EmptyChanges even when
                        // a saved impact packet still lists changes (phantom packet).
                        // Kept here (not in plan.rs) so Layout::new(".") unit tests
                        // stay hermetic without a live git short-circuit.
                        if scope.is_fast() && !working_tree_has_material_changes(working_material) {
                            crate::verify::plan::build_empty_changes_plan(&profile)
                        } else if scope.is_fast() {
                            // 0203-B: classify from live git paths (replace
                            // snapshot packet.changes). Overlay is not persisted.
                            let conn = ctx.storage.as_ref().map(|s| s.get_connection());
                            build_fast_scoped_from_overlay_or_fail_closed(
                                Some(packet),
                                &layout,
                                &rules,
                                &prediction.files,
                                &config.verify,
                                &profile,
                                conn,
                                auto_index,
                                allow_full_fallback,
                                rust_crates,
                            )
                        } else {
                            let conn = ctx.storage.as_ref().map(|s| s.get_connection());
                            crate::verify::plan::build_plan_scoped_with_options(
                                packet,
                                &rules,
                                &prediction.files,
                                &config.verify,
                                &profile,
                                scope,
                                conn,
                                &layout,
                                auto_index,
                                allow_full_fallback,
                            )
                        }
                    }
                    None => {
                        // No saved impact packet (0135 final codex P1 / 0203 P2):
                        // - Full scope: keep build_plan (historical).
                        // - Fast + clean working tree: EmptyChanges cheap path.
                        // - Fast + dirty: live overlay + classifier (docs-only
                        //   cheap; SharedInfra / unmapped src still refuse).
                        let profile = crate::platform::repository::detect_repository(
                            layout.root.as_std_path(),
                        );
                        let empty_packet = crate::impact::packet::ImpactPacket::default();
                        if scope.is_fast() {
                            if working_tree_has_material_changes(working_material) {
                                let conn = ctx.storage.as_ref().map(|s| s.get_connection());
                                build_fast_scoped_from_overlay_or_fail_closed(
                                    None,
                                    &layout,
                                    &rules,
                                    &prediction.files,
                                    &config.verify,
                                    &profile,
                                    conn,
                                    auto_index,
                                    allow_full_fallback,
                                    rust_crates,
                                )
                            } else {
                                let conn = ctx.storage.as_ref().map(|s| s.get_connection());
                                crate::verify::plan::build_plan_scoped_with_options(
                                    &empty_packet,
                                    &rules,
                                    &prediction.files,
                                    &config.verify,
                                    &profile,
                                    scope,
                                    conn,
                                    &layout,
                                    auto_index,
                                    allow_full_fallback,
                                )
                            }
                        } else {
                            crate::verify::plan::build_plan(
                                &empty_packet,
                                &rules,
                                &[],
                                &config.verify,
                                &profile,
                                layout.root.as_std_path(),
                            )
                        }
                    }
                };

                // Apply probabilistic ordering if storage is available
                if let Some(stg) = &ctx.storage
                    && let Ok(dataset) =
                        crate::verify::probability::extract_dataset(stg.get_connection())
                {
                    let probs = crate::verify::probability::calculate_probabilities(&dataset);
                    let matched = plan.apply_probability_ordering(&probs);
                    bayesian_matched_steps = Some(matched);
                    bayesian_dataset_keys = Some(probs.len());
                    // 0144: product surface is dry-run stdout `matched_steps=`;
                    // demote tracing so default RUST_LOG=info is not a duplicate.
                    if matched > 0 {
                        debug!(
                            "Probabilistic verification ordering applied (matched_steps={matched}, dataset_keys={})",
                            probs.len()
                        );
                    } else {
                        // Honesty: never claim "applied N models" with dataset
                        // size when zero plan steps hit the probability map.
                        debug!(
                            "Probabilistic verification ordering skipped reorder (matched_steps=0, dataset_keys={})",
                            probs.len()
                        );
                    }
                }

                // Announce fast→full fallback or MappingRefuse before the user
                // waits. On --json the reason is in fallbackReason — do not
                // print around the payload. On --dry-run, defer so `scope:` is
                // the first product line (0203-C / P5).
                if !json
                    && !dry_run
                    && let Some(reason) = &plan.fallback_reason
                {
                    print_fallback_announcement(reason, plan.refused);
                }

                // Plan banner only under --verbose live path (0121 quiet success).
                // Never on --dry-run: descriptions are pipe-merged walls (0144).
                if verbose && !json && !dry_run {
                    print_verify_plan(&plan);
                }
                for step in &mut plan.steps {
                    step.timeout_secs = cap_auto_step_timeout(step.timeout_secs, timeout_secs);
                }
                let steps = plan.steps.clone();
                (Some(plan), steps)
            }
        }
    };

    // Entity-scoped explanation: show tests mapped to the entity and relevant steps.
    // Skipped under --json (machine payload has steps; explain is human-only).
    if !json && explain && entity.is_some() {
        let target = entity.as_deref().unwrap_or("");
        println!(
            "\n{}",
            format!("Verification explanation for entity: {}", target)
                .if_supports_color(Stream::Stdout, |s| s.style(Style::new().bold().cyan()))
        );

        // M3: prefer resolved stored path for step relevance when alias/suffix resolved.
        let mut resolved_for_filter: Option<String> = None;

        if let Some(storage) = &ctx.storage {
            let conn = storage.get_connection();
            let normalized_entity =
                crate::util::path::normalize_relative_path(layout.root.as_std_path(), target)
                    .unwrap_or_else(|_| target.to_string());

            let mapping_state = explain_test_mappings(conn, &normalized_entity);
            resolved_for_filter = mapping_state.resolved_path().map(|p| p.to_string());

            match mapping_state {
                TestMappingState::TableMissing => {
                    println!(
                        "  Test-mapping table is not present in the index. Run `ledgerful index --incremental` to build it."
                    );
                }
                TestMappingState::TableEmpty => {
                    println!(
                        "  No test mappings have been indexed yet. Run `ledgerful index --incremental` to populate them."
                    );
                }
                TestMappingState::EntityNotIndexed => {
                    println!(
                        "  '{}' is not a recognized indexed file path or symbol name.",
                        target
                    );
                    println!(
                        "  Run `ledgerful index --incremental` if it was added or renamed recently, or confirm the path with `ledgerful search \"{}\"`.",
                        target
                    );
                }
                TestMappingState::EntityAmbiguous { query, candidates } => {
                    let total = candidates.len();
                    println!("  {} indexed paths match '{}':", total, query);
                    let show = total.min(10);
                    for p in candidates.iter().take(show) {
                        println!("    • {}", p);
                    }
                    if total > 10 {
                        println!("    … and {} more", total - 10);
                    }
                    println!("  Provide a more specific path.");
                }
                TestMappingState::NoMappingsForEntity { resolved_path } => {
                    let display = resolved_path
                        .as_deref()
                        .unwrap_or(normalized_entity.as_str());
                    println!(
                        "  '{}' is indexed, but no tests currently map to it.",
                        display
                    );
                    println!(
                        "  This may be accurate (no covering tests yet) -- use `ledgerful search \"{}\"` to confirm test coverage manually.",
                        display
                    );
                }
                TestMappingState::Mapped {
                    tests,
                    resolved_path,
                } => {
                    let display = resolved_path
                        .as_deref()
                        .unwrap_or(normalized_entity.as_str());
                    println!("  Mapped tests for '{}' ({}):", display, tests.len());
                    for t in &tests {
                        println!("    • {}", t.display_line());
                    }
                }
            }
        }

        let relevant: Vec<_> = steps
            .iter()
            .filter(|s| step_relevant_to_entity(&s.command, target, resolved_for_filter.as_deref()))
            .collect();
        println!(
            "\n  Verification steps relevant to this entity ({}):",
            relevant.len()
        );
        for s in &relevant {
            println!("    • {} (timeout: {}s)", s.command, s.timeout_secs);
        }
        println!();
    }

    // MappingRefuse early path: never execute cargo; force fail (vacuous-pass guard).
    // Dry-run and live share this: refuse → exit 1 / Err.
    let plan_refused = plan.as_ref().is_some_and(|p| p.refused);
    if plan_refused {
        if dry_run {
            let plan_source = plan.as_ref().and_then(|p| p.source.clone());
            let assurance =
                execution_assurance_for(&steps, &[], plan_source, manual_requested, true, true);
            if json {
                let reason = plan
                    .as_ref()
                    .and_then(|p| p.fallback_reason.clone())
                    .unwrap_or_else(|| "fast scope unavailable; refusing full suite".to_string());
                let prediction_skipped = command_str.is_none()
                    && matches!(
                        working_material,
                        WorkingTreeMaterial::Clean | WorkingTreeMaterial::Unavailable
                    );
                emit_verify_dry_run_json(
                    &[],
                    scope,
                    Some(reason.clone()),
                    true,
                    bayesian_matched_steps,
                    bayesian_dataset_keys,
                    working_material,
                    prediction_skipped,
                    &assurance,
                )?;
                return Err(miette::miette!("{reason}"));
            }
            // P5: scope line first, then ℹ/Next, assurance, then the refused footer.
            println!("{}", dry_run_scope_line(scope));
            let reason = plan
                .as_ref()
                .and_then(|p| p.fallback_reason.clone())
                .unwrap_or_else(|| "fast scope unavailable; refusing full suite".to_string());
            print_fallback_announcement(&reason, true);
            println!("{}", format_assurance_line(&assurance));
            println!(
                "\n{}",
                "Dry run mode: plan refused — no commands would be executed."
                    .if_supports_color(Stream::Stdout, |s| s.yellow())
            );
            return Err(miette::miette!("{reason}"));
        }

        // Live refuse: emit report/JSON with ok:false, empty steps; no cargo.
        // `VerificationReport::new` moves `plan`. Source is copied first; `steps`
        // is the surviving clone. Production MappingRefuse steps are empty.
        let plan_source = plan.as_ref().and_then(|p| p.source.clone());
        let mut report = VerificationReport::new(plan, Vec::new());
        report.overall_pass = false;
        let assurance = execution_assurance_for(
            &steps,
            &report.results,
            plan_source,
            manual_requested,
            false,
            true,
        );
        if json {
            let payload =
                VerifyCliJson::from_report(&report, scope, bayesian_matched_steps, assurance);
            println!("{}", payload.to_json_string()?);
        } else {
            println!("{}", format_assurance_line(&assurance));
        }
        let reason = report
            .plan
            .as_ref()
            .and_then(|p| p.fallback_reason.clone())
            .unwrap_or_else(|| "fast scope unavailable; refusing full suite".to_string());
        return Err(miette::miette!("{reason}"));
    }

    // Dry Run early exit — plan-first scannable layout (0144).
    // No print_verify_plan (gated above); no cargo execution.
    if dry_run {
        let plan_source = plan.as_ref().and_then(|p| p.source.clone());
        let assurance =
            execution_assurance_for(&steps, &[], plan_source, manual_requested, true, false);
        if json {
            let prediction_skipped = command_str.is_none()
                && matches!(
                    working_material,
                    WorkingTreeMaterial::Clean | WorkingTreeMaterial::Unavailable
                );
            return emit_verify_dry_run_json(
                &steps,
                scope,
                plan.as_ref().and_then(|p| p.fallback_reason.clone()),
                false,
                bayesian_matched_steps,
                bayesian_dataset_keys,
                working_material,
                prediction_skipped,
                &assurance,
            );
        }
        // Manual --command: keep simple Verification Plan + single step + footer.
        // P9: same `scope:` first product line as plan dry-run.
        if manual_requested {
            println!("{}", dry_run_scope_line(scope));
            println!(
                "{}",
                "Verification Plan"
                    .if_supports_color(Stream::Stdout, |s| s.style(Style::new().bold().green()))
            );
            println!(
                "  • {} (timeout: {}s)",
                command_str.as_deref().unwrap_or(""),
                manual_timeout(manual_timeout_from_cli(timeout_secs))
            );
            println!();
            println!("{}", format_assurance_line(&assurance));
            println!(
                "{}",
                "Dry run mode: verification plan displayed above. No commands were executed."
                    .if_supports_color(Stream::Stdout, |s| s.yellow())
            );
            return Ok(());
        }

        // CLI --verbose expands path lists; VERBOSE_DRY_RUN remains additive alias.
        let dry_verbose = verbose || std::env::var("VERBOSE_DRY_RUN").is_ok();
        let formatted = crate::output::verification::format_dry_run_human(
            &steps,
            bayesian_matched_steps,
            bayesian_dataset_keys,
            dry_verbose,
            scope,
            &assurance,
        );
        // SharedInfra dry-run: keep ℹ after `scope:` (first line), before the
        // rest of the 0144 layout.
        if let Some(reason) = plan.as_ref().and_then(|p| p.fallback_reason.as_deref()) {
            match formatted.split_once('\n') {
                Some((first, rest)) => {
                    println!("{first}");
                    print_fallback_announcement(reason, false);
                    print!("{rest}");
                }
                None => {
                    println!("{formatted}");
                    print_fallback_announcement(reason, false);
                }
            }
        } else {
            print_dry_run_human(
                &steps,
                bayesian_matched_steps,
                bayesian_dataset_keys,
                dry_verbose,
                scope,
                &assurance,
            );
        }
        return Ok(());
    }

    // 4. Execute
    // Explicitly release the database connection and close locks before running verification commands.
    // This prevents deadlock/lock contention when cargo test runs child Ledgerful commands.
    if let Some(storage) = ctx.storage.take() {
        let _ = storage.shutdown();
    }

    // Show progress indicator before verification execution.
    // DoD-1 quiet success: demote to debug! when !verbose so the default INFO
    // filter does not print progress noise. --verbose restores info!. Skip
    // entirely for --json (machine mode).
    if !json && !ctx.no_predict {
        let num_steps = steps.len();
        if num_steps > 0 {
            if crate::output::verification::should_emit_verify_progress_info(verbose, json) {
                tracing::info!(
                    target: "cli_summary",
                    "Running {} verification step(s)...",
                    num_steps
                );
            } else {
                tracing::debug!(
                    target: "cli_summary",
                    "Running {} verification step(s)...",
                    num_steps
                );
            }
        }
    }

    let resolved_tx_id = if let Some(ref id) = tx_id {
        match StorageManager::init_with_layout(&layout) {
            Ok(mut stg) => {
                let mgr = crate::ledger::TransactionManager::new(
                    &mut stg,
                    layout.root.clone().into(),
                    config.clone(),
                );
                let resolved = mgr
                    .resolve_tx_id(id)
                    .map_err(|e| miette::miette!("Failed to resolve tx-id '{}': {}", id, e))?;
                match mgr.get_transaction(&resolved) {
                    Ok(Some(tx)) => {
                        if tx.status != "PENDING" {
                            return Err(miette::miette!(
                                "Cannot attach to transaction '{}': status is '{}' (must be PENDING)",
                                resolved,
                                tx.status
                            ));
                        }
                    }
                    Ok(None) => {
                        return Err(miette::miette!("Transaction '{}' not found", resolved));
                    }
                    Err(e) => {
                        return Err(miette::miette!(
                            "Failed to read transaction '{}' from database: {}",
                            resolved,
                            e
                        ));
                    }
                }
                Some(resolved)
            }
            Err(_) => {
                return Err(miette::miette!(
                    "Failed to initialize storage for tx-id resolution"
                ));
            }
        }
    } else {
        let sidecar_path = layout.state_subdir().join("pending_hook_tx");
        let mut auto_id = None;
        if sidecar_path.exists() {
            match std::fs::read_to_string(&sidecar_path) {
                Ok(content) => match serde_json::from_str::<
                    crate::commands::hook_post_commit::PendingHookTx,
                >(&content)
                {
                    Ok(pending) => {
                        let repo_root = layout.root.as_std_path();
                        let mut fresh = false;

                        let editmsg_path = repo_root.join(".git").join("COMMIT_EDITMSG");
                        let index_lock_path = repo_root.join(".git").join("index.lock");

                        if editmsg_path.exists()
                            && index_lock_path.exists()
                            && let Ok(edit_msg) = std::fs::read_to_string(&editmsg_path)
                        {
                            let cleaned = crate::util::text::clean_commit_msg(&edit_msg);
                            use sha2::{Digest, Sha256};
                            let mut hasher = Sha256::new();
                            hasher.update(cleaned.as_bytes());
                            let edit_hash = hex::encode(hasher.finalize());
                            if edit_hash == pending.commit_msg_hash {
                                fresh = true;
                            }
                        }

                        if fresh {
                            match StorageManager::init_with_layout(&layout) {
                                Ok(mut stg) => {
                                    let mgr = crate::ledger::TransactionManager::new(
                                        &mut stg,
                                        layout.root.clone().into(),
                                        config.clone(),
                                    );
                                    match mgr.resolve_tx_id(&pending.tx_id) {
                                        Ok(resolved) => match mgr.get_transaction(&resolved) {
                                            Ok(Some(tx)) => {
                                                if tx.status == "PENDING" {
                                                    auto_id = Some(resolved);
                                                } else {
                                                    warn!(
                                                        "Sidecar transaction {} is in state '{}', not PENDING; skipping auto-bind.",
                                                        resolved, tx.status
                                                    );
                                                }
                                            }
                                            Ok(None) => warn!(
                                                "Sidecar transaction {} not found in DB; skipping auto-bind.",
                                                resolved
                                            ),
                                            Err(e) => warn!(
                                                "Failed to read sidecar transaction {} from DB: {}; skipping auto-bind.",
                                                resolved, e
                                            ),
                                        },
                                        Err(e) => warn!(
                                            "Sidecar transaction {} could not be resolved: {}; skipping auto-bind.",
                                            pending.tx_id, e
                                        ),
                                    }
                                }
                                Err(e) => warn!(
                                    "Failed to initialize storage for auto-bind: {}; skipping auto-bind.",
                                    e
                                ),
                            }
                        } else {
                            warn!(
                                "Sidecar transaction {} is stale (commit_msg_hash mismatch); skipping auto-bind.",
                                pending.tx_id
                            );
                        }
                    }
                    Err(e) => warn!(
                        "Failed to parse pending hook sidecar: {}; skipping auto-bind.",
                        e
                    ),
                },
                Err(e) => warn!(
                    "Failed to read pending hook sidecar: {}; skipping auto-bind.",
                    e
                ),
            }
        }
        auto_id
    };

    let mut report = VerifyEngine::execute_with_scope(
        &mut ctx,
        plan,
        &steps,
        manual_requested,
        resolved_tx_id,
        scope,
    )?;

    // 5. Generate Suggestions
    let ledger_status = query_ledger_status(&layout);
    let suggestions = generate_suggestions(&report, &ledger_status);

    report = report.with_suggested_actions(suggestions);

    // 6. Final Reporting & IPC
    // Ordering on fail (non-json): step FAILURE lines (during run) → structured
    // fail block → Suggested Actions → miette on stderr.
    // Quiet success: suppress Suggested Actions; one trailing ok line.
    let assurance = execution_assurance_for(
        &steps,
        &report.results,
        report.plan.as_ref().and_then(|p| p.source.clone()),
        manual_requested,
        false,
        report.plan.as_ref().is_some_and(|p| p.refused),
    );
    if !json {
        if !report.overall_pass
            && let Some(block) = crate::verify::fail_block::format_fail_block_from_report(&report)
        {
            println!("{block}");
        }

        if should_print_suggested_actions(verbose, report.overall_pass) {
            VerificationReporter::report(&ctx, &report);
            println!("{}", format_assurance_line(&assurance));
        } else {
            // Quiet green: still surface prediction warnings on stderr; no
            // Suggested Actions header on stdout. Assurance sits immediately
            // before the trailing success line.
            if !report.prediction_warnings.is_empty() {
                VerificationReporter::print_prediction_warnings(&report.prediction_warnings);
            }
            println!("{}", format_assurance_line(&assurance));
            println!("Verification passed");
        }
    }

    // Push results to bridge
    let bridge_outcomes = report
        .results
        .iter()
        .map(|res| crate::bridge::model::BridgeVerifyOutcome {
            success: res.exit_code == 0,
            command: res.command.clone(),
            error_snippet: if res.exit_code != 0 {
                let err = if !res.stderr_summary.is_empty() {
                    &res.stderr_summary
                } else {
                    &res.stdout_summary
                };
                Some(err.chars().take(200).collect::<String>())
            } else {
                None
            },
        })
        .collect();
    crate::bridge::notify::push_verify_results(bridge_outcomes);

    // Emit versioned CLI payload before any error return (DoD-15 boundary):
    // JSON present + non-zero = validation rejection; no JSON + non-zero = fatal.
    if json {
        let payload = VerifyCliJson::from_report(&report, scope, bayesian_matched_steps, assurance);
        println!("{}", payload.to_json_string()?);
    }

    if report.overall_pass {
        Ok(())
    } else {
        Err(miette::miette!("Verification failed"))
    }
}

fn print_fallback_announcement(reason: &str, refused: bool) {
    println!(
        "{} {}",
        "ℹ".if_supports_color(Stream::Stdout, |s| s.cyan()),
        reason.if_supports_color(Stream::Stdout, |s| s.yellow())
    );
    if refused {
        println!(
            "{}",
            "Next: ledgerful index --incremental\n      ledgerful verify --scope fast --auto-index\n      ledgerful verify --scope full\n      ledgerful verify --scope fast --allow-full-fallback"
                .if_supports_color(Stream::Stdout, |s| s.yellow())
        );
    }
}

/// Overlay live ignore-filtered git paths onto a classifier packet (`--scope fast`
/// + dirty).
///
/// Replaces `packet.changes` (does not union). Sets `head_hash` from
/// live HEAD. Does not persist the overlay or write `latest-impact.json`.
#[cfg(test)]
fn overlay_fast_classifier_packet(
    layout: &crate::state::layout::Layout,
    packet: crate::impact::packet::ImpactPacket,
) -> miette::Result<crate::impact::packet::ImpactPacket> {
    let (_, rust) = classify_working_tree_material(layout.root.as_std_path(), true);
    overlay_fast_classifier_packet_with_rust(
        layout,
        packet,
        rust.is_some_and(crate::verify::plan::RustCrates::is_strict),
    )
}

fn overlay_fast_classifier_packet_with_rust(
    layout: &crate::state::layout::Layout,
    mut packet: crate::impact::packet::ImpactPacket,
    rust: bool,
) -> miette::Result<crate::impact::packet::ImpactPacket> {
    let live = crate::git::status::collect_changed_files_for_filter(layout)?;
    let mut changes: Vec<crate::impact::packet::ChangedFile> = live
        .into_iter()
        .filter(|c| is_material_or_cheap_verify_path(&c.path, rust))
        .map(file_change_to_changed_file)
        .collect();
    changes.sort_by(|a, b| a.path.cmp(&b.path));
    packet.changes = changes;

    if let Ok(repo) = crate::git::repo::open_repo(layout.root.as_std_path())
        && let Ok((hash, _)) = crate::git::repo::get_head_info(&repo)
    {
        packet.head_hash = hash;
    }
    Ok(packet)
}

fn file_change_to_changed_file(
    change: crate::git::FileChange,
) -> crate::impact::packet::ChangedFile {
    let (status, old_path) = match change.change_type {
        crate::git::ChangeType::Added => ("Added".to_string(), None),
        crate::git::ChangeType::Modified => ("Modified".to_string(), None),
        crate::git::ChangeType::Deleted => ("Deleted".to_string(), None),
        crate::git::ChangeType::Renamed { old_path } => ("Renamed".to_string(), Some(old_path)),
    };
    crate::impact::packet::ChangedFile {
        path: change.path,
        status,
        old_path,
        is_staged: change.is_staged,
        ..Default::default()
    }
}

fn is_material_or_cheap_verify_path(path: &std::path::Path, rust: bool) -> bool {
    is_material_for_profile(path, rust) || crate::verify::plan::is_non_code_cheap_path(path)
}

/// 0549 D8 applies where the embedded lookup runs (Rust profile); other
/// profiles keep the legacy allowlist.
fn is_material_for_profile(path: &std::path::Path, rust: bool) -> bool {
    if rust {
        is_material_verify_path(path)
    } else {
        is_legacy_material_verify_path(path)
    }
}

/// Live overlay + classifier for `--scope fast` dirty trees. Shared by Some-packet
/// and None-packet (P2) arms. Overlay failure is fail-closed (snapshot or refuse).
#[allow(clippy::too_many_arguments)]
fn build_fast_scoped_from_overlay_or_fail_closed(
    snapshot: Option<&crate::impact::packet::ImpactPacket>,
    layout: &crate::state::layout::Layout,
    rules: &crate::policy::rules::Rules,
    predicted: &[crate::verify::predict::PredictedFile],
    verify_config: &crate::config::model::VerifyConfig,
    profile: &crate::platform::repository::RepositoryProfile,
    conn: Option<&rusqlite::Connection>,
    auto_index: bool,
    allow_full_fallback: bool,
    rust_crates: Option<crate::verify::plan::RustCrates>,
) -> crate::verify::plan::VerificationPlan {
    let base = snapshot.cloned().unwrap_or_default();
    let overlaid = match overlay_fast_classifier_packet_with_rust(
        layout,
        base,
        rust_crates.is_some_and(crate::verify::plan::RustCrates::is_strict),
    ) {
        Ok(packet) => packet,
        Err(err) => {
            warn!("fast-scope live overlay failed: {err}");
            // Unknown alone does not reject a successfully observed cheap-only
            // packet. When the live paths are unavailable, though, a stale
            // cheap snapshot cannot prove that strict material is untouched.
            // Keep status-lock / unreadable-Git failures closed before either
            // the saved-packet route or an authorized full fallback.
            if let Some(crate::verify::plan::RustCrates::Unknown(class)) = rust_crates {
                return crate::verify::plan::refuse_plan_for_trigger(&format!(
                    "Rust detection inconclusive: {class}"
                ));
            }
            match snapshot {
                Some(packet) => {
                    return crate::verify::plan::build_plan_scoped_with_rust_crates(
                        packet,
                        rules,
                        predicted,
                        verify_config,
                        profile,
                        crate::verify::plan::VerifyScope::Fast,
                        conn,
                        layout,
                        auto_index,
                        allow_full_fallback,
                        rust_crates,
                    );
                }
                None => {
                    if allow_full_fallback {
                        let empty = crate::impact::packet::ImpactPacket::default();
                        let mut plan = crate::verify::plan::build_plan(
                            &empty,
                            rules,
                            &[],
                            verify_config,
                            profile,
                            layout.root.as_std_path(),
                        );
                        plan.fallback_reason = Some(
                            "fast scope unavailable — no impact packet for dirty tree; run `ledgerful scan --impact`; running full (~5-8 min)"
                                .to_string(),
                        );
                        plan.refused = false;
                        return plan;
                    }
                    return crate::verify::plan::refuse_plan_for_trigger(
                        "no impact packet for dirty tree; run `ledgerful scan --impact`",
                    );
                }
            }
        }
    };
    crate::verify::plan::build_plan_scoped_with_rust_crates(
        &overlaid,
        rules,
        predicted,
        verify_config,
        profile,
        crate::verify::plan::VerifyScope::Fast,
        conn,
        layout,
        auto_index,
        allow_full_fallback,
        rust_crates,
    )
}

/// Working-tree material-change probe (0321). Git open/status failure is
/// [`Unavailable`], not a clean tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorkingTreeMaterial {
    Clean,
    Dirty,
    Unavailable,
}

fn classify_working_tree_material(
    repo_root: &std::path::Path,
    detect_nested: bool,
) -> (WorkingTreeMaterial, Option<crate::verify::plan::RustCrates>) {
    let Ok(repo) = crate::git::repo::open_repo(repo_root) else {
        return (WorkingTreeMaterial::Unavailable, None);
    };
    let Ok(changes) = crate::git::status::get_repo_status(&repo) else {
        return if detect_nested {
            (
                WorkingTreeMaterial::Dirty,
                Some(crate::verify::plan::RustCrates::Unknown("unreadable")),
            )
        } else {
            (WorkingTreeMaterial::Unavailable, None)
        };
    };
    classify_material_changes(
        repo_root,
        &changes,
        detect_nested,
        crate::verify::plan::repo_has_rust_crates,
    )
}

/// The only discovery trigger. A missing memo means NotNeeded, not a request
/// for a later planning stage to walk. The injected FnOnce also makes the
/// at-most-once rule observable without global counters or environment state.
///
/// A dirty path whose strict and legacy materiality differ always triggers
/// discovery: without it, a `.txt` template would read as a clean tree. When
/// there is no root `Cargo.toml`, a dirty **cheap-set** path (docs, README,
/// LICENSE, packaging) triggers it too: that is the only class that can
/// otherwise fall into the cheap plan and return a zero-step green for
/// referenced material (0549 DoD-2). A repo with a root manifest still never
/// walks on a docs-only edit, and a non-Rust repo is not walked for ordinary
/// source changes.
fn classify_material_changes(
    repo_root: &std::path::Path,
    changes: &[crate::git::FileChange],
    detect_rust: bool,
    detect: impl FnOnce(&std::path::Path) -> crate::verify::plan::RustCrates,
) -> (WorkingTreeMaterial, Option<crate::verify::plan::RustCrates>) {
    let cheap_without_root = !repo_root.join("Cargo.toml").is_file()
        && changes
            .iter()
            .any(|change| crate::verify::plan::is_non_code_cheap_path(&change.path));
    let rust = if detect_rust
        && (cheap_without_root
            || changes.iter().any(|change| {
                is_material_verify_path(&change.path)
                    != is_legacy_material_verify_path(&change.path)
            })) {
        Some(detect(repo_root))
    } else {
        None
    };
    if changes.iter().any(|change| {
        is_material_for_profile(
            &change.path,
            rust.is_some_and(crate::verify::plan::RustCrates::is_strict),
        )
    }) {
        (WorkingTreeMaterial::Dirty, rust)
    } else {
        (WorkingTreeMaterial::Clean, rust)
    }
}

/// True when the working tree has **material** changes that verify should not
/// ignore when there is no saved impact packet (0135 final codex P1).
///
/// Material = any changed path except an explicit exemption list (0549 D8,
/// overturning the 0321 extension allowlist so a dirty `.txt` template or
/// `.csv` fixture is not mistaken for a clean tree): `.ledgerful/**`,
/// PATH/test shims such as a root `cargo.bat`, and `.git/`, `target/`,
/// `node_modules/` trees.
///
/// On git discovery/status failure, returns false so clean EmptyChanges still
/// works in non-git fixtures (Unavailable ≠ Dirty).
fn working_tree_has_material_changes(material: WorkingTreeMaterial) -> bool {
    matches!(material, WorkingTreeMaterial::Dirty)
}

#[allow(clippy::too_many_arguments)]
fn execution_assurance_for(
    steps: &[VerificationStep],
    results: &[crate::verify::results::VerificationResult],
    plan_source: Option<crate::verify::plan::PlanSource>,
    manual_requested: bool,
    preview: bool,
    refused: bool,
) -> crate::verify::assurance::ExecutionAssuranceJson {
    build_execution_assurance(AssuranceInputs {
        planned_steps: steps,
        results,
        plan_source,
        manual_requested,
        preview,
        refused,
    })
}

#[allow(clippy::too_many_arguments)]
fn emit_verify_dry_run_json(
    steps: &[crate::verify::plan::VerificationStep],
    scope: crate::verify::plan::VerifyScope,
    fallback_reason: Option<String>,
    refused: bool,
    matched_steps: Option<usize>,
    dataset_keys: Option<usize>,
    material: WorkingTreeMaterial,
    prediction_gate: bool,
    assurance: &crate::verify::assurance::ExecutionAssuranceJson,
) -> Result<()> {
    let git_available = !matches!(material, WorkingTreeMaterial::Unavailable);
    let clean_tree = match material {
        WorkingTreeMaterial::Clean => Some(true),
        WorkingTreeMaterial::Dirty => Some(false),
        WorkingTreeMaterial::Unavailable => None,
    };
    // Caller already applied the 0288 gate (Clean or Unavailable skip predict).
    // Do not re-AND with Clean — that would report predictionSkipped:false after
    // an Unavailable skip. Reason is cleanTree only when the tree is actually clean.
    let prediction_skipped = prediction_gate;
    let prediction_skip_reason =
        if prediction_skipped && matches!(material, WorkingTreeMaterial::Clean) {
            Some("cleanTree".to_string())
        } else {
            None
        };
    let scope_executed = if refused {
        "refused".to_string()
    } else if fallback_reason.is_some() {
        "full".to_string()
    } else {
        scope.to_string()
    };
    let planned: Vec<VerifyDryRunStepJson> = if refused {
        Vec::new()
    } else {
        steps
            .iter()
            .map(|s| VerifyDryRunStepJson {
                name: crate::verify::fail_block::step_name_from_command(&s.command),
                command: s.command.clone(),
                status: "planned".to_string(),
                timeout_secs: s.timeout_secs,
                budget_source: s.budget_source.clone(),
            })
            .collect()
    };
    let payload = VerifyDryRunJson {
        schema_version: diagnostic_schema_version(),
        kind: verify_dry_run_kind(),
        scope_requested: scope.to_string(),
        scope_executed,
        fallback_reason,
        refused,
        executed: false,
        git_available,
        clean_tree,
        prediction_skipped,
        prediction_skip_reason,
        matched_steps,
        dataset_keys,
        steps: planned,
        execution_assurance: Some(assurance.clone()),
    };
    println!("{}", payload.to_json_string()?);
    Ok(())
}

fn is_material_verify_path(path: &std::path::Path) -> bool {
    let norm = path.to_string_lossy().replace('\\', "/");
    let norm = norm.trim_start_matches("./");
    // PATH/test shim used by empty-repo verify tests — not product source.
    if norm.eq_ignore_ascii_case("cargo.bat")
        || norm.eq_ignore_ascii_case("cargo.cmd")
        || norm.eq_ignore_ascii_case("cargo")
    {
        return false;
    }
    // 0549 D8: VCS and build-output trees are never material.
    const EXEMPT_DIRS: &[&str] = &[".ledgerful", ".git", "target", "node_modules"];
    if norm.split('/').next().is_some_and(|first| {
        EXEMPT_DIRS
            .iter()
            .any(|exempt| crate::verify::plan::path_component_matches(first, exempt))
    }) {
        return false;
    }
    true
}

/// Pre-0549 extension allowlist, kept for repositories where the embedded
/// lookup does not run (no Rust profile; non-Rust embedding is 0570).
fn is_legacy_material_verify_path(path: &std::path::Path) -> bool {
    crate::verify::plan::is_legacy_material_path(path)
}

fn manual_step(command: String, timeout_secs: u64) -> VerificationStep {
    VerificationStep {
        description: "Manually requested verification command".to_string(),
        command,
        timeout_secs,
        shell: true,
        budget_source: Some("manual".to_string()),
    }
}

#[cfg(test)]
mod material_verify_path_tests {
    use super::is_material_verify_path;
    use std::path::Path;

    #[test]
    fn cpp_source_and_header_are_material() {
        assert!(is_material_verify_path(Path::new("src/widget.cpp")));
        assert!(is_material_verify_path(Path::new("include/widget.hpp")));
        assert!(is_material_verify_path(Path::new("src/main.c")));
        assert!(is_material_verify_path(Path::new("src/lib.h")));
        assert!(is_material_verify_path(Path::new("src/util.cc")));
        assert!(is_material_verify_path(Path::new("src/util.cxx")));
        assert!(is_material_verify_path(Path::new("include/types.hh")));
        assert!(is_material_verify_path(Path::new("include/types.hxx")));
        assert!(is_material_verify_path(Path::new("include/types.h++")));
    }

    #[test]
    fn ledgerful_and_non_source_not_material() {
        assert!(!is_material_verify_path(Path::new(
            ".ledgerful/state/ledger.cozo"
        )));
        assert!(!is_material_verify_path(Path::new(".ledgerful/state/x")));
        assert!(!is_material_verify_path(Path::new("cargo.bat")));
        assert!(!is_material_verify_path(Path::new("target/debug/x.rs")));
        assert!(!is_material_verify_path(Path::new("node_modules/a/b.js")));
        assert!(!is_material_verify_path(Path::new(".git/HEAD")));
    }

    #[test]
    fn non_source_templates_and_data_are_material_0549() {
        // 0549 D8: the 0321 `notes.txt` lock is deliberately overturned; an
        // embedded template or fixture must not read as a clean tree.
        assert!(is_material_verify_path(Path::new("notes.txt")));
        assert!(is_material_verify_path(Path::new("templates/message.txt")));
        assert!(is_material_verify_path(Path::new("data/x.csv")));
        assert!(is_material_verify_path(Path::new("assets/logo.png")));
        assert!(is_material_verify_path(Path::new("tests/fixtures/a.bin")));
    }

    #[test]
    fn packaging_homebrew_rb_is_material() {
        assert!(is_material_verify_path(Path::new(
            "packaging/homebrew/ledgerful.rb"
        )));
        assert!(is_material_verify_path(Path::new(
            "packaging/scoop/ledgerful.json"
        )));
    }
}

#[cfg(test)]
mod overlay_fast_tests {
    use super::{is_material_verify_path, overlay_fast_classifier_packet};
    use crate::impact::packet::{ChangedFile, ImpactPacket};
    use crate::state::layout::Layout;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use tempfile::tempdir;

    fn git(cwd: &Path, args: &[&str]) -> std::process::Output {
        Command::new("git")
            .args(args)
            .current_dir(cwd)
            .env("GIT_AUTHOR_NAME", "test")
            .env("GIT_AUTHOR_EMAIL", "test@example.com")
            .env("GIT_COMMITTER_NAME", "test")
            .env("GIT_COMMITTER_EMAIL", "test@example.com")
            .output()
            .expect("git command")
    }

    fn init_repo_with_commit(dir: &Path) {
        assert!(git(dir, &["init", "-b", "main"]).status.success());
        assert!(
            git(dir, &["config", "user.email", "test@example.com"])
                .status
                .success()
        );
        assert!(git(dir, &["config", "user.name", "test"]).status.success());
        fs::write(dir.join("README.md"), "hello\n").expect("write tracked");
        assert!(git(dir, &["add", "."]).status.success());
        assert!(git(dir, &["commit", "-m", "init"]).status.success());
    }

    fn overlay_paths(dir: &Path, snapshot: ImpactPacket) -> Vec<String> {
        let root = camino::Utf8Path::from_path(dir).expect("utf8 path");
        let layout = Layout::new(root);
        let overlaid = overlay_fast_classifier_packet(&layout, snapshot).expect("overlay");
        let mut paths: Vec<String> = overlaid
            .changes
            .iter()
            .map(|c| c.path.to_string_lossy().replace('\\', "/"))
            .collect();
        paths.sort();
        paths
    }

    fn snapshot_with_src() -> ImpactPacket {
        ImpactPacket {
            head_hash: Some("stale-snapshot-hash".to_string()),
            changes: vec![ChangedFile {
                path: PathBuf::from("src/foo.rs"),
                status: "Modified".to_string(),
                ..Default::default()
            }],
            ..ImpactPacket::default()
        }
    }

    fn unknown_live_plan(
        root: &Path,
        snapshot: Option<&ImpactPacket>,
        profile: &crate::platform::repository::RepositoryProfile,
        allow_full: bool,
    ) -> crate::verify::plan::VerificationPlan {
        let layout = Layout::new(camino::Utf8Path::from_path(root).unwrap());
        super::build_fast_scoped_from_overlay_or_fail_closed(
            snapshot,
            &layout,
            &crate::policy::rules::Rules::default(),
            &[],
            &crate::config::model::VerifyConfig::default(),
            profile,
            None,
            false,
            allow_full,
            Some(crate::verify::plan::RustCrates::Unknown("unreadable")),
        )
    }

    #[rstest::rstest]
    #[case("docs/example.txt", false)]
    #[case("LICENSE", false)]
    #[case("notes.txt", true)]
    fn unknown_detection_uses_actual_live_path_cheapness(
        #[case] path: &str,
        #[case] refused: bool,
    ) {
        let dir = tempdir().unwrap();
        init_repo_with_commit(dir.path());
        let changed = dir.path().join(path);
        fs::create_dir_all(changed.parent().unwrap()).unwrap();
        fs::write(changed, "changed").unwrap();
        let snapshot = snapshot_with_src();
        let plan = unknown_live_plan(
            dir.path(),
            Some(&snapshot),
            &crate::platform::repository::RepositoryProfile::default(),
            false,
        );
        assert_eq!(plan.refused, refused, "{:?}", plan.fallback_reason);
        assert!(plan.steps.is_empty());
        if refused {
            assert!(
                plan.fallback_reason
                    .as_deref()
                    .unwrap()
                    .contains("Rust detection inconclusive: unreadable")
            );
        } else {
            assert_eq!(plan.fallback_reason, None);
        }
    }

    #[test]
    fn unknown_detection_with_failed_overlay_does_not_trust_cheap_snapshot() {
        let dir = tempdir().unwrap(); // intentionally not a Git repository
        let snapshot = ImpactPacket {
            changes: vec![ChangedFile {
                path: "README.md".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let plan = unknown_live_plan(
            dir.path(),
            Some(&snapshot),
            &crate::platform::repository::RepositoryProfile::default(),
            true,
        );
        assert!(plan.refused);
        assert!(plan.steps.is_empty());
        assert!(
            plan.fallback_reason
                .as_deref()
                .unwrap()
                .contains("Rust detection inconclusive: unreadable")
        );
    }

    #[test]
    fn unknown_detection_cheap_live_path_keeps_broken_root_manifest_guard() {
        let dir = tempdir().unwrap();
        init_repo_with_commit(dir.path());
        fs::write(dir.path().join("Cargo.toml"), "not valid toml {{{").unwrap();
        fs::write(dir.path().join("LICENSE"), "changed").unwrap();
        let profile = crate::platform::repository::detect_repository(dir.path());
        let plan = unknown_live_plan(dir.path(), None, &profile, false);
        assert!(plan.refused);
        assert!(
            plan.fallback_reason
                .as_deref()
                .unwrap()
                .contains("malformed Cargo.toml")
        );
    }

    #[test]
    fn overlay_fast_replaces_snapshot_changes_not_union() {
        let dir = tempdir().expect("tempdir");
        init_repo_with_commit(dir.path());
        fs::write(dir.path().join("CHANGELOG.md"), "unreleased\n").expect("changelog");
        fs::create_dir_all(dir.path().join("docs")).expect("docs dir");
        fs::write(dir.path().join("docs").join("installation.md"), "docs\n").expect("docs");

        let paths = overlay_paths(dir.path(), snapshot_with_src());
        assert!(
            paths.iter().any(|p| p == "CHANGELOG.md"),
            "live CHANGELOG must replace snapshot, got {paths:?}"
        );
        assert!(
            paths.iter().any(|p| p == "docs/installation.md"),
            "live docs must be classified, got {paths:?}"
        );
        assert!(
            !paths.iter().any(|p| p == "src/foo.rs"),
            "must replace snapshot src, not union, got {paths:?}"
        );
    }

    #[test]
    fn overlay_fast_sets_live_head_hash() {
        let dir = tempdir().expect("tempdir");
        init_repo_with_commit(dir.path());
        fs::write(dir.path().join("CHANGELOG.md"), "unreleased\n").expect("changelog");
        let root = camino::Utf8Path::from_path(dir.path()).expect("utf8 path");
        let layout = Layout::new(root);
        let overlaid =
            overlay_fast_classifier_packet(&layout, snapshot_with_src()).expect("overlay");
        let repo = crate::git::repo::open_repo(dir.path()).expect("open");
        let (live_head, _) = crate::git::repo::get_head_info(&repo).expect("head");
        assert_eq!(
            overlaid.head_hash, live_head,
            "overlay head_hash must be live HEAD, not snapshot"
        );
        assert_ne!(
            overlaid.head_hash.as_deref(),
            Some("stale-snapshot-hash"),
            "must not keep snapshot head"
        );
    }

    #[test]
    fn overlay_fast_retains_packaging_rb_drops_ledgerful_and_cargo_bat() {
        let dir = tempdir().expect("tempdir");
        init_repo_with_commit(dir.path());
        let pkg = dir.path().join("packaging").join("homebrew");
        fs::create_dir_all(&pkg).expect("packaging dir");
        fs::write(pkg.join("ledgerful.rb"), "class Ledgerful\n").expect("rb");
        let state = dir.path().join(".ledgerful").join("state");
        fs::create_dir_all(&state).expect("ledgerful state");
        fs::write(state.join("x"), "nope\n").expect("state file");
        fs::write(dir.path().join("cargo.bat"), "@echo off\n").expect("cargo.bat");

        let paths = overlay_paths(dir.path(), ImpactPacket::default());
        assert!(
            paths.iter().any(|p| p == "packaging/homebrew/ledgerful.rb"),
            "packaging rb must be retained, got {paths:?}"
        );
        assert!(
            !paths.iter().any(|p| p.contains(".ledgerful")),
            ".ledgerful must be ignore-filtered, got {paths:?}"
        );
        assert!(
            !paths.iter().any(|p| p.eq_ignore_ascii_case("cargo.bat")),
            "cargo.bat must not be classified, got {paths:?}"
        );
        assert!(!is_material_verify_path(Path::new("cargo.bat")));
        assert!(!is_material_verify_path(Path::new(".ledgerful/state/x")));
    }
}

#[cfg(test)]
mod execute_verify_json_gate_tests {
    /// DoD-15: clap-level fatal rejects before `execute_verify` runs — no path
    /// that could emit a partial `VerifyCliJson` payload.
    #[test]
    fn verify_json_invalid_scope_is_clap_fatal() {
        use crate::cli::Cli;
        use clap::Parser;
        let err = Cli::try_parse_from(["ledgerful", "verify", "--json", "--scope", "not-a-scope"]);
        assert!(
            err.is_err(),
            "invalid --scope under --json must fail at clap (fatal, no partial JSON)"
        );
        let msg = err.unwrap_err().to_string();
        assert!(
            msg.contains("not-a-scope") || msg.contains("scope") || msg.contains("fast"),
            "clap error should mention scope; got {msg}"
        );
    }

    /// DoD-15: mix-refuse stays an error (health+dry-run). Allowed diagnostic
    /// `--json` paths emit sibling kinds, not executed `VerifyCliJson`.
    #[test]
    fn verify_json_rejected_combos_are_errors_not_partial_payloads() {
        let src = include_str!("execute.rs");
        assert!(
            src.contains("refuse_mixed_verify_diagnostics(true, dry_run"),
            "health+dry-run must still refuse in execute"
        );
        assert!(
            src.contains("emit_verify_health_json"),
            "health+json must emit verifyHealth"
        );
        assert!(
            src.contains("emit_verify_dry_run_json"),
            "dry-run+json must emit verifyDryRun"
        );
        let prod = src
            .split("#[cfg(test)]")
            .next()
            .expect("production body before unit tests");
        assert!(
            !prod.contains("verify --json cannot be combined with --health"),
            "stale health+json refuse must not block the allow path"
        );
        assert!(
            !prod.contains("verify --json cannot be combined with --dry-run"),
            "stale dry-run+json refuse must not block the allow path"
        );
        // Emit-before-err boundary: executed JSON println is immediately before overall_pass check.
        let emit_idx = src
            .find("payload.to_json_string()")
            .expect("JSON emit site");
        let fail_idx = src
            .find("if report.overall_pass")
            .expect("overall_pass gate");
        assert!(
            emit_idx < fail_idx,
            "DoD-15 boundary: JSON must emit before validation-rejection Err"
        );
    }

    /// 0144 B1: dry-run must not print the plan-banner wall; both call sites gate
    /// `print_verify_plan` with `!dry_run` (and verbose && !json).
    #[test]
    fn execute_verify_print_verify_plan_gated_off_dry_run() {
        let src = include_str!("execute.rs");
        // Production body only — include_str also sees this test module.
        let prod = src
            .split("#[cfg(test)]")
            .next()
            .expect("production body before unit tests");
        let mut from = 0usize;
        let mut gated_sites = 0usize;
        while let Some(rel) = prod[from..].find("print_verify_plan(") {
            let abs = from + rel;
            // Look back a short window for the dry-run gate on the same call site.
            let window_start = abs.saturating_sub(120);
            let window = &prod[window_start..abs];
            assert!(
                window.contains("!dry_run"),
                "print_verify_plan at byte {abs} must be gated by !dry_run; nearby: {window:?}"
            );
            gated_sites += 1;
            from = abs + "print_verify_plan(".len();
        }
        assert!(
            gated_sites >= 2,
            "expected both config_plan and plan print_verify_plan sites; found {gated_sites}"
        );
    }

    /// 0288 DoD-3: auto-policy dry-run on a fully-clean tree must skip
    /// `OutcomePredictor::predict` via the material-changes gate — not by
    /// assigning `ctx.no_predict`. Bayesian `extract_dataset` stays after.
    #[test]
    fn execute_verify_clean_dry_run_skips_predict_not_no_predict_flag() {
        let src = include_str!("execute.rs");
        let prod = src
            .split("#[cfg(test)]")
            .next()
            .expect("production body before unit tests");
        assert!(
            !prod.contains("ctx.no_predict ="),
            "DoD-3: do not set ctx.no_predict (storage/packet warning gates)"
        );
        let predict_at = prod
            .find("OutcomePredictor::predict(&mut ctx)")
            .expect("predict call");
        let window_start = predict_at.saturating_sub(280);
        let gate = &prod[window_start..predict_at];
        assert!(
            gate.contains("dry_run")
                && gate.contains("working_tree_has_material_changes(working_material)"),
            "clean dry-run must gate predict with dry_run + the material snapshot; nearby: {gate:?}"
        );
        // 0549 D8 hoists ONE material snapshot and feeds it to prediction,
        // overlay, and JSON, so the gate argument is no longer the raw
        // `layout.root.as_std_path()` this test used to look for. Assert the
        // equivalent fact instead: that snapshot is still classified from the
        // real repo root path, not from an abstract flag.
        assert!(
            prod.contains("classify_working_tree_material(layout.root.as_std_path(), detect_rust)"),
            "material snapshot must be classified from the real repo root path"
        );
        let extract_at = prod
            .find("probability::extract_dataset")
            .expect("Bayesian extract_dataset");
        assert!(
            predict_at < extract_at,
            "extract_dataset must remain after the predict arm"
        );
    }
}

#[cfg(test)]
mod rust_detection_trigger_tests {
    use super::*;
    use crate::verify::plan::RustCrates;
    use std::cell::Cell;
    use std::path::{Path, PathBuf};

    fn change(path: &str) -> crate::git::FileChange {
        crate::git::FileChange {
            path: PathBuf::from(path),
            change_type: crate::git::ChangeType::Modified,
            is_staged: false,
        }
    }

    #[rstest::rstest]
    #[case(&[], true)]
    #[case(&["README.md"], true)]
    #[case(&["docs/guide.md", "data/a.json", "data/b.yaml", "data/c.sql"], true)]
    #[case(&["message.txt"], false)]
    fn not_needed_never_calls_discovery(#[case] paths: &[&str], #[case] enabled: bool) {
        let tmp = tempfile::tempdir().unwrap();
        // Even root presence cannot bypass the single materiality trigger.
        std::fs::write(tmp.path().join("Cargo.toml"), "[package]\nname=\"fx\"\n").unwrap();
        let changes: Vec<_> = paths.iter().map(|p| change(p)).collect();
        let (_, memo) = classify_material_changes(tmp.path(), &changes, enabled, |_| {
            panic!("NotNeeded must not discover Rust");
        });
        assert_eq!(memo, None);
    }

    #[rstest::rstest]
    #[case("Target/x.rs")]
    #[case("Node_Modules/a.js")]
    #[case(".LEDGERFUL/state/x.db")]
    #[case(".GIT/HEAD")]
    fn leading_exempt_components_share_platform_case_policy(#[case] path: &str) {
        assert_eq!(is_material_verify_path(Path::new(path)), !cfg!(windows));
        assert!(is_material_verify_path(Path::new("targeted/x.rs")));
    }

    #[test]
    fn multiple_strict_paths_compute_one_unknown_memo_and_stay_dirty() {
        let calls = Cell::new(0);
        let (material, memo) = classify_material_changes(
            Path::new("unused"),
            &[change("a.txt"), change("b.csv")],
            true,
            |_| {
                calls.set(calls.get() + 1);
                RustCrates::Unknown("unreadable")
            },
        );
        assert_eq!(calls.get(), 1);
        assert_eq!(material, WorkingTreeMaterial::Dirty);
        assert_eq!(memo, Some(RustCrates::Unknown("unreadable")));
    }

    #[test]
    fn absent_rust_keeps_non_source_legacy_clean() {
        let (material, memo) =
            classify_material_changes(Path::new("unused"), &[change("notes.txt")], true, |_| {
                RustCrates::Absent
            });
        assert_eq!(material, WorkingTreeMaterial::Clean);
        assert_eq!(memo, Some(RustCrates::Absent));
    }

    #[test]
    fn no_root_manifest_triggers_detection_for_a_docs_only_change() {
        // 0549 DoD-2: a nested-only repo must not return a zero-step green for
        // a referenced document. With no root manifest the root-based profile
        // cannot rule out crates below the root, so discovery must run even
        // though a `.md` edit matches strict *and* legacy materiality.
        let tmp = tempfile::tempdir().unwrap();
        let calls = Cell::new(0);
        let (material, memo) =
            classify_material_changes(tmp.path(), &[change("docs/contract.md")], true, |_| {
                calls.set(calls.get() + 1);
                RustCrates::Present
            });
        assert_eq!(calls.get(), 1);
        assert_eq!(material, WorkingTreeMaterial::Dirty);
        assert_eq!(memo, Some(RustCrates::Present));
    }

    #[test]
    fn root_manifest_keeps_a_docs_only_change_from_walking() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("Cargo.toml"), "[package]\nname=\"fx\"\n").unwrap();
        let (material, memo) =
            classify_material_changes(tmp.path(), &[change("docs/contract.md")], true, |_| {
                panic!("a root manifest plus a docs-only edit must not walk");
            });
        assert_eq!(material, WorkingTreeMaterial::Dirty);
        assert_eq!(memo, None);
    }

    #[test]
    fn no_root_manifest_still_respects_detect_rust_off() {
        let tmp = tempfile::tempdir().unwrap();
        let (material, memo) =
            classify_material_changes(tmp.path(), &[change("docs/contract.md")], false, |_| {
                panic!("detect_rust=false must not walk")
            });
        assert_eq!(material, WorkingTreeMaterial::Dirty);
        assert_eq!(memo, None);
    }

    #[test]
    fn no_root_manifest_does_not_walk_for_a_non_cheap_legacy_only_change() {
        // `notes/guide.md` is legacy material (`.md`) but not cheap-set, so it
        // reaches the mapping/refusal route instead of the cheap plan. Walking
        // here would only add cost and could over-refuse a non-Rust repo.
        let tmp = tempfile::tempdir().unwrap();
        let (material, memo) =
            classify_material_changes(tmp.path(), &[change("notes/guide.md")], true, |_| {
                panic!("a non-cheap legacy-only change must not walk")
            });
        assert_eq!(material, WorkingTreeMaterial::Dirty);
        assert_eq!(memo, None);
    }
}
