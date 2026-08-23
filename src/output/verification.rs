use crate::output::diagnostics::print_header;
use crate::verify::engine::VerificationContext;
use crate::verify::plan::{VerificationStep, VerifyScope};
use crate::verify::results::VerificationReport;
use crate::verify::suggestions::{Suggestion, SuggestionSeverity};
use owo_colors::{OwoColorize, Stream, Style};
use std::collections::BTreeMap;

/// Default max predicted paths shown per source on dry-run (0144 scannability).
/// Overflow points operators at `--verbose` for the full list.
pub const DRY_RUN_PRED_PATH_DEFAULT: usize = 3;

/// Marker prefix inside plan step descriptions for predicted-impact segments.
const PREDICTED_IMPACT_PREFIX: &str = "Predicted impact (";

/// Delimiter between source name and path in a predicted-impact segment.
/// Must not use `find(')')` alone — `RuntimeDependency` reasons can embed `)`.
const PREDICTED_SOURCE_PATH_DELIM: &str = ") on ";

/// Parse predicted-impact `(source, path)` pairs from plan step descriptions.
///
/// Plan dedupe pipe-merges segments with ` | `, so a rule step description may
/// contain multiple `Predicted impact (…)` segments after `From rules: …`.
/// Walks every step, splits on ` | `, and extracts via the first `) on `
/// after the prefix (nested parens in the source survive).
///
/// Returns a [`BTreeMap`] (deterministic source order). Paths per source are
/// sorted and de-duplicated for stable dry-run output.
pub fn parse_predicted_impacts(steps: &[VerificationStep]) -> BTreeMap<String, Vec<String>> {
    let mut groups: BTreeMap<String, Vec<String>> = BTreeMap::new();

    for step in steps {
        for segment in step.description.split(" | ") {
            let segment = segment.trim();
            let Some(after_prefix) = segment
                .find(PREDICTED_IMPACT_PREFIX)
                .map(|i| &segment[i + PREDICTED_IMPACT_PREFIX.len()..])
            else {
                continue;
            };
            let Some(delim_at) = after_prefix.find(PREDICTED_SOURCE_PATH_DELIM) else {
                // Malformed: no ") on " delimiter — skip (do not use first `)`).
                continue;
            };
            let source = after_prefix[..delim_at].to_string();
            let path = after_prefix[delim_at + PREDICTED_SOURCE_PATH_DELIM.len()..].to_string();
            if source.is_empty() || path.is_empty() {
                continue;
            }
            groups.entry(source).or_default().push(path);
        }
    }

    for paths in groups.values_mut() {
        paths.sort_unstable();
        paths.dedup();
    }

    groups
}

/// First product line of every human dry-run (plan, refuse, `--command`).
///
/// Static phrasing: omitted `--scope` and explicit `--scope full` both print
/// the `full` line. Do not mention `CLI default` (clap `ValueSource` is not used).
pub fn dry_run_scope_line(scope: VerifyScope) -> String {
    match scope {
        VerifyScope::Fast => "scope: fast".to_string(),
        VerifyScope::Full => "scope: full (pre-push uses --scope fast)".to_string(),
    }
}

/// Format human dry-run stdout (plan-first, scannable). Pure — no side effects.
///
/// Layout (0144 B1/B2 + 0203-C):
/// 1. `scope:` line (requested/executed verify scope for this invocation)
/// 2. Optional Bayesian line when `matched` is `Some` (ordering ran)
/// 3. Verification Steps — every plan step's `command` + timeout (same set the
///    engine would execute; do not filter by description — pure predicted-impact
///    rows still carry real commands, B1 rule 2 / B5)
/// 4. Predicted Impacts (grouped by source) — omit section when empty
/// 5. Dry-run footer
pub fn format_dry_run_human(
    steps: &[VerificationStep],
    matched: Option<usize>,
    dataset_keys: Option<usize>,
    dry_verbose: bool,
    scope: VerifyScope,
) -> String {
    let mut sections: Vec<String> = Vec::new();
    sections.push(dry_run_scope_line(scope));

    if let Some(n) = matched {
        let keys = dataset_keys.unwrap_or(0);
        sections.push(format!(
            "Bayesian ordering: matched_steps={n} dataset_keys={keys}"
        ));
    }

    // Print all plan steps (engine executes all). Plan may already merge by
    // command; do not drop pure-predicted description rows — they still run.
    if !steps.is_empty() {
        let mut block = String::from("Verification Steps:\n");
        for step in steps {
            block.push_str(&format!(
                "  • {} (timeout: {}s)\n",
                step.command, step.timeout_secs
            ));
        }
        // trim trailing newline — sections join with blank lines
        if block.ends_with('\n') {
            block.pop();
        }
        sections.push(block);
    }

    let groups = parse_predicted_impacts(steps);
    if !groups.is_empty() {
        let mut block = String::from("Predicted Impacts (grouped by source):\n");
        for (source, paths) in &groups {
            block.push_str(&format!("  Source: {} — {} items\n", source, paths.len()));
            let show = if dry_verbose {
                paths.len()
            } else {
                paths.len().min(DRY_RUN_PRED_PATH_DEFAULT)
            };
            for path in paths.iter().take(show) {
                block.push_str(&format!("    • {path}\n"));
            }
            if !dry_verbose && paths.len() > DRY_RUN_PRED_PATH_DEFAULT {
                let more = paths.len() - DRY_RUN_PRED_PATH_DEFAULT;
                block.push_str(&format!(
                    "    … and {more} more (use --verbose for full list)\n"
                ));
            }
        }
        if block.ends_with('\n') {
            block.pop();
        }
        sections.push(block);
    }

    sections.push(
        "Dry run mode: verification plan displayed above. No commands were executed.".to_string(),
    );

    let mut out = sections.join("\n\n");
    out.push('\n');
    out
}

