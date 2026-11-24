use crate::change_evidence::pack_store::capture_budget::CaptureBudget;
use crate::change_evidence::pack_store::capture_db::QualifiedCaptureStorage;
use crate::config::model::Config;
use crate::exec::ExecutionResult;
use crate::impact::packet::ImpactPacket;
use crate::output::human::print_verify_result;
use crate::output::verification::{
    format_verify_step_ok, format_verify_step_start, should_emit_verify_step_progress,
};
use crate::state::layout::Layout;
use crate::state::storage::StorageManager;
use crate::state::storage::verification::{
    CaptureUnavailable, PreparedExecutedAttempt, VerificationCaptureCommit,
};
use crate::verify::plan::{VerificationPlan, VerificationStep};
use crate::verify::results::{VerificationReport, VerificationResult, write_verify_report};
use crate::verify::runner::{
    StepExecution, VERIFY_STEP_TIMEOUT_EXIT_CODE, execute_step_outcome,
    execute_step_outcome_metered, prepare_manual_step, prepare_rule_step,
};
use chrono::Utc;
use miette::Result;
use std::path::PathBuf;
use tracing::{info, warn};

pub struct VerificationContext {
    pub layout: Layout,
    pub current_dir: PathBuf,
    pub config: Config,
    pub packet: Option<ImpactPacket>,
    pub storage: Option<StorageManager>,
    pub no_predict: bool,
    pub explain: bool,
    pub health: bool,
    pub warnings: Vec<String>,
    /// When true, suppress human `println!` progress (e.g. `verify --json`).
    pub suppress_human_output: bool,
    /// When true, emit per-step SUCCESS lines, plan banner chatter, and
    /// progress `info!`. Quiet success (default) keeps FAILURE lines only.
    /// Independent of `suppress_human_output` — never set suppress from `!verbose`.
    pub verbose: bool,
}

impl VerificationContext {
    pub fn new(
        layout: Layout,
        current_dir: PathBuf,
        config: Config,
        no_predict: bool,
        explain: bool,
        health: bool,
    ) -> Self {
        Self {
            layout,
            current_dir,
            config,
            packet: None,
            storage: None,
            no_predict,
            explain,
            health,
            warnings: Vec::new(),
            suppress_human_output: false,
            verbose: false,
        }
    }

    pub fn add_warning(&mut self, warning: String) {
        self.warnings.push(warning);
    }
}

