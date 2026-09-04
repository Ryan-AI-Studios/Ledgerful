//! `ledgerful verify` execution path (0251 extract from `verify/mod.rs`).

use crate::output::human::print_verify_plan;
use crate::output::verification::{
    VerificationReporter, dry_run_scope_line, print_dry_run_human, should_print_suggested_actions,
};
use crate::state::storage::StorageManager;
use crate::verify::engine::{VerificationContext, VerifyEngine};
use crate::verify::plan::{VerificationStep, VerifyScope, build_plan_from_config};
use crate::verify::predictor::OutcomePredictor;
use crate::verify::results::VerificationReport;
use crate::verify::suggestions::{generate_suggestions, query_ledger_status};
use crate::verify::timeouts::manual_timeout;
use miette::Result;
use owo_colors::{OwoColorize, Stream, Style};
use std::env;
use tracing::{debug, warn};

use super::dto::VerifyCliJson;
use super::health::execute_verify_health;
use super::mapping::{TestMappingState, explain_test_mappings, step_relevant_to_entity};

/// Named options for [`execute_verify`] (0191). Signature flags stay in dispatch.
#[derive(Debug, Clone)]
pub struct ExecuteVerifyOpts {
    pub command: Option<String>,
    pub tx_id: Option<String>,
    pub timeout_secs: u64,
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
        if json {
            // Health is a separate surface; --json is for the plan execution payload.
            return Err(miette::miette!(
                "verify --json cannot be combined with --health"
            ));
        }
        return execute_verify_health(&layout, &config);
    }

    // Bayesian apply hit count for honesty log + VerifyCliJson.matchedSteps (0140).
    // None = ordering not attempted; Some(n) = extract_dataset succeeded.
    let mut bayesian_matched_steps: Option<usize> = None;
    // Probability map size when extract_dataset succeeds (0144 dry-run stdout).
    let mut bayesian_dataset_keys: Option<usize> = None;

    // 3. Build Plan
    let (plan, steps) = match command_str {
        Some(ref cmd) => (
            None,
            vec![manual_step(cmd.clone(), manual_timeout(timeout_secs))],
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
                let prediction = OutcomePredictor::predict(&mut ctx)?;
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
                        if scope.is_fast()
                            && !working_tree_has_material_changes(layout.root.as_std_path())
                        {
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
                            if working_tree_has_material_changes(layout.root.as_std_path()) {
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
                        println!("    • {}", t);
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
            if json {
                return Err(miette::miette!(
                    "verify --json cannot be combined with --dry-run"
                ));
            }
            // P5: scope line first, then ℹ/Next, then the refused footer.
            println!("{}", dry_run_scope_line(scope));
            let reason = plan
                .as_ref()
                .and_then(|p| p.fallback_reason.clone())
                .unwrap_or_else(|| "fast scope unavailable; refusing full suite".to_string());
            print_fallback_announcement(&reason, true);
            println!(
                "\n{}",
                "Dry run mode: plan refused — no commands would be executed."
                    .if_supports_color(Stream::Stdout, |s| s.yellow())
            );
            return Err(miette::miette!("{reason}"));
        }

        // Live refuse: emit report/JSON with ok:false, empty steps; no cargo.
        let mut report = VerificationReport::new(plan, Vec::new());
        report.overall_pass = false;
        if json {
            let payload = VerifyCliJson::from_report(&report, scope, bayesian_matched_steps);
            println!("{}", payload.to_json_string()?);
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
        if json {
            return Err(miette::miette!(
                "verify --json cannot be combined with --dry-run"
            ));
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
                timeout_secs
            );
            println!();
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
    if !json {
        if !report.overall_pass
            && let Some(block) = crate::verify::fail_block::format_fail_block_from_report(&report)
        {
            println!("{block}");
        }

        if should_print_suggested_actions(verbose, report.overall_pass) {
            VerificationReporter::report(&ctx, &report);
        } else {
            // Quiet green: still surface prediction warnings on stderr; no
            // Suggested Actions header on stdout.
            if !report.prediction_warnings.is_empty() {
                VerificationReporter::print_prediction_warnings(&report.prediction_warnings);
            }
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
        let payload = VerifyCliJson::from_report(&report, scope, bayesian_matched_steps);
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
fn overlay_fast_classifier_packet(
    layout: &crate::state::layout::Layout,
    mut packet: crate::impact::packet::ImpactPacket,
) -> miette::Result<crate::impact::packet::ImpactPacket> {
    let live = crate::git::status::collect_changed_files_for_filter(layout)?;
    let mut changes: Vec<crate::impact::packet::ChangedFile> = live
        .into_iter()
        .filter(|c| is_material_or_cheap_verify_path(&c.path))
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

fn is_material_or_cheap_verify_path(path: &std::path::Path) -> bool {
    is_material_verify_path(path) || crate::verify::plan::is_non_code_cheap_path(path)
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
) -> crate::verify::plan::VerificationPlan {
    let base = snapshot.cloned().unwrap_or_default();
    let overlaid = match overlay_fast_classifier_packet(layout, base) {
        Ok(packet) => packet,
        Err(err) => {
            warn!("fast-scope live overlay failed: {err}");
            match snapshot {
                Some(packet) => {
                    return crate::verify::plan::build_plan_scoped_with_options(
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
    crate::verify::plan::build_plan_scoped_with_options(
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
    )
}

/// True when the working tree has **material** changes that verify should not
/// ignore when there is no saved impact packet (0135 final codex P1).
///
/// Material = source-like extensions or known shared-infra basenames. Ignores
/// `.ledgerful/**` (also default ignore) and PATH/test fixtures such as a
/// root `cargo.bat` used by empty-repo integration tests.
///
/// On git discovery/status failure, returns false so clean EmptyChanges still
/// works in non-git fixtures.
fn working_tree_has_material_changes(repo_root: &std::path::Path) -> bool {
    let Ok(repo) = crate::git::repo::open_repo(repo_root) else {
        return false;
    };
    let Ok(changes) = crate::git::status::get_repo_status(&repo) else {
        return false;
    };
    changes.iter().any(|c| is_material_verify_path(&c.path))
}

fn is_material_verify_path(path: &std::path::Path) -> bool {
    let norm = path.to_string_lossy().replace('\\', "/");
    let norm = norm.trim_start_matches("./");
    if norm.starts_with(".ledgerful/") || norm == ".ledgerful" {
        return false;
    }
    // PATH/test shim used by empty-repo verify tests — not product source.
    if norm.eq_ignore_ascii_case("cargo.bat")
        || norm.eq_ignore_ascii_case("cargo.cmd")
        || norm.eq_ignore_ascii_case("cargo")
    {
        return false;
    }
    // 0203-D: packaging templates (including `.rb`) are material so a
    // template-only diff is not LiveEmpty.
    if norm == "packaging" || norm.starts_with("packaging/") {
        return true;
    }
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    // Shared-infra basenames that already force full under --scope fast when
    // present in a packet (see plan::touches_shared_infra).
    const INFRA: &[&str] = &[
        "cargo.toml",
        "cargo.lock",
        "package.json",
        "package-lock.json",
        "pnpm-lock.yaml",
        "yarn.lock",
        "go.mod",
        "go.sum",
        "pyproject.toml",
        "requirements.txt",
        "poetry.lock",
        "dockerfile",
        "docker-compose.yml",
        "docker-compose.yaml",
        "makefile",
    ];
    if INFRA.iter().any(|b| name == *b) {
        return true;
    }
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    matches!(
        ext.as_str(),
        "rs" | "ts"
            | "tsx"
            | "js"
            | "jsx"
            | "mjs"
            | "cjs"
            | "py"
            | "go"
            | "java"
            | "kt"
            | "kts"
            // D2 C/C++ (aligned with analysis/semantic index support)
            | "c"
            | "h"
            | "cpp"
            | "cc"
            | "cxx"
            | "hpp"
            | "hh"
            | "hxx"
            | "h++"
            | "toml"
            | "yaml"
            | "yml"
            | "json"
            | "md"
            | "css"
            | "scss"
            | "html"
            | "sql"
            | "sh"
            | "ps1"
    )
}

fn manual_step(command: String, timeout_secs: u64) -> VerificationStep {
    VerificationStep {
        description: "Manually requested verification command".to_string(),
        command,
        timeout_secs,
        shell: true,
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
        assert!(!is_material_verify_path(Path::new("notes.txt")));
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

    /// DoD-15: rejected `--json` combinations return `Err` from dispatch helpers
    /// without building a report (no `VerifyCliJson` emit site reached).
    #[test]
    fn verify_json_rejected_combos_are_errors_not_partial_payloads() {
        // Structural: execute_verify returns Err early for health/dry-run+json
        // before plan execution or JSON println. Live process proof is in
        // integration + output/0093-after/verify-json-fatal.*.
        let src = include_str!("execute.rs");
        assert!(
            src.contains("verify --json cannot be combined with --health"),
            "health+json must reject"
        );
        assert!(
            src.contains("verify --json cannot be combined with --dry-run"),
            "dry-run+json must reject"
        );
        // Emit-before-err boundary: JSON println is immediately before overall_pass check.
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
}