/// Print human dry-run stdout via [`format_dry_run_human`].
pub fn print_dry_run_human(
    steps: &[VerificationStep],
    matched: Option<usize>,
    dataset_keys: Option<usize>,
    dry_verbose: bool,
    scope: VerifyScope,
) {
    print!(
        "{}",
        format_dry_run_human(steps, matched, dataset_keys, dry_verbose, scope)
    );
}

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

/// Whether the aggregate "Running N verification step(s)..." progress banner may
/// be emitted at INFO on `cli_summary` (default filter → stdout).
///
/// This gate covers **only** the aggregate banner under `--verbose`. Per-step
/// product progress (`[i/n] Running:` / compact ok) is separate — see
/// [`should_emit_verify_step_progress`] — and is emitted via `println!` on the
/// human path (including default non-verbose), not via tracing INFO.
///
/// Quiet/default demotes the **banner** to `debug!`; `--verbose` restores INFO.
/// JSON / machine mode never uses INFO progress (caller also skips emission
/// entirely when `json`). Do **not** remove the aggregate banner under verbose.
pub fn should_emit_verify_progress_info(verbose: bool, json: bool) -> bool {
    verbose && !json
}

/// Whether per-step product progress lines (`[i/n] Running:` / compact ok) may
/// be printed on stdout.
///
/// True whenever human output is not suppressed (i.e. not `--json`). Independent
/// of `verbose` — default and verbose both emit step-start; compact ok is
/// default-only (see engine loop).
pub fn should_emit_verify_step_progress(suppress_human_output: bool) -> bool {
    !suppress_human_output
}

/// Format a greppable step-start product line (no leading newline).
///
/// Example: `"[1/2] Running: cargo fmt --all -- --check"`.
pub fn format_verify_step_start(i: usize, n: usize, command: &str) -> String {
    format!("[{i}/{n}] Running: {command}")
}

/// Compact elapsed for step-done lines.
///
/// - `ms >= 1000` → tenths of a second (`1.0s`, `2.2s`) via `as_secs_f64`
/// - else → whole milliseconds (`999ms`, `340ms`)
pub fn format_duration_compact(d: std::time::Duration) -> String {
    let ms = d.as_millis();
    if ms >= 1000 {
        format!("{:.1}s", d.as_secs_f64())
    } else {
        format!("{ms}ms")
    }
}