/// Check whether `cargo nextest` is available on PATH.
pub fn probe_nextest() -> bool {
    std::process::Command::new("cargo")
        .args(["nextest", "--version"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[path = "execution_origin.rs"]
mod execution_origin;
use execution_origin::ExecutionCollector;
pub(crate) use execution_origin::{ExecutionTerminal, MAX_CAPTURE_POSITIONS};

/// Awaiting the caller's actual postflight revision/scope proof. The returned
/// writer owns the original qualified attempt when that proof succeeds.
pub(crate) type ExecutePostflight<'call, 'scope> = dyn FnOnce(
        &Layout,
        &CompletedExecution<'_>,
    ) -> std::result::Result<
        (QualifiedCaptureStorage<'scope>, PreparedExecutedAttempt),
        CaptureUnavailable,
    > + 'call;

pub(crate) struct CompletedExecution<'a> {
    report: VerificationReport,
    terminal: Option<ExecutionTerminal>,
    capture_commit: Option<VerificationCaptureCommit>,
    steps: &'a [VerificationStep],
    scope: crate::verify::plan::VerifyScope,
    manual_requested: bool,
}

impl<'a> CompletedExecution<'a> {
    pub(crate) fn report(&self) -> &VerificationReport {
        &self.report
    }
    pub(crate) fn steps(&self) -> &'a [VerificationStep] {
        self.steps
    }
    pub(crate) fn scope(&self) -> crate::verify::plan::VerifyScope {
        self.scope
    }

    pub(crate) fn manual_requested(&self) -> bool {
        self.manual_requested
    }

    pub(crate) fn terminal(&self) -> Option<&ExecutionTerminal> {
        self.terminal.as_ref()
    }
    pub(crate) fn capture_commit(&self) -> Option<VerificationCaptureCommit> {
        self.capture_commit
    }

    pub(crate) fn add_suggestions(
        &mut self,
        suggestions: Vec<crate::verify::suggestions::Suggestion>,
    ) {
        self.report.suggested_actions = suggestions;
    }
}

#[cfg(test)]
impl CompletedExecution<'static> {
    pub(crate) fn for_storage_test(report: VerificationReport) -> Self {
        let mut collector = ExecutionCollector::new(report.results.len());
        for (position, result) in report.results.iter().enumerate() {
            collector.record_finished(position, result);
        }
        Self {
            report,
            terminal: Some(collector.complete()),
            capture_commit: None,
            steps: &[],
            scope: crate::verify::plan::VerifyScope::Full,
            manual_requested: true,
        }
    }
}

pub struct VerifyEngine;

impl VerifyEngine {
    pub fn execute(
        ctx: &mut VerificationContext,
        plan: Option<VerificationPlan>,
        steps: &[VerificationStep],
        manual_requested: bool,
        tx_id: Option<String>,
    ) -> Result<VerificationReport> {
        Self::execute_with_scope(
            ctx,
            plan,
            steps,
            manual_requested,
            tx_id,
            crate::verify::plan::VerifyScope::Full,
        )
    }

    pub fn execute_with_scope(
        ctx: &mut VerificationContext,
        plan: Option<VerificationPlan>,
        steps: &[VerificationStep],
        manual_requested: bool,
        tx_id: Option<String>,
        scope: crate::verify::plan::VerifyScope,
    ) -> Result<VerificationReport> {
        Self::execute_collecting(
            ctx,
            plan,
            steps,
            manual_requested,
            tx_id,
            scope,
            false,
            None,
            None,
        )
        .map(|execution| execution.report)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn execute_collecting<'a>(
        ctx: &mut VerificationContext,
        plan: Option<VerificationPlan>,
        steps: &'a [VerificationStep],
        manual_requested: bool,
        tx_id: Option<String>,
        scope: crate::verify::plan::VerifyScope,
        capture: bool,
        postflight: Option<Box<ExecutePostflight<'_, '_>>>,
        capture_budget: Option<(&CaptureBudget, &std::path::Path)>,
    ) -> Result<CompletedExecution<'a>> {
        let mut capture =
            (capture || postflight.is_some()).then(|| ExecutionCollector::new(steps.len()));
        let mut persisted_results = Vec::new();
        let mut overall_success = true;
        let policy = ctx.config.verify.effective_process_policy();
        // Empty steps: loop body never runs — no [1/0] product lines.
        let n = steps.len();

        for (idx, step) in steps.iter().enumerate() {
            let i = idx + 1;
            let prepared = if manual_requested {
                prepare_manual_step(step)
            } else {
                prepare_rule_step(step, ctx.config.verify.allow_shell_steps, &policy)?
            };

            // Product step-start progress (0148): greppable `[i/n] Running:` on
            // human path only — never under --json. Not tracing::info! so 0154
            // cannot strip it.
            if should_emit_verify_step_progress(ctx.suppress_human_output) {
                println!(
                    "{}",
                    format_verify_step_start(i, n, &prepared.display_command)
                );
            }

            // Progress INFO must not hit stderr under machine mode (`verify --json`)
            // or quiet (non-verbose) success path. Structural subscriber filter also
            // raises normal_layer to WARN under --json; demote here so even a
            // misconfigured RUST_LOG cannot reintroduce the line. Never reuse
            // suppress_human_output for quiet-success (that would kill FAILURE).
            if ctx.suppress_human_output || !ctx.verbose {
                tracing::debug!(
                    "Running verification command via {:?}: {}",
                    prepared.execution_mode,
                    prepared.display_command
                );
            } else {
                info!(
                    "Running verification command via {:?}: {}",
                    prepared.execution_mode, prepared.display_command
                );
            }

            // Local fast-path speed lever: enable incremental compilation for
            // warm rebuilds. Only on the fast convenience scope; never in CI or
            // on the authoritative full scope. Mutually exclusive with sccache
            // (which requires CARGO_INCREMENTAL=0), so this is only set when
            // no RUSTC_WRAPPER is present.
            let has_rustc_wrapper = std::env::var("RUSTC_WRAPPER").is_ok();
            let mut command = std::process::Command::new(&prepared.executable);
            command.args(&prepared.args);
            if should_enable_incremental(
                scope,
                &prepared.display_command,
                is_ci(),
                has_rustc_wrapper,
            ) {
                command.env("CARGO_INCREMENTAL", "1");
            }

            let outcome = match capture_budget {
                Some(budget) => {
                    execute_step_outcome_metered(&prepared, &policy, Some(command), Some(budget))
                }
                None => execute_step_outcome(&prepared, &policy, Some(command)),
            };
            match outcome? {
                StepExecution::Finished(result) => {
                    // Product step-done (0148 matrix):
                    // - default pass: compact ok + elapsed (no SUCCESS banner)
                    // - verbose pass: SUCCESS via print_verify_result (no compact ok)
                    // - fail (default or verbose): FAILURE via print_verify_result
                    // - json: never println
                    if should_emit_verify_step_progress(ctx.suppress_human_output) {
                        if result.exit_code == 0 && !ctx.verbose {
                            println!(
                                "{}",
                                format_verify_step_ok(
                                    i,
                                    n,
                                    &prepared.display_command,
                                    result.duration
                                )
                            );
                        }
                        if result.exit_code != 0 || ctx.verbose {
                            print_verify_result(
                                &prepared.display_command,
                                step.timeout_secs,
                                &result,
                                ctx.verbose,
                            );
                        }
                    }

                    let report_result = Self::to_report_result(&prepared.display_command, &result);
                    if report_result.exit_code != 0 {
                        overall_success = false;
                    }
                    if let Some(capture) = &mut capture {
                        capture.record_finished(idx, &report_result);
                    }
                    persisted_results.push(report_result);
                }
                StepExecution::TimedOut { timeout, message } => {
                    overall_success = false;
                    let stderr_summary = Self::truncate_summary(&message);
                    let truncated = stderr_summary.chars().count() < message.chars().count();
                    let report_result = VerificationResult {
                        command: prepared.display_command.clone(),
                        exit_code: VERIFY_STEP_TIMEOUT_EXIT_CODE,
                        duration_ms: timeout.as_millis() as u64,
                        stdout_summary: String::new(),
                        stderr_summary,
                        truncated,
                        timestamp: Utc::now().to_rfc3339(),
                    };
                    if let Some(capture) = &mut capture {
                        capture.record_timeout(idx, &report_result);
                    }
                    persisted_results.push(report_result);
                    eprintln!("{message}");
                    break;
                }
            }
        }

        let mut report = VerificationReport::new(plan, persisted_results.clone())
            .with_warnings(ctx.warnings.clone())
            .with_tx_id(tx_id);
        report.overall_pass = overall_success;

        write_verify_report(&ctx.layout, &report)?;
        let mut execution = CompletedExecution {
            report,
            terminal: capture.map(ExecutionCollector::complete),
            capture_commit: None,
            steps,
            scope,
            manual_requested,
        };

        if let Some(prove) = postflight {
            let result = match execution.terminal() {
                Some(terminal) if terminal.is_consistent() => prove(&ctx.layout, &execution),
                _ => Err(CaptureUnavailable::InvalidOrigin),
            };
            execution.capture_commit = Some(match result {
                Ok((storage, attempt)) => match storage.write_executed(&execution, attempt) {
                    Ok(commit) => commit,
                    Err(reason) => {
                        Self::persist_verify_report(&ctx.layout, execution.report());
                        VerificationCaptureCommit::Unavailable(reason)
                    }
                },
                Err(reason) => {
                    Self::persist_verify_report(&ctx.layout, execution.report());
                    VerificationCaptureCommit::Unavailable(reason)
                }
            });
        } else {
            Self::persist_verify_report(&ctx.layout, execution.report());
        }

        Self::record_semantic_test_outcomes(
            &ctx.layout,
            &Some(ctx.config.clone()),
            &ctx.packet,
            &persisted_results,
        );

        Ok(execution)
    }

    fn to_report_result(command: &str, result: &ExecutionResult) -> VerificationResult {
        VerificationResult {
            command: command.to_string(),
            exit_code: result.exit_code,
            duration_ms: result.duration.as_millis() as u64,
            stdout_summary: Self::truncate_summary(&result.stdout),
            stderr_summary: Self::truncate_summary(&result.stderr),
            truncated: result.truncated,
            timestamp: Utc::now().to_rfc3339(),
        }
    }

    fn truncate_summary(output: &str) -> String {
        output.chars().take(500).collect()
    }

    fn persist_verify_report(layout: &Layout, report: &VerificationReport) {
        let Ok(mut storage) = StorageManager::init_with_layout(layout) else {
            warn!("Could not initialize SQLite for verification report persistence");
            return;
        };

        if let Err(e) = storage.get_connection_mut().execute("BEGIN IMMEDIATE", []) {
            warn!("Failed to begin transaction for verification results: {e}");
            return;
        }

        let plan_json = report
            .plan
            .as_ref()
            .and_then(|plan| serde_json::to_string(plan).ok());

        let Ok(run_id) = storage.save_verification_run(
            &report.timestamp,
            plan_json.as_deref(),
            report.overall_pass,
            report.tx_id.as_deref(),
        ) else {
            warn!("Failed to persist verification run metadata");
            let _ = storage.get_connection_mut().execute("ROLLBACK", []);
            return;
        };

        for result in &report.results {
            if let Err(err) = storage.save_verification_result(
                run_id,
                &result.command,
                result.exit_code,
                result.duration_ms,
                result.truncated,
                report.tx_id.as_deref(),
            ) {
                warn!("Failed to persist verification result: {err}");
                let _ = storage.get_connection_mut().execute("ROLLBACK", []);
                return;
            }
        }

        if let Err(e) = storage.get_connection_mut().execute("COMMIT", []) {
            warn!("Failed to commit transaction for verification results: {e}");
        }
    }

    fn record_semantic_test_outcomes(
        layout: &Layout,
        config: &Option<crate::config::model::Config>,
        packet: &Option<crate::impact::packet::ImpactPacket>,
        results: &[VerificationResult],
    ) {
        let (Some(config), Some(packet)) = (config, packet) else {
            return;
        };

        let Ok(storage) = StorageManager::init_with_layout(layout) else {
            warn!("Failed to open storage for semantic test outcome recording");
            return;
        };

        let diff_text = crate::verify::semantic_predictor::build_diff_text(packet);
        let diff_summary: String = diff_text.chars().take(200).collect();
        let commit_hash = packet.head_hash.clone().unwrap_or_default();

        // Bayesian join (0140): store canonical step key in test_file so
        // extract_dataset / apply_probability_ordering share identity with
        // scoped nextest argv variants. Leave verification_results raw.
        let outcomes: Vec<crate::verify::semantic_predictor::TestOutcome> = results
            .iter()
            .map(|r| crate::verify::semantic_predictor::TestOutcome {
                test_name: r.command.clone(),
                test_file: crate::verify::probability::verify_step_key(&r.command),
                commit_hash: commit_hash.clone(),
                status: if r.exit_code == 0 {
                    crate::verify::semantic_predictor::TestStatus::Passed
                } else {
                    crate::verify::semantic_predictor::TestStatus::Failed
                },
                duration_ms: r.duration_ms,
                diff_summary: diff_summary.clone(),
            })
            .collect();

        if let Err(e) = crate::verify::semantic_predictor::record_test_outcomes(
            storage.get_connection(),
            &config.local_model,
            &outcomes,
            &diff_text,
        ) {
            warn!("Failed to record test outcomes for semantic prediction: {e}");
        }
    }
}

