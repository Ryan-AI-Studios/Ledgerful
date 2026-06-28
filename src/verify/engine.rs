use crate::config::model::Config;
use crate::exec::ExecutionResult;
use crate::impact::packet::ImpactPacket;
use crate::output::human::print_verify_result;
use crate::platform::process_policy::ProcessPolicy;
use crate::state::layout::Layout;
use crate::state::storage::StorageManager;
use crate::verify::plan::{VerificationPlan, VerificationStep};
use crate::verify::results::{VerificationReport, VerificationResult, write_verify_report};
use crate::verify::runner::{execute_step, prepare_manual_step, prepare_rule_step};
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

pub struct VerifyEngine;

impl VerifyEngine {
    pub fn execute(
        ctx: &mut VerificationContext,
        plan: Option<VerificationPlan>,
        steps: &[VerificationStep],
        manual_requested: bool,
    ) -> Result<VerificationReport> {
        let mut persisted_results = Vec::new();
        let mut overall_success = true;
        let policy = ProcessPolicy::default();

        for step in steps {
            let prepared = if manual_requested {
                prepare_manual_step(step)
            } else {
                prepare_rule_step(step)
            };
            info!(
                "Running verification command via {:?}: {}",
                prepared.execution_mode, prepared.display_command
            );
            let result = execute_step(&prepared, &policy)?;
            print_verify_result(&prepared.display_command, step.timeout_secs, &result);

            let report_result = Self::to_report_result(&prepared.display_command, &result);
            if report_result.exit_code != 0 {
                overall_success = false;
            }
            persisted_results.push(report_result);
        }

        let mut report = VerificationReport::new(plan, persisted_results.clone())
            .with_warnings(ctx.warnings.clone());
        report.overall_pass = overall_success;

        write_verify_report(&ctx.layout, &report)?;
        Self::persist_verify_report(&ctx.layout, &report);

        Self::record_semantic_test_outcomes(
            &ctx.layout,
            &Some(ctx.config.clone()),
            &ctx.packet,
            &persisted_results,
        );

        Ok(report)
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
        let db_path = layout.state_subdir().join("ledger.db");
        let Ok(storage) = StorageManager::init(db_path.as_std_path()) else {
            warn!("Could not initialize SQLite for verification report persistence");
            return;
        };

        let plan_json = report
            .plan
            .as_ref()
            .and_then(|plan| serde_json::to_string(plan).ok());

        let Ok(run_id) = storage.save_verification_run(
            &report.timestamp,
            plan_json.as_deref(),
            report.overall_pass,
        ) else {
            warn!("Failed to persist verification run metadata");
            return;
        };

        for result in &report.results {
            if let Err(err) = storage.save_verification_result(
                run_id,
                &result.command,
                result.exit_code,
                result.duration_ms,
                result.truncated,
            ) {
                warn!("Failed to persist verification result: {err}");
            }
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

        let db_path = layout.state_subdir().join("ledger.db");
        let Ok(storage) = StorageManager::init(db_path.as_std_path()) else {
            warn!("Failed to open storage for semantic test outcome recording");
            return;
        };

        let diff_text = crate::verify::semantic_predictor::build_diff_text(packet);
        let diff_summary: String = diff_text.chars().take(200).collect();
        let commit_hash = packet.head_hash.clone().unwrap_or_default();

        let outcomes: Vec<crate::verify::semantic_predictor::TestOutcome> = results
            .iter()
            .map(|r| crate::verify::semantic_predictor::TestOutcome {
                test_name: r.command.clone(),
                test_file: r.command.clone(),
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
