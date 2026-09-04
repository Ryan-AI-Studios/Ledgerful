use crate::platform::env::ExecutableStatus;
use owo_colors::{OwoColorize, Stream, Style};
use std::io::{self, Write};

pub struct DoctorReport<'a> {
    pub platform: &'a str,
    pub shell: &'a str,
    pub tools: &'a Vec<(String, ExecutableStatus)>,
    pub path_display: &'a str,
    pub path_kind: &'a str,
    /// Current worktree workdir (analysis root).
    pub work_root: &'a str,
    /// Resolved `.ledgerful` state home (may be on the main worktree for linked trees).
    pub state_dir: &'a str,
    pub is_wsl_mounted: bool,
    pub embedding_model_status: String,
    /// From `BackendAvailabilityReport::is_failure` — preferred over string
    /// matching when counting doctor failures for the embedding backend.
    pub embedding_model_failed: bool,
    pub completion_model_status: String,
    pub native_graph_status: String,
    pub active_ask_backend: String,
    pub index_health: Vec<String>,
    pub target_triple: &'a str,
}

/// Pure summary text for doctor aggregate-first header (0109 / 0209).
///
/// Priority: block → action-critical warn → optional warn → info → all-pass.
/// Header “warning(s)” is `warn_action` (what Index Health expands). Optional
/// clause is always ` · {n} optional` (never `optional warning(s)` here).
/// Red “issue(s)” wording is reserved for **block** only.
pub fn format_doctor_summary_text(
    block: u64,
    warn_action: u64,
    warn_optional: u64,
    info: u64,
) -> String {
    if block > 0 {
        format!("✗ Doctor: {block} block issue(s)")
    } else if warn_action > 0 && warn_optional > 0 {
        format!(
            "✓ Doctor: ready for publish env · {warn_action} warning(s) · {warn_optional} optional"
        )
    } else if warn_action > 0 {
        format!("✓ Doctor: ready for publish env · {warn_action} warning(s)")
    } else if warn_optional > 0 {
        format!("✓ Doctor: ready for publish env · {warn_optional} optional")
    } else if info > 0 {
        format!("✓ Doctor: ready for publish env · {info} hint(s)")
    } else {
        "✓ Doctor: all checks passed".to_string()
    }
}

/// Counters for the doctor aggregate-first header (0109 severity model).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DoctorSummaryCounts {
    pub block: u64,
    pub warn: u64,
    pub info: u64,
}

/// Human doctor progressive-disclosure profile (0174 3-tier).
///
/// - Default: expand Block + ActionWarn; collapse Hygiene (Optional or Info).
/// - `full`: expand hygiene too (info non-optional under Index Health; optional
///   under Optional Accelerators).
/// - `quiet`: suppress multi-line remediations (VRAM suppressed by caller).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DoctorHumanProfile {
    pub full: bool,
    pub quiet: bool,
}

/// Greppable trailer when hygiene findings are collapsed (default human).
///
/// When `warn_optional == 0`, byte-stable with the 0174 string. Optional
/// warns add `(1 optional warning)` / `(N optional warnings)` (0209-C).
pub fn format_hygiene_collapse_trailer(hygiene_count: usize, warn_optional: u64) -> String {
    if warn_optional == 0 {
        format!("{hygiene_count} hygiene finding(s) collapsed — run doctor --full")
    } else {
        let warning_word = if warn_optional == 1 {
            "warning"
        } else {
            "warnings"
        };
        format!(
            "{hygiene_count} hygiene finding(s) collapsed ({warn_optional} optional {warning_word}) — run doctor --full"
        )
    }
}

/// Greppable trailer when observe-mode signing hygiene is deferred (0225).
///
/// Separate from [`format_hygiene_collapse_trailer`] — never fold `later`
/// into `hygiene_count`.
pub fn format_signing_deferred_trailer(later_count: usize) -> String {
    format!("{later_count} signing finding(s) deferred (observe) — run doctor --full")
}

/// Tools-table label + uncoloured status text (0209-B).
///
/// `gemini` / `gemini-cli` are the optional PATH CLI, not Cloud Ask.
/// Printer applies NotFound colour; this helper returns plain strings.
pub fn format_doctor_tool_line(name: &str, status: &ExecutableStatus) -> (String, String) {
    let is_gemini_cli = name == "gemini" || name == "gemini-cli";
    let label = if is_gemini_cli {
        "gemini CLI".to_string()
    } else {
        name.to_string()
    };
    let status_text = match status {
        ExecutableStatus::Found(p) => format!("Found ({})", p.display()),
        ExecutableStatus::NotFound if is_gemini_cli => {
            "NOT FOUND (optional CLI; not the Cloud Ask backend)".to_string()
        }
        ExecutableStatus::NotFound => "NOT FOUND".to_string(),
    };
    (label, status_text)
}