fn is_ci() -> bool {
    // Most CI systems set CI=true, but some use CI=1, CI=yes, or just CI=
    // (present but empty). Any presence of the variable indicates CI.
    std::env::var("CI").is_ok()
}

/// Deterministic helper for the fast-path `CARGO_INCREMENTAL=1` decision.
/// Kept pure so unit tests don't depend on ambient environment variables.
fn should_enable_incremental(
    scope: crate::verify::plan::VerifyScope,
    display_command: &str,
    is_ci: bool,
    has_rustc_wrapper: bool,
) -> bool {
    scope.is_fast() && !is_ci && display_command.starts_with("cargo ") && !has_rustc_wrapper
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_should_enable_incremental_on_fast_non_ci_cargo() {
        assert!(should_enable_incremental(
            crate::verify::plan::VerifyScope::Fast,
            "cargo clippy --all-targets --all-features",
            false,
            false,
        ));
    }

    #[test]
    fn test_should_not_enable_incremental_on_full_scope() {
        assert!(!should_enable_incremental(
            crate::verify::plan::VerifyScope::Full,
            "cargo clippy --all-targets --all-features",
            false,
            false,
        ));
    }

    #[test]
    fn test_should_not_enable_incremental_in_ci() {
        assert!(!should_enable_incremental(
            crate::verify::plan::VerifyScope::Fast,
            "cargo nextest run --workspace",
            true,
            false,
        ));
    }

    #[test]
    fn test_should_not_enable_incremental_when_sccache_active() {
        assert!(!should_enable_incremental(
            crate::verify::plan::VerifyScope::Fast,
            "cargo nextest run --workspace",
            false,
            true,
        ));
    }

    #[test]
    fn test_should_not_enable_incremental_for_non_cargo_command() {
        assert!(!should_enable_incremental(
            crate::verify::plan::VerifyScope::Fast,
            "ledgerful audit",
            false,
            false,
        ));
    }

    fn temp_verify_ctx() -> (tempfile::TempDir, VerificationContext) {
        let tmp = tempfile::tempdir().unwrap();
        let root = camino::Utf8Path::from_path(tmp.path()).unwrap();
        let layout = Layout::new(root);
        let ctx = VerificationContext::new(
            layout,
            tmp.path().to_path_buf(),
            Config::default(),
            false,
            false,
            false,
        );
        (tmp, ctx)
    }

    #[test]
    fn engine_timeout_emits_result_and_skips_later_step() {
        let (_tmp, mut ctx) = temp_verify_ctx();
        let (command, basename) = if cfg!(target_os = "windows") {
            ("ping -n 60 127.0.0.1", "ping")
        } else {
            ("sleep 60", "sleep")
        };
        ctx.config
            .verify
            .allowed_commands
            .push(basename.to_string());
        let sentinel = "sentinel-command-not-allowlisted-0408";
        let steps = vec![
            VerificationStep {
                command: command.to_string(),
                timeout_secs: 1,
                description: "sleeper".to_string(),
                shell: false,
                budget_source: None,
            },
            VerificationStep {
                command: sentinel.to_string(),
                timeout_secs: 30,
                description: "must not run".to_string(),
                shell: false,
                budget_source: None,
            },
        ];
        let started = std::time::Instant::now();
        let report = VerifyEngine::execute_with_scope(
            &mut ctx,
            None,
            &steps,
            false,
            None,
            crate::verify::plan::VerifyScope::Full,
        )
        .unwrap();
        // Hang detector only — timeout-fired proof is exit 124 +
        // duration_ms == 1000 + sentinel absent. Must stay well above
        // observed spawn+1s (~15s) and well below ping -n 60 (~59s).
        assert!(
            started.elapsed() < std::time::Duration::from_secs(45),
            "wall clock {:?}",
            started.elapsed()
        );
        assert_eq!(report.results.len(), 1, "{:?}", report.results);
        assert_eq!(report.results[0].exit_code, 124);
        assert_eq!(report.results[0].duration_ms, 1000);
        assert!(
            report.results[0]
                .stderr_summary
                .contains("Step timed out after"),
            "{}",
            report.results[0].stderr_summary
        );
        assert!(
            report.results[0].stderr_summary.contains(command),
            "{}",
            report.results[0].stderr_summary
        );
        let blob = format!("{:?}", report.results);
        assert!(!blob.contains(sentinel), "{blob}");
        assert!(!report.overall_pass);
    }

    #[test]
    fn engine_nonzero_exit_still_runs_next_step() {
        let (_tmp, mut ctx) = temp_verify_ctx();
        let steps = vec![
            VerificationStep {
                command: "git not-a-real-subcommand".to_string(),
                timeout_secs: 30,
                description: "bad git".to_string(),
                shell: false,
                budget_source: None,
            },
            VerificationStep {
                command: "git --version".to_string(),
                timeout_secs: 30,
                description: "git version".to_string(),
                shell: false,
                budget_source: None,
            },
        ];
        let report = VerifyEngine::execute_with_scope(
            &mut ctx,
            None,
            &steps,
            false,
            None,
            crate::verify::plan::VerifyScope::Full,
        )
        .unwrap();
        assert_eq!(report.results.len(), 2, "{:?}", report.results);
        assert_ne!(report.results[0].exit_code, 0);
        assert_eq!(report.results[1].exit_code, 0);
    }
}
