use crate::exec::ExecutionResult;
use crate::verify::plan::VerificationPlan;
use owo_colors::{OwoColorize, Stream, Style};

pub fn print_verify_plan(plan: &VerificationPlan) {
    // Detect whether nextest is used from the first step's command
    let runner = plan
        .steps
        .first()
        .map(|s| {
            if s.command.contains("nextest") {
                "nextest"
            } else {
                "cargo test"
            }
        })
        .unwrap_or("cargo test");
    println!(
        "\n{}",
        "Verification Plan"
            .if_supports_color(Stream::Stdout, |s| s.style(Style::new().bold().underline()))
    );
    if let Some(source) = &plan.source {
        let source_str = match source {
            crate::verify::plan::PlanSource::AutoPolicy => "Auto-Policy",
            crate::verify::plan::PlanSource::ExplicitConfig => "Explicit Config",
            crate::verify::plan::PlanSource::HistoricalRulesFallback => {
                "Historical Rules (Auto-Policy Fallback)"
            }
            crate::verify::plan::PlanSource::Manual => "Manual",
        };
        println!(
            "  {} {}",
            "Source:".if_supports_color(Stream::Stdout, |s| s.dimmed()),
            source_str
        );
    }
    println!(
        "  {} {}",
        "Runner:".if_supports_color(Stream::Stdout, |s| s.dimmed()),
        runner
    );
    for step in &plan.steps {
        let desc = if step.description.is_empty() {
            &step.command
        } else {
            &step.description
        };
        println!(
            "  {} {}",
            "•".if_supports_color(Stream::Stdout, |s| s.dimmed()),
            desc
        );
    }
}

/// Print per-step verify outcome.
///
/// SUCCESS lines only when `verbose` (quiet success by default). FAILURE always
/// when called (caller already gates machine mode via `suppress_human_output`).
pub fn print_verify_result(name: &str, _timeout: u64, result: &ExecutionResult, verbose: bool) {
    if result.exit_code == 0 {
        if crate::output::verification::should_print_success_step(verbose) {
            println!(
                "\n{} Verification passed for: {}",
                "SUCCESS"
                    .if_supports_color(Stream::Stdout, |s| s.style(Style::new().green().bold())),
                name
            );
        }
    } else {
        println!(
            "\n{} Verification failed for: {}",
            "FAILURE".if_supports_color(Stream::Stdout, |s| s.style(Style::new().red().bold())),
            name
        );
    }
}