/// Human `WSL Support:` line when the workdir is a WSL-mounted Windows drive.
pub fn wsl_support_line(is_wsl_mounted: bool) -> Option<&'static str> {
    if is_wsl_mounted {
        Some("WSL Support:         Active (Mounted)")
    } else {
        None
    }
}

/// Partition findings for human 3-tier display (0174).
///
/// Returns `(index_health_findings, optional_accelerator_findings, hygiene_count)`:
/// - Index Health expand: always action-critical; plus non-optional Info when `full`.
/// - Optional Accelerators findings: Optional-category only when `full`.
/// - `hygiene_count`: total hygiene findings (for collapse trailer when `!full`).
pub fn partition_doctor_findings_for_human(
    findings: &[crate::commands::doctor::DoctorFinding],
    full: bool,
) -> (
    Vec<&crate::commands::doctor::DoctorFinding>,
    Vec<&crate::commands::doctor::DoctorFinding>,
    usize,
) {
    use crate::commands::doctor::{DoctorCategory, DoctorSeverity, is_action_critical, is_hygiene};

    let hygiene_count = findings.iter().filter(|f| is_hygiene(f)).count();

    let index_health: Vec<_> = findings
        .iter()
        .filter(|f| {
            if (f.session_priority.is_later() || f.acknowledged) && !full {
                false
            } else if is_action_critical(f) {
                true
            } else if full {
                // Non-optional info expands under Index Health when --full.
                f.severity == DoctorSeverity::Info && f.category != DoctorCategory::Optional
            } else {
                false
            }
        })
        .collect();

    // Optional Accelerators under --full: hygiene optional only. Optional-category
    // blocks (if any) are already action-critical and listed under Index Health —
    // do not double-print (0174 review P3).
    let optional_findings: Vec<_> = if full {
        findings
            .iter()
            .filter(|f| f.category == DoctorCategory::Optional && is_hygiene(f))
            .collect()
    } else {
        Vec::new()
    };

    (index_health, optional_findings, hygiene_count)
}

/// Human doctor report from structured findings (0109 + 0174 3-tier).
///
/// `index_health` holds non-finding status lines (e.g. Search index OK).
/// Severity-classified issues are in `findings` and printed with prefixes.
/// Progressive disclosure: Block + ActionWarn always expanded; Hygiene
/// (Optional or Info) collapsed unless `profile.full`.
pub fn print_doctor_report(
    report: &DoctorReport,
    summary: &DoctorSummaryCounts,
    findings: &[crate::commands::doctor::DoctorFinding],
    profile: DoctorHumanProfile,
) {
    let mut out = io::stdout();
    let _ = super::print_doctor_report_to(&mut out, report, summary, findings, profile);
}

