use crate::output::diagnostics::print_header;
use crate::verify::engine::VerificationContext;
use crate::verify::results::VerificationReport;
use crate::verify::suggestions::{Suggestion, SuggestionSeverity};
use owo_colors::OwoColorize;

/// Whether to print Suggested Actions / full human report chatter after verify.
///
/// Quiet green (`!verbose && overall_pass`) suppresses Suggested Actions so
/// agent hooks see only the trailing "Verification passed" line (track 0121).
pub fn should_print_suggested_actions(verbose: bool, overall_pass: bool) -> bool {
    verbose || !overall_pass
}

/// Whether to print per-step SUCCESS lines during verify.
pub fn should_print_success_step(verbose: bool) -> bool {
    verbose
}

/// Label for a per-step verify result line, or `None` when quiet SUCCESS.
///
/// Pure companion to [`crate::output::human::print_verify_result`] so quiet
/// suppression can be unit-tested without capturing stdout.
pub fn verify_step_result_label(exit_code: i32, verbose: bool) -> Option<&'static str> {
    if exit_code == 0 {
        if should_print_success_step(verbose) {
            Some("SUCCESS")
        } else {
            None
        }
    } else {
        Some("FAILURE")
    }
}

pub struct VerificationReporter;

impl VerificationReporter {
    pub fn report(_ctx: &VerificationContext, report: &VerificationReport) {
        // CG-F35 (requirement #1): `ctx.warnings` (persisted onto the report
        // as `prediction_warnings`) previously only reached `latest-verify.json`
        // — nothing printed them, so a caller relying on prediction quality
        // (storage-init failures, stale-cache warnings, etc.) had no visible
        // signal. These are diagnostics about the run, not the JSON/report
        // contract itself, so they go to stderr.
        if !report.prediction_warnings.is_empty() {
            Self::print_prediction_warnings(&report.prediction_warnings);
        }

        // Suggested actions are already printed in execute_verify for now,
        // but let's move the printer here.
        if !report.suggested_actions.is_empty() {
            Self::print_suggested_actions(
                &report.suggested_actions,
                std::env::var("NO_COLOR").is_ok(),
            );
        }
    }

    pub fn print_prediction_warnings(warnings: &[String]) {
        for warning in warnings {
            eprintln!("{} {}", "WARN".yellow().bold(), warning);
        }
    }

    pub fn print_suggested_actions(suggestions: &[Suggestion], no_color: bool) {
        print_header("Suggested Actions");

        for s in suggestions {
            let severity_icon = match s.severity {
                SuggestionSeverity::ActionRequired => {
                    if no_color {
                        "!!".to_string()
                    } else {
                        "!!".red().bold().to_string()
                    }
                }
                SuggestionSeverity::Warning => {
                    if no_color {
                        "!".to_string()
                    } else {
                        "!".yellow().bold().to_string()
                    }
                }
                SuggestionSeverity::Info => {
                    if no_color {
                        "i".to_string()
                    } else {
                        "i".cyan().to_string()
                    }
                }
            };

            let description = if no_color {
                s.description.clone()
            } else {
                match s.severity {
                    SuggestionSeverity::ActionRequired => s.description.red().to_string(),
                    SuggestionSeverity::Warning => s.description.yellow().to_string(),
                    SuggestionSeverity::Info => s.description.dimmed().to_string(),
                }
            };

            println!("{} {}", severity_icon, description);
            println!("   → {}", s.command);
            println!();
        }
    }

    pub fn print_ci_predictions(
        similar_ci: &[(crate::verify::ci_predictor::CIJobOutcome, f32)],
        explain: bool,
        embed_config: &crate::config::model::LocalModelConfig,
        diff_text: &str,
    ) {
        if similar_ci.is_empty() {
            return;
        }

        println!("\n{}", "Predicted CI Failures:".bold().bright_red());

        let mut explain_config = embed_config.clone();
        explain_config.timeout_secs = 15;

        let engine = if explain {
            Some(crate::verify::explanation::ExplanationEngine::new(
                explain_config,
            ))
        } else {
            None
        };

        let mut table = crate::output::table::build_table(["Job Name", "Platform", "Probability"]);

        let failure_scores = crate::verify::ci_predictor::compute_ci_failure_scores(similar_ci);

        for (job_name, score) in &failure_scores {
            let platform = similar_ci
                .iter()
                .find(|(o, _)| &o.job_name == job_name)
                .map(|(o, _)| o.platform.clone())
                .unwrap_or_else(|| "unknown".to_string());

            let prob_color = if *score > 0.7 {
                format!("{:.0}%", *score * 100.0).red().bold().to_string()
            } else if *score > 0.4 {
                format!("{:.0}%", *score * 100.0).yellow().to_string()
            } else {
                format!("{:.0}%", *score * 100.0).green().to_string()
            };

            table.add_row(vec![job_name.clone(), platform.clone(), prob_color]);
        }
        println!("{table}");

        if let Some(engine) = engine {
            for (job_name, score) in failure_scores {
                if score > 0.4 {
                    let platform = similar_ci
                        .iter()
                        .find(|(o, _)| o.job_name == job_name)
                        .map(|(o, _)| o.platform.clone())
                        .unwrap_or_else(|| "unknown".to_string());

                    match engine.explain_ci_failure(
                        &job_name,
                        &platform,
                        &diff_text.chars().take(200).collect::<String>(),
                        similar_ci,
                    ) {
                        Ok(explanation) => {
                            println!(
                                "  {} {}: {}",
                                "Rationale for".dimmed(),
                                job_name.yellow(),
                                explanation
                            );
                        }
                        Err(e) => tracing::warn!("Failed to generate CI failure explanation: {e}"),
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        should_print_success_step, should_print_suggested_actions, verify_step_result_label,
    };

    #[test]
    fn should_print_suggested_actions_quiet_green_suppresses() {
        // Quiet green: no Suggested Actions / full human report chatter.
        assert!(!should_print_suggested_actions(false, true));
    }

    #[test]
    fn should_print_suggested_actions_verbose_green_prints() {
        assert!(should_print_suggested_actions(true, true));
    }

    #[test]
    fn should_print_suggested_actions_quiet_fail_prints() {
        // After fail block, Suggested Actions still print without --verbose.
        assert!(should_print_suggested_actions(false, false));
    }

    #[test]
    fn should_print_suggested_actions_verbose_fail_prints() {
        assert!(should_print_suggested_actions(true, false));
    }

    #[test]
    fn should_print_success_step_only_when_verbose() {
        assert!(!should_print_success_step(false));
        assert!(should_print_success_step(true));
    }

    #[test]
    fn verify_step_result_label_quiet_success_omits_success_text() {
        assert_eq!(verify_step_result_label(0, false), None);
        assert_eq!(verify_step_result_label(0, true), Some("SUCCESS"));
        assert_eq!(verify_step_result_label(1, false), Some("FAILURE"));
        assert_eq!(verify_step_result_label(1, true), Some("FAILURE"));
    }
}