/// Format a compact default-path step-done line (no leading newline).
///
/// Example: `"[1/2] ok  cargo fmt --all -- --check  (2.2s)"` — double space
/// after `ok`, then command, then double space before elapsed.
pub fn format_verify_step_ok(i: usize, n: usize, command: &str, d: std::time::Duration) -> String {
    format!("[{i}/{n}] ok  {command}  ({})", format_duration_compact(d))
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
            Self::print_suggested_actions(&report.suggested_actions);
        }
    }

    pub fn print_prediction_warnings(warnings: &[String]) {
        for warning in warnings {
            eprintln!(
                "{} {}",
                "WARN".if_supports_color(Stream::Stderr, |s| {
                    s.style(Style::new().yellow().bold())
                }),
                warning
            );
        }
    }

    /// Print Suggested Actions to stdout. Colour is gated via
    /// `if_supports_color(Stream::Stdout, …)` (shared 0131 policy — no
    /// one-off `NO_COLOR` bool).
    pub fn print_suggested_actions(suggestions: &[Suggestion]) {
        print_header("Suggested Actions");

        for s in suggestions {
            let severity_icon = match s.severity {
                SuggestionSeverity::ActionRequired => "!!"
                    .if_supports_color(Stream::Stdout, |t| t.style(Style::new().red().bold()))
                    .to_string(),
                SuggestionSeverity::Warning => "!"
                    .if_supports_color(Stream::Stdout, |t| t.style(Style::new().yellow().bold()))
                    .to_string(),
                SuggestionSeverity::Info => "i"
                    .if_supports_color(Stream::Stdout, |t| t.cyan())
                    .to_string(),
            };

            let description = match s.severity {
                SuggestionSeverity::ActionRequired => s
                    .description
                    .if_supports_color(Stream::Stdout, |t| t.red())
                    .to_string(),
                SuggestionSeverity::Warning => s
                    .description
                    .if_supports_color(Stream::Stdout, |t| t.yellow())
                    .to_string(),
                SuggestionSeverity::Info => s
                    .description
                    .if_supports_color(Stream::Stdout, |t| t.dimmed())
                    .to_string(),
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

        println!(
            "\n{}",
            "Predicted CI Failures:".if_supports_color(Stream::Stdout, |s| s
                .style(Style::new().bold().bright_red()))
        );

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
                format!("{:.0}%", *score * 100.0)
                    .if_supports_color(Stream::Stdout, |s| s.style(Style::new().red().bold()))
                    .to_string()
            } else if *score > 0.4 {
                format!("{:.0}%", *score * 100.0)
                    .if_supports_color(Stream::Stdout, |s| s.yellow())
                    .to_string()
            } else {
                format!("{:.0}%", *score * 100.0)
                    .if_supports_color(Stream::Stdout, |s| s.green())
                    .to_string()
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
                                "Rationale for".if_supports_color(Stream::Stdout, |s| s.dimmed()),
                                job_name.if_supports_color(Stream::Stdout, |s| s.yellow()),
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
        DRY_RUN_PRED_PATH_DEFAULT, dry_run_scope_line, format_dry_run_human,
        format_duration_compact, format_verify_step_ok, format_verify_step_start,
        parse_predicted_impacts, should_emit_verify_progress_info,
        should_emit_verify_step_progress, should_print_success_step,
        should_print_suggested_actions, verify_step_result_label,
    };
    use crate::verify::plan::{VerificationStep, VerifyScope};
    use std::time::Duration;

    fn step(command: &str, description: &str) -> VerificationStep {
        VerificationStep {
            command: command.to_string(),
            timeout_secs: 60,
            description: description.to_string(),
            shell: false,
        }
    }

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

    #[test]
    fn quiet_progress_not_emitted_at_info() {
        // Aggregate banner: default/quiet path must not emit at INFO → stdout.
        // Per-step product progress is separate (`should_emit_verify_step_progress`).
        assert!(!should_emit_verify_progress_info(false, false));
        assert!(!should_emit_verify_progress_info(false, true));
        assert!(!should_emit_verify_progress_info(true, true));
        assert!(should_emit_verify_progress_info(true, false));
    }

    #[test]
    fn format_verify_step_start_exact_prefix_no_leading_newline() {
        let s = format_verify_step_start(1, 2, "cargo fmt --all -- --check");
        assert!(
            s.starts_with("[1/2] Running:"),
            "exact prefix [i/n] Running:: {s}"
        );
        assert_eq!(s, "[1/2] Running: cargo fmt --all -- --check");
        assert!(!s.starts_with('\n'), "no leading newline: {s:?}");
    }

    #[test]
    fn format_duration_compact_999ms() {
        assert_eq!(format_duration_compact(Duration::from_millis(999)), "999ms");
    }

    #[test]
    fn format_duration_compact_1000ms() {
        assert_eq!(format_duration_compact(Duration::from_millis(1000)), "1.0s");
    }

    #[test]
    fn format_duration_compact_2200ms() {
        assert_eq!(format_duration_compact(Duration::from_millis(2200)), "2.2s");
    }

    #[test]
    fn format_verify_step_ok_contains_elapsed_no_leading_newline() {
        let s = format_verify_step_ok(
            1,
            2,
            "cargo fmt --all -- --check",
            Duration::from_millis(2200),
        );
        assert_eq!(s, "[1/2] ok  cargo fmt --all -- --check  (2.2s)");
        assert!(s.contains("(2.2s)"), "contains elapsed: {s}");
        assert!(!s.starts_with('\n'), "no leading newline: {s:?}");
        // Double space after "ok" is intentional (locked format).
        assert!(s.contains("ok  cargo"), "double-space after ok: {s}");
    }

    #[test]
    fn should_emit_verify_step_progress_respects_suppress() {
        assert!(!should_emit_verify_step_progress(true));
        assert!(should_emit_verify_step_progress(false));
    }

    #[test]
    fn should_print_success_step_quiet_gate_unchanged() {
        // 0121 / 0148: SUCCESS banner remains verbose-only.
        assert!(!should_print_success_step(false));
        assert!(should_print_success_step(true));
    }

    #[test]
    fn parse_predicted_impacts_pipe_merged_both_sources() {
        // Plan dedupe merges rule + CallGraph + Temporal into one description.
        let steps = vec![step(
            "cargo clippy",
            "From rules: cargo clippy | Predicted impact (CallGraph) on a | Predicted impact (Temporal) on b",
        )];
        let groups = parse_predicted_impacts(&steps);
        assert_eq!(groups.get("CallGraph"), Some(&vec!["a".to_string()]));
        assert_eq!(groups.get("Temporal"), Some(&vec!["b".to_string()]));
        // starts_with-only filter would have lost both segments.
        assert!(!steps[0].description.starts_with("Predicted impact"));
    }

    #[test]
    fn parse_predicted_impacts_nested_parens_in_source() {
        // RuntimeDependency Display can embed ')' — must split on first ") on ".
        let steps = vec![step(
            "cargo test",
            "Predicted impact (rdma (lib)) on src/x.rs",
        )];
        let groups = parse_predicted_impacts(&steps);
        assert_eq!(
            groups.get("rdma (lib)"),
            Some(&vec!["src/x.rs".to_string()])
        );
    }

    #[test]
    fn parse_predicted_impacts_skips_malformed_without_on_delim() {
        let steps = vec![step(
            "cargo test",
            "Predicted impact (CallGraph missing path",
        )];
        let groups = parse_predicted_impacts(&steps);
        assert!(groups.is_empty());
    }

    #[test]
    fn format_dry_run_default_top_n_and_matched_steps() {
        let mut paths = Vec::new();
        for i in 1..=5 {
            paths.push(format!("src/file{i}.rs"));
        }
        // Build one pipe-merged description with 5 CallGraph paths.
        let mut desc = String::from("From rules: cargo clippy");
        for p in &paths {
            desc.push_str(&format!(" | Predicted impact (CallGraph) on {p}"));
        }
        let steps = vec![
            step("cargo clippy --all-targets", &desc),
            step("cargo nextest run", "From rules: cargo nextest run"),
        ];
        let formatted = format_dry_run_human(&steps, Some(3), Some(11), false, VerifyScope::Full);

        assert!(
            formatted.contains("Bayesian ordering: matched_steps=3 dataset_keys=11"),
            "greppable Bayesian line: {formatted}"
        );
        assert!(formatted.contains("Verification Steps:"));
        assert!(formatted.contains("cargo clippy --all-targets (timeout: 60s)"));
        assert!(formatted.contains("cargo nextest run (timeout: 60s)"));
        // Description wall must not appear as a Verification Steps line.
        assert!(
            !formatted
                .lines()
                .any(|l| l.contains("From rules:") || l.contains(" | Predicted impact")),
            "no pipe-merged description wall: {formatted}"
        );
        assert!(formatted.contains("Predicted Impacts (grouped by source):"));
        assert!(formatted.contains("Source: CallGraph — 5 items"));
        // Top-3 default
        assert_eq!(DRY_RUN_PRED_PATH_DEFAULT, 3);
        let path_lines: Vec<_> = formatted
            .lines()
            .filter(|l| l.trim_start().starts_with('•') && l.contains("src/file"))
            .collect();
        assert_eq!(path_lines.len(), 3, "default top-3 paths: {formatted}");
        assert!(
            formatted.contains("… and 2 more (use --verbose for full list)"),
            "overflow points at --verbose: {formatted}"
        );
        assert!(
            !formatted.contains("VERBOSE_DRY_RUN"),
            "overflow must not only mention env: {formatted}"
        );
        assert!(formatted.contains(
            "Dry run mode: verification plan displayed above. No commands were executed."
        ));
    }

    #[test]
    fn format_dry_run_verbose_all_paths_no_plan_banner() {
        let desc = "From rules: cargo clippy \
            | Predicted impact (CallGraph) on a.rs \
            | Predicted impact (CallGraph) on b.rs \
            | Predicted impact (CallGraph) on c.rs \
            | Predicted impact (CallGraph) on d.rs";
        let steps = vec![step("cargo clippy", desc)];
        let formatted = format_dry_run_human(&steps, Some(1), Some(2), true, VerifyScope::Full);

        // All 4 paths, one per line
        for p in ["a.rs", "b.rs", "c.rs", "d.rs"] {
            assert!(
                formatted.lines().any(|l| l.trim() == format!("• {p}")),
                "expected path {p} on own line: {formatted}"
            );
        }
        assert!(
            !formatted.contains("more (use --verbose for full list)"),
            "verbose has no overflow: {formatted}"
        );
        // Plan-banner artifacts only come from print_verify_plan — format helper
        // must never emit them (dry-run+verbose gates that printer off).
        assert!(
            !formatted.contains("Verification Plan\n")
                && !formatted.contains("Runner:")
                && !formatted
                    .lines()
                    .any(|l| l.trim_start().starts_with("Source: Auto-Policy")
                        || l.trim_start().starts_with("Source: Explicit")),
            "no plan-banner artifacts: {formatted}"
        );
        // Per-source heading uses "Source: CallGraph — N items" (product section),
        // which is fine — not the plan banner "Source: Auto-Policy".
        assert!(formatted.contains("Source: CallGraph — 4 items"));
    }

    #[test]
    fn format_dry_run_empty_predictions_omits_section() {
        let steps = vec![step(
            "cargo clippy",
            "From rules: cargo clippy --all-targets",
        )];
        let formatted = format_dry_run_human(&steps, None, None, false, VerifyScope::Full);
        assert!(!formatted.contains("Predicted Impacts"));
        assert!(!formatted.contains("Bayesian ordering:"));
        assert!(formatted.contains("Verification Steps:"));
        assert!(formatted.contains(
            "Dry run mode: verification plan displayed above. No commands were executed."
        ));
    }

    #[test]
    fn format_dry_run_matched_zero_still_prints_line() {
        // 0140 vacuous honesty: ordering ran, zero hits — still print key=value.
        let steps = vec![step("cargo test", "From rules: cargo test")];
        let formatted = format_dry_run_human(&steps, Some(0), Some(5), false, VerifyScope::Full);
        assert!(formatted.contains("Bayesian ordering: matched_steps=0 dataset_keys=5"));
    }

    #[test]
    fn format_dry_run_pure_predicted_step_still_lists_command() {
        // Plan builder creates pure Predicted impact (…) descriptions that still
        // carry real executable commands (B1 rule 2 / B5). Must appear under
        // Verification Steps AND path under Predicted Impacts.
        let steps = vec![step(
            "cargo fmt --all -- --check",
            "Predicted impact (Temporal) on docs/x.md",
        )];
        let formatted = format_dry_run_human(&steps, None, None, false, VerifyScope::Full);
        assert!(
            formatted.contains("cargo fmt --all -- --check"),
            "executable command must appear in Verification Steps: {formatted}"
        );
        assert!(
            formatted.contains("docs/x.md"),
            "path must appear under Predicted Impacts: {formatted}"
        );
        assert!(formatted.contains("Verification Steps:"));
        assert!(formatted.contains("Predicted Impacts (grouped by source):"));
        assert!(formatted.contains("Source: Temporal — 1 items"));
    }

    #[test]
    fn format_dry_run_human_first_line_scope_fast() {
        let steps = vec![step("cargo fmt --all -- --check", "Non-code changes")];
        let formatted = format_dry_run_human(&steps, None, None, false, VerifyScope::Fast);
        assert_eq!(
            formatted.lines().next(),
            Some("scope: fast"),
            "first product line must name fast scope: {formatted}"
        );
        assert!(!formatted.contains("CLI default"));
    }

    #[test]
    fn format_dry_run_human_first_line_scope_full_static() {
        let steps = vec![step("cargo nextest run --workspace", "full")];
        let formatted = format_dry_run_human(&steps, None, None, false, VerifyScope::Full);
        assert_eq!(
            formatted.lines().next(),
            Some("scope: full (pre-push uses --scope fast)"),
            "first product line must be the static full line: {formatted}"
        );
        assert!(formatted.contains("pre-push uses --scope fast"));
        assert!(!formatted.contains("CLI default"));
        assert_eq!(
            dry_run_scope_line(VerifyScope::Full),
            "scope: full (pre-push uses --scope fast)"
        );
    }
}