/// Write the human doctor report to `out` (0225 tests capture this path).
pub(crate) fn print_doctor_report_to(
    out: &mut dyn Write,
    report: &DoctorReport,
    summary: &DoctorSummaryCounts,
    findings: &[crate::commands::doctor::DoctorFinding],
    profile: DoctorHumanProfile,
) -> io::Result<()> {
    let split = crate::commands::doctor::split_doctor_warns(findings);
    debug_assert_eq!(split.total, summary.warn);
    let summary_text =
        format_doctor_summary_text(summary.block, split.action, split.optional, summary.info);
    if summary.block > 0 {
        writeln!(
            out,
            "{}",
            summary_text.if_supports_color(Stream::Stdout, |s| s.style(Style::new().red().bold()))
        )?;
    } else if summary.warn > 0 {
        writeln!(
            out,
            "{}",
            summary_text
                .if_supports_color(Stream::Stdout, |s| s.style(Style::new().yellow().bold()))
        )?;
    } else {
        writeln!(
            out,
            "{}",
            summary_text
                .if_supports_color(Stream::Stdout, |s| s.style(Style::new().green().bold()))
        )?;
    }

    writeln!(out, "\nLedgerful Doctor - Environment Health Check")?;
    writeln!(out, "==================================================")?;
    writeln!(out, "{:<20} {}", "Environment:", report.platform)?;
    writeln!(out, "{:<20} {}", "Active Shell:", report.shell)?;

    let family = if cfg!(windows) { "windows" } else { "unix" };
    let telemetry = format!(
        "os={}, arch={}, family={}, target_triple={}",
        std::env::consts::OS,
        std::env::consts::ARCH,
        family,
        report.target_triple
    );
    writeln!(out, "{:<20} {}", "LEDGERFUL_PLATFORM:", telemetry)?;

    writeln!(out, "\nTools:")?;
    for (name, status) in report.tools {
        let (label, status_text) = format_doctor_tool_line(name, status);
        let status_str = if matches!(status, ExecutableStatus::NotFound) {
            status_text
                .if_supports_color(Stream::Stdout, |s| s.red())
                .to_string()
        } else {
            status_text
        };
        writeln!(out, "  {:<18} {}", label, status_str)?;
    }

    writeln!(out, "\nCurrent Path:        {}", report.path_display)?;
    writeln!(out, "Path Type:           {}", report.path_kind)?;
    writeln!(out, "Work root:           {}", report.work_root)?;
    writeln!(out, "State dir:           {}", report.state_dir)?;
    if let Some(line) = wsl_support_line(report.is_wsl_mounted) {
        writeln!(out, "{line}")?;
    }

    writeln!(out, "\nActive Ask Backend:  {}", report.active_ask_backend)?;
    writeln!(out, "Native Graph:        {}", report.native_graph_status)?;

    let (index_findings, optional_findings, hygiene_count) =
        partition_doctor_findings_for_human(findings, profile.full);

    if !report.index_health.is_empty() || !index_findings.is_empty() {
        writeln!(out, "\nIndex Health:")?;
        for health in &report.index_health {
            writeln!(out, "  • {}", health)?;
        }
        for f in &index_findings {
            print_doctor_finding_line(out, f, "  • ", profile.quiet)?;
        }
    }

    writeln!(out, "\n── Optional Accelerators ──────────────────────")?;
    writeln!(
        out,
        "Embedding Model:     {}",
        report.embedding_model_status
    )?;
    writeln!(
        out,
        "Completion Model:    {}",
        report.completion_model_status
    )?;
    for f in &optional_findings {
        print_doctor_finding_line(out, f, "", profile.quiet)?;
    }

    if !profile.full && hygiene_count > 0 {
        writeln!(
            out,
            "\n{}",
            format_hygiene_collapse_trailer(hygiene_count, split.optional)
        )?;
    }
    if !profile.full {
        let later_count = findings
            .iter()
            .filter(|f| f.session_priority.is_later() && !f.acknowledged)
            .count();
        if later_count > 0 {
            writeln!(out, "\n{}", format_signing_deferred_trailer(later_count))?;
        }
    }
    Ok(())
}

/// Whether multi-line remediations print under the finding (suppressed when quiet).
pub(crate) fn doctor_should_print_remediation(quiet: bool) -> bool {
    !quiet
}

/// Print one finding line with optional remediation (suppressed when quiet).
fn print_doctor_finding_line(
    out: &mut dyn Write,
    f: &crate::commands::doctor::DoctorFinding,
    bullet: &str,
    quiet: bool,
) -> io::Result<()> {
    let prefix = match f.severity {
        crate::commands::doctor::DoctorSeverity::Block => "[block]"
            .if_supports_color(Stream::Stdout, |s| s.red())
            .to_string(),
        crate::commands::doctor::DoctorSeverity::Warn => "[warn]"
            .if_supports_color(Stream::Stdout, |s| s.yellow())
            .to_string(),
        crate::commands::doctor::DoctorSeverity::Info => "[info]"
            .if_supports_color(Stream::Stdout, |s| s.cyan())
            .to_string(),
    };
    writeln!(out, "{bullet}{prefix} [{}] {}", f.code, f.message)?;
    if super::doctor_should_print_remediation(quiet) {
        let rem_indent = if bullet.is_empty() { "  " } else { "    " };
        print_doctor_remediation(
            out,
            f.remediation.as_deref(),
            f.message.as_str(),
            rem_indent,
        )?;
    }
    Ok(())
}

/// Print structured remediation under a finding once (skip if identical to message).
fn print_doctor_remediation(
    out: &mut dyn Write,
    remediation: Option<&str>,
    message: &str,
    indent: &str,
) -> io::Result<()> {
    let Some(rem) = remediation else {
        return Ok(());
    };
    let rem = rem.trim();
    if rem.is_empty() || rem == message.trim() {
        return Ok(());
    }
    for line in rem.lines() {
        writeln!(out, "{indent}{line}")?;
    }
    Ok(())
}
