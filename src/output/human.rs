use crate::exec::ExecutionResult;
use crate::impact::packet::{
    BlastRadius, DeadCodeFinding, Hotspot, ImpactPacket, RiskLevel, TemporalCoupling,
};
use crate::observability::signal::{ObservabilitySignal, SignalSeverity};
use crate::output::table::{apply_table_style, resolve_table_style};
use crate::platform::env::ExecutableStatus;
use crate::verify::plan::VerificationPlan;
use comfy_table::{Cell, Color, Table};
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
    let _ = print_doctor_report_to(&mut out, report, summary, findings, profile);
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
    if doctor_should_print_remediation(quiet) {
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

/// Honest-ceiling footer for dead-code human output (0100 Option 1 / DoD-4).
pub const DEAD_CODE_HONESTY_FOOTER: &str = "Heuristic evidence — not proof of dead code. Factors include reachability, git activity, and test coverage.";

/// Empty-state copy when no findings pass the confidence threshold.
pub const DEAD_CODE_EMPTY_STATE: &str = "No findings above threshold (heuristic analysis).";

pub fn print_scan_summary(snapshot: &crate::git::RepoSnapshot) {
    println!(
        "\n{}",
        "Ledgerful Git Scan Summary"
            .if_supports_color(Stream::Stdout, |s| s.style(Style::new().bold().underline()))
    );
    println!(
        "{:<15} {}",
        "Branch:".if_supports_color(Stream::Stdout, |s| s.bold()),
        snapshot.branch_name.as_deref().unwrap_or("unknown")
    );
    println!(
        "{:<15} {}",
        "HEAD:".if_supports_color(Stream::Stdout, |s| s.bold()),
        snapshot.head_hash.as_deref().unwrap_or("unknown")
    );

    let state_str = if snapshot.is_clean {
        "CLEAN"
            .if_supports_color(Stream::Stdout, |s| s.green())
            .to_string()
    } else {
        "DIRTY"
            .if_supports_color(Stream::Stdout, |s| s.yellow())
            .to_string()
    };
    println!(
        "{:<15} {}",
        "State:".if_supports_color(Stream::Stdout, |s| s.bold()),
        state_str
    );

    if !snapshot.changes.is_empty() {
        // Prefer shared state (linked worktrees). Non-git → Layout::new(cwd) for
        // ignore-pattern config only — no DB open. Resolve-after-discover fails closed.
        let layout = match crate::commands::helpers::get_layout_or_cwd_if_not_git() {
            Ok(l) => l,
            Err(_) => {
                // Rare: bad UTF-8 cwd. Use empty defaults without inventing state.
                let root = camino::Utf8PathBuf::from(".");
                crate::state::layout::Layout::new(root)
            }
        };
        let config = crate::config::load::load_config(&layout).unwrap_or_default();
        let ignore_set = if !config.watch.ignore_patterns.is_empty() {
            let mut builder = globset::GlobSetBuilder::new();
            for pattern in &config.watch.ignore_patterns {
                if let Ok(glob) = globset::Glob::new(pattern) {
                    builder.add(glob);
                }
            }
            builder.build().ok()
        } else {
            None
        };

        let mut table = Table::new();
        apply_table_style(&mut table, resolve_table_style());
        table.set_header(vec!["State", "Action", "File Path"]);

        for change in &snapshot.changes {
            let state = if change.is_staged {
                "Staged"
                    .if_supports_color(Stream::Stdout, |s| s.green())
                    .to_string()
            } else {
                "Unstaged"
                    .if_supports_color(Stream::Stdout, |s| s.dimmed())
                    .to_string()
            };
            let action = match &change.change_type {
                crate::git::ChangeType::Added => "Added"
                    .if_supports_color(Stream::Stdout, |s| s.green())
                    .to_string(),
                crate::git::ChangeType::Modified => "Modified"
                    .if_supports_color(Stream::Stdout, |s| s.yellow())
                    .to_string(),
                crate::git::ChangeType::Deleted => "Deleted"
                    .if_supports_color(Stream::Stdout, |s| s.red())
                    .to_string(),
                crate::git::ChangeType::Renamed { old_path } => {
                    format!("Renamed ({})", old_path.display())
                        .if_supports_color(Stream::Stdout, |s| s.blue())
                        .to_string()
                }
            };

            let is_ignored = if let Some(ref set) = ignore_set {
                let path_str = change.path.to_string_lossy().replace('\\', "/");
                set.is_match(path_str)
            } else {
                false
            };

            let path_display = if is_ignored {
                format!("{} (ignored)", change.path.display())
                    .if_supports_color(Stream::Stdout, |s| s.dimmed())
                    .to_string()
            } else {
                change.path.display().to_string()
            };

            table.add_row(vec![
                Cell::new(state),
                Cell::new(action),
                Cell::new(path_display),
            ]);
        }
        println!("{table}");
    }
}

pub fn print_impact_summary(packet: &ImpactPacket) {
    println!(
        "\n{}",
        "Change Impact Analysis"
            .if_supports_color(Stream::Stdout, |s| s.style(Style::new().bold().underline()))
    );

    let risk_color = match packet.risk_level {
        RiskLevel::High => Color::Red,
        RiskLevel::Medium => Color::Yellow,
        RiskLevel::Low => Color::Green,
    };

    let mut risk_table = Table::new();
    apply_table_style(&mut risk_table, resolve_table_style());
    risk_table.add_row(vec![
        Cell::new("OVERALL RISK"),
        Cell::new(format!("{:?}", packet.risk_level).to_uppercase()).fg(risk_color),
    ]);
    println!("{risk_table}");

    // 0173 honesty: demoted count + modes before deep temporal table.
    println!(
        "  pathMode={}  demotedTemporal={}  analysisMode={}",
        packet.path_mode, packet.demoted_temporal_count, packet.analysis_mode
    );
    if packet.demoted_temporal_count > 0 {
        println!(
            "  {} process/governance temporal coupling(s) demoted from risk (full list below; --include-governance restores weight)",
            packet.demoted_temporal_count
        );
    }

    if !packet.hotspots.is_empty() {
        print_hotspots(&packet.hotspots);
    }

    if let Some(ref blast) = packet.blast_radius
        && !blast.is_empty_for_serde()
    {
        print_structural_blast(blast);
    }

    if !packet.temporal_couplings.is_empty() {
        print_temporal_couplings(&packet.temporal_couplings);
    }

    if !packet.observability.is_empty() {
        print_observability_signals(&packet.observability);
    }

    if let Some(ref gaps) = packet.test_gaps {
        println!(
            "\nTest gaps: {} (mapped={}, fileMapped={}, unmapped={})",
            gaps.status.as_str(),
            gaps.mapped_count,
            gaps.file_mapped_count,
            gaps.unmapped_count
        );
        if gaps.unmapped_count > 0 {
            eprintln!(
                "warning: {} changed source symbol(s) lack structural test mapping",
                gaps.unmapped_count
            );
        }
    }

    if let Some(ref flows) = packet.affected_flows {
        use crate::impact::enrichment::affected_flows::AffectedFlowsStatus;
        println!(
            "\n{}",
            "Affected HTTP flows (registered routes — not CRG call-chain traces)"
                .if_supports_color(Stream::Stdout, |s| s.style(Style::new().bold().underline()))
        );
        println!(
            "  Status: {}  flowCount={}{}",
            flows.status.as_str(),
            flows.flow_count,
            if flows.flow_capped {
                format!(" (capped; total={})", flows.flow_total)
            } else {
                String::new()
            }
        );
        if flows.status == AffectedFlowsStatus::Available && flows.flow_count == 0 {
            println!("  No registered HTTP flows touched by this change set");
        } else {
            for flow in flows.flows.iter().take(8) {
                let handler = flow.handler_symbol_name.as_deref().unwrap_or("-");
                println!(
                    "  - {} {}  [{}]  ({})",
                    flow.method, flow.path_pattern, flow.framework, handler
                );
            }
            if flows.flows.len() > 8 {
                println!(
                    "  … and {} more (full list: impact --json)",
                    flows.flows.len() - 8
                );
            }
        }
    }
}

pub fn print_impact_brief(packet: &ImpactPacket) {
    let risk = format!("{:?}", packet.risk_level).to_uppercase();
    match packet.risk_level {
        RiskLevel::High => println!(
            "Impact Analysis: Risk is {}",
            risk.if_supports_color(Stream::Stdout, |s| s.style(Style::new().red().bold()))
        ),
        RiskLevel::Medium => println!(
            "Impact Analysis: Risk is {}",
            risk.if_supports_color(Stream::Stdout, |s| s.style(Style::new().yellow().bold()))
        ),
        RiskLevel::Low => println!(
            "Impact Analysis: Risk is {}",
            risk.if_supports_color(Stream::Stdout, |s| s.style(Style::new().green().bold()))
        ),
    }
    // 0173 honesty header: demoted temporal + modes before deep tables.
    if packet.demoted_temporal_count > 0 || packet.path_mode != "code" {
        println!(
            "  pathMode={}  demotedTemporal={}  analysisMode={}",
            packet.path_mode, packet.demoted_temporal_count, packet.analysis_mode
        );
    } else if packet.analysis_mode != "working_tree" {
        println!("  analysisMode={}", packet.analysis_mode);
    }
    if packet.demoted_temporal_count > 0 {
        println!(
            "  {} process/governance temporal coupling(s) demoted from risk (use --include-governance to restore)",
            packet.demoted_temporal_count
        );
    }
    if let Some(ref blast) = packet.blast_radius
        && !blast.edges.is_empty()
    {
        let s = &blast.confidence_summary;
        let mut tier = format!("scipBound={} resolved={}", s.scip_bound, s.resolved);
        if s.ambiguous > 0 {
            tier.push_str(&format!(" ambiguous={}", s.ambiguous));
        }
        println!(
            "  Structural blast: {} edge(s), {} must-touch file(s) (depth {}; {tier})",
            blast.edges.len(),
            blast.must_touch_files.len(),
            blast.depth_applied
        );
    }
    if let Some(ref gaps) = packet.test_gaps
        && gaps.unmapped_count > 0
    {
        eprintln!(
            "warning: {} changed source symbol(s) lack structural test mapping",
            gaps.unmapped_count
        );
    }
    if let Some(ref flows) = packet.affected_flows {
        use crate::impact::enrichment::affected_flows::AffectedFlowsStatus;
        if flows.status == AffectedFlowsStatus::Available && flows.flow_count > 0 {
            println!("  flows={}", flows.flow_count);
        }
    }
}

/// Structural call-graph blast (≠ deploy high-blast resources).
fn print_structural_blast(blast: &BlastRadius) {
    println!(
        "\n{}",
        "Structural blast radius (call graph — not deploy high-blast resources)"
            .if_supports_color(Stream::Stdout, |s| s.style(Style::new().bold().underline()))
    );
    println!(
        "  Depth applied: {} (requested {})",
        blast.depth_applied, blast.depth_requested
    );

    let s = &blast.confidence_summary;
    if s.total > 0 || !blast.edges.is_empty() {
        println!(
            "  Confidence: scipBound={} resolved={} ambiguous={} unresolved={} capped={} unknown={} expandable={} total={}",
            s.scip_bound,
            s.resolved,
            s.ambiguous,
            s.unresolved,
            s.capped,
            s.unknown,
            s.expandable,
            if s.total > 0 {
                s.total
            } else {
                blast.edges.len()
            }
        );
    }

    if !blast.must_touch_files.is_empty() {
        println!("  Must-touch files:");
        for path in blast.must_touch_files.iter().take(20) {
            println!("    - {path}");
        }
        if blast.must_touch_files.len() > 20 {
            println!(
                "    … and {} more (full list: impact --json)",
                blast.must_touch_files.len() - 20
            );
        }
    }

    if !blast.edges.is_empty() {
        println!("  Top callers (hop · status · evidence):");
        for edge in blast.edges.iter().take(15) {
            let evidence = if edge.evidence.is_empty() {
                "-"
            } else {
                edge.evidence.as_str()
            };
            println!(
                "    - hop{} {}@{} → {}  [{} {}]",
                edge.hop,
                edge.from_symbol,
                edge.from_file,
                edge.to_symbol,
                edge.resolution_status,
                evidence
            );
        }
        if blast.edges.len() > 15 {
            println!(
                "    … and {} more edges (full edges: impact --json)",
                blast.edges.len() - 15
            );
        }
    }

    if !blast.honesty_notes.is_empty() {
        println!("  Notes:");
        for note in &blast.honesty_notes {
            println!("    - {note}");
        }
    }
}

pub fn print_hotspots(hotspots: &[Hotspot]) {
    println!(
        "\n{}",
        "Codebase Hotspots (Risk Density)".if_supports_color(Stream::Stdout, |s| s.bold())
    );
    let mut table = Table::new();
    apply_table_style(&mut table, resolve_table_style());
    table.set_header(vec!["Rank", "Score", "Freq", "Comp", "File Path"]);

    for (i, h) in hotspots.iter().enumerate() {
        table.add_row(vec![
            Cell::new((i + 1).to_string()),
            Cell::new(format!("{:.3}", h.display_score)),
            Cell::new(format!("{:.1}", h.frequency)),
            Cell::new(h.complexity.to_string()),
            Cell::new(h.path.display().to_string()),
        ]);
    }
    println!("{table}");
}

pub fn print_hotspots_table(hotspots: &[Hotspot]) {
    print_hotspots(hotspots);
}

pub fn print_hotspots_table_with_centrality(hotspots: &[Hotspot]) {
    println!(
        "\n{}",
        "Codebase Hotspots (with Centrality)".if_supports_color(Stream::Stdout, |s| s.bold())
    );
    let mut table = Table::new();
    apply_table_style(&mut table, resolve_table_style());
    table.set_header(vec!["Rank", "Score", "Freq", "Comp", "Cent", "File Path"]);

    for (i, h) in hotspots.iter().enumerate() {
        let cent = h
            .centrality
            .map(|c| c.to_string())
            .unwrap_or_else(|| "-".to_string());
        table.add_row(vec![
            Cell::new((i + 1).to_string()),
            Cell::new(format!("{:.3}", h.display_score)),
            Cell::new(format!("{:.1}", h.frequency)),
            Cell::new(h.complexity.to_string()),
            Cell::new(cent),
            Cell::new(h.path.display().to_string()),
        ]);
    }
    println!("{table}");
}

pub fn print_semantic_hotspots(matches: &[crate::semantic::hotspots::SemanticMatch]) {
    println!(
        "\n{}",
        "Semantic Hotspots (Duplicate Density)".if_supports_color(Stream::Stdout, |s| s.bold())
    );
    let mut table = Table::new();
    apply_table_style(&mut table, resolve_table_style());
    table.set_header(vec!["Rank", "Similarity", "File 1", "File 2"]);

    for (i, m) in matches.iter().enumerate() {
        table.add_row(vec![
            Cell::new((i + 1).to_string()),
            Cell::new(format!("{:.3}", m.similarity)),
            Cell::new(format!("{}:{}", m.file1, m.name1)),
            Cell::new(format!("{}:{}", m.file2, m.name2)),
        ]);
    }
    println!("{table}");
}

fn print_temporal_couplings(couplings: &[TemporalCoupling]) {
    println!(
        "\n{}",
        "Temporal Couplings (>70% co-change)".if_supports_color(Stream::Stdout, |s| s.bold())
    );
    let mut table = Table::new();
    apply_table_style(&mut table, resolve_table_style());
    table.set_header(vec!["Strength", "File A", "File B"]);

    for c in couplings {
        table.add_row(vec![
            Cell::new(format!("{:.0}%", c.score * 100.0)),
            Cell::new(c.file_a.display().to_string()),
            Cell::new(c.file_b.display().to_string()),
        ]);
    }
    println!("{table}");
}

fn print_observability_signals(signals: &[ObservabilitySignal]) {
    println!(
        "\n{}",
        "Observability Signals".if_supports_color(Stream::Stdout, |s| s.bold())
    );
    let mut table = Table::new();
    apply_table_style(&mut table, resolve_table_style());
    table.set_header(vec!["Source", "Severity", "Signal"]);

    for signal in signals {
        let sev = match signal.severity {
            SignalSeverity::Critical => "CRITICAL"
                .if_supports_color(Stream::Stdout, |s| s.red())
                .to_string(),
            SignalSeverity::Warning => "WARN"
                .if_supports_color(Stream::Stdout, |s| s.yellow())
                .to_string(),
            SignalSeverity::Normal => "NORMAL"
                .if_supports_color(Stream::Stdout, |s| s.blue())
                .to_string(),
        };
        table.add_row(vec![
            Cell::new(signal.source.clone()),
            Cell::new(sev),
            Cell::new(signal.signal_label.clone()),
        ]);
    }
    println!("{table}");
}

pub fn print_dead_code_summary(
    findings: &[DeadCodeFinding],
    _threshold: f64,
    include_traits: bool,
) {
    println!(
        "\n{}",
        "Dead Code Analysis".if_supports_color(Stream::Stdout, |s| s.bold())
    );
    if findings.is_empty() {
        println!("  {DEAD_CODE_EMPTY_STATE}");
    } else {
        let mut table = Table::new();
        apply_table_style(&mut table, resolve_table_style());
        table.set_header(vec!["Symbol", "File", "Confidence", "Factors"]);

        for f in findings {
            let factors_str = f
                .factors
                .iter()
                .map(|fac| format!("{:?}", fac))
                .collect::<Vec<_>>()
                .join(", ");

            table.add_row(vec![
                Cell::new(f.symbol_name.clone()),
                Cell::new(f.file_path.display().to_string()),
                Cell::new(format!("{:.0}%", f.confidence * 100.0)),
                Cell::new(factors_str),
            ]);
        }
        println!("{table}");
    }
    // 0100 Option 1: honest-ceiling footer (title kept; not proof of dead code).
    println!("  {DEAD_CODE_HONESTY_FOOTER}");

    // DX4: the broad `HINT: Derived traits ...` warning was removed because
    // derive-based and standard-trait false positives are now suppressed
    // structurally (derive penalty in `dead_code::filters::derive_penalty`
    // and the `is_standard_trait` filter from CG-F6). The `--include-traits`
    // flag's own help text in `args.rs` remains as user documentation.
    let _ = include_traits;
}

pub fn print_dead_code_grouped(findings: &[DeadCodeFinding]) {
    use std::collections::BTreeMap;

    println!(
        "\n{}",
        "Dead Code Analysis (grouped by file)".if_supports_color(Stream::Stdout, |s| s.bold())
    );

    if findings.is_empty() {
        println!("  {DEAD_CODE_EMPTY_STATE}");
        println!("  {DEAD_CODE_HONESTY_FOOTER}");
        return;
    }

    // Group by file path, computing avg confidence, symbol count, top factor.
    let mut groups: BTreeMap<String, Vec<&DeadCodeFinding>> = BTreeMap::new();
    for f in findings {
        let path = f.file_path.display().to_string();
        groups.entry(path).or_default().push(f);
    }

    // Build rows: (file, symbols, avg_confidence, top_factor)
    let mut rows: Vec<(String, usize, f64, String)> = groups
        .iter()
        .map(|(file, finds)| {
            let count = finds.len();
            let avg: f64 = finds.iter().map(|f| f.confidence).sum::<f64>() / count as f64;
            // Top factor = most common factor across symbols in this file.
            // Use BTreeMap for deterministic iteration order on ties.
            let mut factor_counts: std::collections::BTreeMap<
                &crate::impact::packet::ConfidenceFactor,
                usize,
            > = std::collections::BTreeMap::new();
            for f in finds.iter() {
                for fac in &f.factors {
                    *factor_counts.entry(fac).or_default() += 1;
                }
            }
            let top_factor = factor_counts
                .into_iter()
                .max_by_key(|(_, c)| *c)
                .map(|(fac, _)| format!("{:?}", fac))
                .unwrap_or_else(|| "Unknown".to_string());
            (file.clone(), count, avg, top_factor)
        })
        .collect();

    // Deterministic sort: avg confidence desc, then file path asc
    rows.sort_by(|a, b| {
        b.2.partial_cmp(&a.2)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(&b.0))
    });

    let mut table = Table::new();
    apply_table_style(&mut table, resolve_table_style());
    table.set_header(vec!["File", "Symbols", "Avg Confidence", "Top Factor"]);

    for (file, count, avg, factor) in &rows {
        table.add_row(vec![
            Cell::new(file),
            Cell::new(count),
            Cell::new(format!("{:.0}%", avg * 100.0)),
            Cell::new(factor),
        ]);
    }
    println!("{table}");
    println!("  {DEAD_CODE_HONESTY_FOOTER}");
}

pub fn print_dead_code_explanation(findings: &[DeadCodeFinding], file_path: &str) {
    let explanation =
        crate::impact::analysis::dead_code::compute_dead_code_explanation(file_path, findings);
    print_dead_code_explanation_struct(&explanation);
}

pub fn print_dead_code_explanation_struct(
    explanation: &crate::impact::analysis::dead_code::DeadCodeExplanation,
) {
    if explanation.symbols.is_empty() {
        println!(
            "\nNo findings for '{}' above threshold (heuristic analysis).",
            explanation.file
        );
        println!("  {DEAD_CODE_HONESTY_FOOTER}");
        return;
    }

    println!(
        "\n{}",
        format!("Dead Code Analysis: {}", explanation.file)
            .if_supports_color(Stream::Stdout, |s| s.bold())
    );
    println!("\nSymbols flagged: {}\n", explanation.symbols.len());

    for symbol in &explanation.symbols {
        println!(
            "  {} ({:.0}% confidence)",
            symbol.symbol_name,
            symbol.confidence * 100.0
        );
        for factor in &symbol.factors {
            let name = match &factor.kind {
                crate::impact::packet::ConfidenceFactor::UnreachableFromEntrypoints => {
                    "UnreachableFromEntrypoints"
                }
                crate::impact::packet::ConfidenceFactor::GitInactive { .. } => "GitInactive",
                crate::impact::packet::ConfidenceFactor::NoTestCoverage => "NoTestCoverage",
            };
            println!("    {}: {}", name, factor.description);
        }
        println!();
    }
    println!("  {DEAD_CODE_HONESTY_FOOTER}");
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::ExecutionResult;

    #[test]
    fn print_verify_result_quiet_suppresses_success_keeps_failure() {
        use crate::output::verification::verify_step_result_label;

        // Pure gate: SUCCESS text not emitted when verbose=false; FAILURE always.
        assert_eq!(
            verify_step_result_label(0, false),
            None,
            "quiet SUCCESS must not emit SUCCESS text"
        );
        assert_eq!(
            verify_step_result_label(0, true),
            Some("SUCCESS"),
            "verbose SUCCESS must emit SUCCESS"
        );
        assert_eq!(
            verify_step_result_label(1, false),
            Some("FAILURE"),
            "quiet FAILURE must still emit FAILURE"
        );
        assert_eq!(
            verify_step_result_label(1, true),
            Some("FAILURE"),
            "verbose FAILURE must emit FAILURE"
        );

        // Smoke: production print path must not panic either way.
        let pass = ExecutionResult {
            exit_code: 0,
            stdout: String::new(),
            stderr: String::new(),
            duration: std::time::Duration::from_millis(1),
            truncated: false,
        };
        let fail = ExecutionResult {
            exit_code: 1,
            stdout: String::new(),
            stderr: "err".into(),
            duration: std::time::Duration::from_millis(1),
            truncated: false,
        };
        print_verify_result("step", 30, &pass, false);
        print_verify_result("step", 30, &pass, true);
        print_verify_result("step", 30, &fail, false);
        print_verify_result("step", 30, &fail, true);
    }

    #[test]
    fn doctor_summary_text_four_way() {
        assert_eq!(
            format_doctor_summary_text(1, 0, 0, 0),
            "✗ Doctor: 1 block issue(s)"
        );
        assert_eq!(
            format_doctor_summary_text(0, 2, 0, 0),
            "✓ Doctor: ready for publish env · 2 warning(s)"
        );
        assert_eq!(
            format_doctor_summary_text(0, 0, 0, 3),
            "✓ Doctor: ready for publish env · 3 hint(s)"
        );
        assert_eq!(
            format_doctor_summary_text(0, 0, 0, 0),
            "✓ Doctor: all checks passed"
        );
        // Block wins over warn/info when present.
        assert!(format_doctor_summary_text(1, 9, 0, 9).contains("block issue"));
        // Warn uses ready shape — never red soft-fail wording.
        assert!(!format_doctor_summary_text(0, 2, 0, 0).contains("issue(s) found"));
    }

    /// 0209 DoD-1 six-row header copy (unit fixtures, not live dogfood).
    #[test]
    fn doctor_summary_text_warn_split_six_row() {
        // Row 1: 3 action + 1 optional
        let row1 = format_doctor_summary_text(0, 3, 1, 0);
        assert!(row1.contains("3 warning(s) · 1 optional"), "{row1}");
        assert!(!row1.contains("optional warning(s)"), "{row1}");

        // Row 2: 3 action only
        let row2 = format_doctor_summary_text(0, 3, 0, 0);
        assert!(row2.contains("3 warning(s)"), "{row2}");
        assert!(!row2.contains("optional"), "{row2}");

        // Row 3: 1 optional only — no "0 warning", no "optional warning(s)"
        let row3 = format_doctor_summary_text(0, 0, 1, 0);
        assert!(row3.contains("· 1 optional"), "{row3}");
        assert!(!row3.contains("0 warning"), "{row3}");
        assert!(!row3.contains("optional warning(s)"), "{row3}");
        assert!(!row3.contains("warning(s)"), "{row3}");

        // Row 4: block wins; no ready-shape
        let row4 = format_doctor_summary_text(1, 2, 1, 0);
        assert!(row4.contains("block issue(s)"), "{row4}");
        assert!(!row4.contains("ready for publish"), "{row4}");

        // Row 5: empty
        let row5 = format_doctor_summary_text(0, 0, 0, 0);
        assert!(row5.contains("all checks passed"), "{row5}");

        // Row 6: info only
        let row6 = format_doctor_summary_text(0, 0, 0, 3);
        assert!(row6.contains("3 hint(s)"), "{row6}");
    }

    #[test]
    fn format_doctor_tool_line_gemini_cli_vs_cloud_ask() {
        use std::path::PathBuf;

        let not_found = ExecutableStatus::NotFound;
        let gemini_found = ExecutableStatus::Found(PathBuf::from(r"C:\Users\bin\gemini.exe"));
        let gemini_cli_found =
            ExecutableStatus::Found(PathBuf::from(r"C:\Users\bin\gemini-cli.exe"));

        let banned = |text: &str| {
            let lower = text.to_ascii_lowercase();
            assert!(!lower.contains("install"), "{text}");
            assert!(!lower.contains("npm"), "{text}");
            assert!(!lower.contains("antigravity"), "{text}");
        };

        // Row 1: gemini NotFound
        let (label, text) = format_doctor_tool_line("gemini", &not_found);
        assert_eq!(label, "gemini CLI");
        assert!(text.contains("NOT FOUND"), "{text}");
        assert!(text.contains("optional CLI"), "{text}");
        assert!(text.contains("not the Cloud Ask backend"), "{text}");
        banned(&text);

        // Row 2: gemini-cli NotFound
        let (label, text) = format_doctor_tool_line("gemini-cli", &not_found);
        assert_eq!(label, "gemini CLI");
        assert!(text.contains("NOT FOUND"), "{text}");
        assert!(text.contains("optional CLI"), "{text}");
        assert!(text.contains("not the Cloud Ask backend"), "{text}");
        banned(&text);

        // Row 3: gemini Found
        let (label, text) = format_doctor_tool_line("gemini", &gemini_found);
        assert_eq!(label, "gemini CLI");
        assert!(text.contains("Found ("), "{text}");
        assert!(!text.contains("NOT FOUND"), "{text}");

        // Row 4: git NotFound — unchanged, no CLI/Ask clause
        let (label, text) = format_doctor_tool_line("git", &not_found);
        assert_eq!(label, "git");
        assert_eq!(text, "NOT FOUND");
        assert!(!text.contains("Cloud Ask"), "{text}");
        assert!(!text.contains("optional CLI"), "{text}");

        // Row 5: gemini-cli Found
        let (label, text) = format_doctor_tool_line("gemini-cli", &gemini_cli_found);
        assert_eq!(label, "gemini CLI");
        assert!(text.contains("Found ("), "{text}");
        assert!(!text.contains("NOT FOUND"), "{text}");
    }

    #[test]
    fn format_hygiene_collapse_trailer_optional_clause() {
        let t11 = format_hygiene_collapse_trailer(11, 1);
        assert!(t11.contains("11 hygiene finding(s) collapsed"), "{t11}");
        assert!(t11.contains("1 optional warning"), "{t11}");
        assert!(t11.contains("doctor --full"), "{t11}");

        let t12 = format_hygiene_collapse_trailer(12, 2);
        assert!(t12.contains("2 optional warnings"), "{t12}");

        let t10 = format_hygiene_collapse_trailer(10, 0);
        assert_eq!(t10, "10 hygiene finding(s) collapsed — run doctor --full");
        assert!(!t10.contains("optional"), "{t10}");
    }

    #[test]
    fn format_signing_deferred_trailer_exact() {
        assert_eq!(
            format_signing_deferred_trailer(3),
            "3 signing finding(s) deferred (observe) — run doctor --full"
        );
        assert_eq!(
            format_signing_deferred_trailer(1),
            "1 signing finding(s) deferred (observe) — run doctor --full"
        );
        let hygiene = format_hygiene_collapse_trailer(10, 0);
        assert_eq!(
            hygiene,
            "10 hygiene finding(s) collapsed — run doctor --full"
        );
        assert!(!hygiene.contains("signing"), "{hygiene}");
        assert!(!hygiene.contains("deferred"), "{hygiene}");
    }

    /// 0174 T1–T5: human 3-tier partition + --full expands hygiene.
    #[test]
    fn doctor_human_partition_three_tier() {
        use crate::commands::doctor::{DoctorCategory, DoctorFinding};

        let findings = vec![
            DoctorFinding::warn(
                "completion-unreachable",
                DoctorCategory::Optional,
                "completion down",
            ),
            DoctorFinding::warn("hook-template-stale", DoctorCategory::Gate, "hooks stale"),
            DoctorFinding::warn("sig-pin", DoctorCategory::Signing, "no keys")
                .with_remediation("ledgerful config set 'intent.trusted_public_keys=[\"hex\"]'"),
            DoctorFinding::block("tool-git", DoctorCategory::Tools, "git missing"),
            DoctorFinding::info("sccache-hint", DoctorCategory::Optional, "install sccache"),
        ];

        let (index, optional, hygiene) = partition_doctor_findings_for_human(&findings, false);
        // T1 optional warn + T2 info (sccache) collapsed → hygiene_count=2
        // 0226: hook-template-stale is Warn/Gate → expanded, not hygiene
        assert_eq!(hygiene, 2);
        // T3 sig-pin + T4 block + hook-template-stale expanded
        let codes: Vec<&str> = index.iter().map(|f| f.code.as_str()).collect();
        assert!(codes.contains(&"sig-pin"));
        assert!(codes.contains(&"tool-git"));
        assert!(codes.contains(&"hook-template-stale"));
        assert!(!codes.contains(&"completion-unreachable"));
        assert!(
            optional.is_empty(),
            "optional findings collapsed by default"
        );

        // T5 --full expands hygiene
        let (index_full, optional_full, hygiene_full) =
            partition_doctor_findings_for_human(&findings, true);
        assert_eq!(hygiene_full, 2);
        let full_index_codes: Vec<&str> = index_full.iter().map(|f| f.code.as_str()).collect();
        assert!(full_index_codes.contains(&"hook-template-stale"));
        assert!(full_index_codes.contains(&"sig-pin"));
        assert!(full_index_codes.contains(&"tool-git"));
        let full_opt_codes: Vec<&str> = optional_full.iter().map(|f| f.code.as_str()).collect();
        assert!(full_opt_codes.contains(&"completion-unreachable"));
        assert!(full_opt_codes.contains(&"sccache-hint"));

        let trailer = format_hygiene_collapse_trailer(3, 1);
        assert!(trailer.contains("3 hygiene finding(s) collapsed"));
        assert!(trailer.contains("doctor --full"));
    }

    /// 0225: later signing omitted from default Index Health; hygiene_count unchanged.
    #[test]
    fn doctor_human_partition_later_signing_omitted_not_hygiene() {
        use crate::commands::doctor::{DoctorCategory, DoctorFinding, SessionPriority};

        let mut later_pin = DoctorFinding::warn("sig-pin", DoctorCategory::Signing, "no keys");
        later_pin.session_priority = SessionPriority::Later;
        let mut later_ver = DoctorFinding::warn("sig-version", DoctorCategory::Signing, "v1 rows");
        later_ver.session_priority = SessionPriority::Later;
        let mut later_phantom = DoctorFinding::warn(
            "PHANTOM_PROMOTED_WITHOUT_VERIFY",
            DoctorCategory::Signing,
            "phantoms",
        );
        later_phantom.session_priority = SessionPriority::Later;
        let behind = DoctorFinding::warn(
            "binary-behind-tree",
            DoctorCategory::Tools,
            "PATH binary lags tree",
        );
        let hygiene_info =
            DoctorFinding::info("sccache-hint", DoctorCategory::Optional, "sccache hint");
        let hook_stale =
            DoctorFinding::warn("hook-template-stale", DoctorCategory::Gate, "hooks stale");

        let findings = vec![
            later_pin,
            later_ver,
            later_phantom,
            behind,
            hygiene_info,
            hook_stale,
        ];

        let (index, optional, hygiene) = partition_doctor_findings_for_human(&findings, false);
        assert_eq!(hygiene, 1, "later must not increment hygiene_count");
        let codes: Vec<&str> = index.iter().map(|f| f.code.as_str()).collect();
        assert!(codes.contains(&"binary-behind-tree"));
        assert!(codes.contains(&"hook-template-stale"));
        assert!(optional.is_empty());
        assert!(!codes.contains(&"sig-pin"));
        assert!(!codes.contains(&"sig-version"));
        assert!(!codes.contains(&"PHANTOM_PROMOTED_WITHOUT_VERIFY"));

        let (index_full, _, hygiene_full) = partition_doctor_findings_for_human(&findings, true);
        assert_eq!(hygiene_full, 1);
        let full_codes: Vec<&str> = index_full.iter().map(|f| f.code.as_str()).collect();
        assert!(full_codes.contains(&"sig-pin"));
        assert!(full_codes.contains(&"sig-version"));
        assert!(full_codes.contains(&"PHANTOM_PROMOTED_WITHOUT_VERIFY"));
        assert!(full_codes.contains(&"binary-behind-tree"));
        assert!(full_codes.contains(&"hook-template-stale"));
    }

    /// 0225 Codex P2-1: printer emits deferred trailer; `--full` expands bodies.
    #[test]
    fn print_doctor_report_later_trailer_and_full_expand() {
        use crate::commands::doctor::{DoctorCategory, DoctorFinding, SessionPriority, summarize};

        fn later(code: &str, msg: &str) -> DoctorFinding {
            let mut f = DoctorFinding::warn(code, DoctorCategory::Signing, msg);
            f.session_priority = SessionPriority::Later;
            f
        }

        let findings = vec![
            later("PHANTOM_PROMOTED_WITHOUT_VERIFY", "phantoms"),
            later("sig-pin", "no keys"),
            later("sig-version", "v1 rows"),
            DoctorFinding::warn(
                "binary-behind-tree",
                DoctorCategory::Tools,
                "PATH binary lags tree",
            ),
            DoctorFinding::warn("hook-template-stale", DoctorCategory::Gate, "hooks stale"),
            DoctorFinding::info("sccache-hint", DoctorCategory::Optional, "sccache"),
        ];

        let tools: Vec<(String, ExecutableStatus)> = Vec::new();
        let report = DoctorReport {
            platform: "test",
            shell: "test",
            tools: &tools,
            path_display: "test",
            path_kind: "test",
            work_root: "test",
            state_dir: "test/.ledgerful",
            is_wsl_mounted: false,
            embedding_model_status: "OK".to_string(),
            embedding_model_failed: false,
            completion_model_status: "OK".to_string(),
            native_graph_status: "Ready".to_string(),
            active_ask_backend: "test".to_string(),
            index_health: vec!["Search index: OK (1 documents)".to_string()],
            target_triple: "test",
        };
        let counts = summarize(&findings);
        let summary = DoctorSummaryCounts {
            block: counts.block,
            warn: counts.warn,
            info: counts.info,
        };

        let mut default_buf = Vec::new();
        print_doctor_report_to(
            &mut default_buf,
            &report,
            &summary,
            &findings,
            DoctorHumanProfile {
                full: false,
                quiet: true,
            },
        )
        .expect("write default");
        let default = String::from_utf8(default_buf).expect("utf8");
        assert!(
            default.contains("3 signing finding(s) deferred (observe) — run doctor --full"),
            "{default}"
        );
        assert!(
            default.contains("1 hygiene finding(s) collapsed — run doctor --full"),
            "{default}"
        );
        assert!(!default.contains("[sig-pin]"), "{default}");
        assert!(!default.contains("[sig-version]"), "{default}");
        assert!(
            !default.contains("[PHANTOM_PROMOTED_WITHOUT_VERIFY]"),
            "{default}"
        );
        assert!(default.contains("[binary-behind-tree]"), "{default}");
        assert!(default.contains("[hook-template-stale]"), "{default}");
        assert!(!default.contains("[sccache-hint]"), "{default}");
        assert!(default.contains("warning(s)"), "{default}");

        let mut full_buf = Vec::new();
        print_doctor_report_to(
            &mut full_buf,
            &report,
            &summary,
            &findings,
            DoctorHumanProfile {
                full: true,
                quiet: true,
            },
        )
        .expect("write full");
        let full = String::from_utf8(full_buf).expect("utf8");
        assert!(full.contains("[sig-pin]"), "{full}");
        assert!(full.contains("[sig-version]"), "{full}");
        assert!(full.contains("[PHANTOM_PROMOTED_WITHOUT_VERIFY]"), "{full}");
        assert!(full.contains("[binary-behind-tree]"), "{full}");
        assert!(full.contains("[hook-template-stale]"), "{full}");
        assert!(full.contains("[sccache-hint]"), "{full}");
        assert!(!full.contains("signing finding(s) deferred"), "{full}");
        assert!(!full.contains("hygiene finding(s) collapsed"), "{full}");
    }

    #[test]
    fn print_doctor_report_hygiene_only_trailer_byte_stable_without_later() {
        use crate::commands::doctor::{DoctorCategory, DoctorFinding, summarize};

        let findings = vec![
            DoctorFinding::warn("hook-template-stale", DoctorCategory::Gate, "hooks stale"),
            DoctorFinding::info("sccache-hint", DoctorCategory::Optional, "sccache"),
            DoctorFinding::info("tool-gemini", DoctorCategory::Optional, "gemini"),
            DoctorFinding::info("scip-rust-missing", DoctorCategory::Optional, "scip"),
        ];
        let tools: Vec<(String, ExecutableStatus)> = Vec::new();
        let report = DoctorReport {
            platform: "test",
            shell: "test",
            tools: &tools,
            path_display: "test",
            path_kind: "test",
            work_root: "test",
            state_dir: "test/.ledgerful",
            is_wsl_mounted: false,
            embedding_model_status: "OK".to_string(),
            embedding_model_failed: false,
            completion_model_status: "OK".to_string(),
            native_graph_status: "Ready".to_string(),
            active_ask_backend: "test".to_string(),
            index_health: Vec::new(),
            target_triple: "test",
        };
        let counts = summarize(&findings);
        let summary = DoctorSummaryCounts {
            block: counts.block,
            warn: counts.warn,
            info: counts.info,
        };
        let mut buf = Vec::new();
        print_doctor_report_to(
            &mut buf,
            &report,
            &summary,
            &findings,
            DoctorHumanProfile::default(),
        )
        .expect("write");
        let text = String::from_utf8(buf).expect("utf8");
        assert!(
            text.contains("3 hygiene finding(s) collapsed — run doctor --full"),
            "{text}"
        );
        assert!(text.contains("[hook-template-stale]"), "{text}");
        assert!(!text.contains("[sccache-hint]"), "{text}");
        assert!(!text.contains("[tool-gemini]"), "{text}");
        assert!(!text.contains("[scip-rust-missing]"), "{text}");
        assert!(!text.contains("signing finding(s) deferred"), "{text}");
        assert!(!text.contains("optional warning"), "{text}");
    }

    /// 0209-D: mixed info tool-gemini + optional warn; trailer uses warnOptional.
    #[test]
    fn doctor_mixed_info_tool_gemini_trailer_uses_warn_optional() {
        use crate::commands::doctor::{DoctorCategory, DoctorFinding, split_doctor_warns};

        let findings = vec![
            DoctorFinding::info(
                "tool-gemini",
                DoctorCategory::Optional,
                "gemini NOT FOUND (optional CLI; not the Cloud Ask backend)",
            ),
            DoctorFinding::warn(
                "completion-unreachable",
                DoctorCategory::Optional,
                "completion down",
            ),
            DoctorFinding::info("sccache-hint", DoctorCategory::Optional, "sccache hint"),
        ];
        let split = split_doctor_warns(&findings);
        assert_eq!(split.optional, 1, "info must not increment warnOptional");
        assert_eq!(split.action, 0);
        assert_eq!(split.total, 1);
        let hygiene = findings.len();
        let trailer = format_hygiene_collapse_trailer(hygiene, split.optional);
        assert!(trailer.contains("1 optional warning"), "{trailer}");
        assert!(!trailer.contains("3 optional"), "{trailer}");
        assert!(trailer.contains("doctor --full"), "{trailer}");
    }

    #[test]
    fn doctor_human_profile_defaults() {
        let p = DoctorHumanProfile::default();
        assert!(!p.full);
        assert!(!p.quiet);
    }

    /// 0174 T6: quiet suppresses remediations; default prints them.
    #[test]
    fn doctor_quiet_suppresses_remediation_gate() {
        assert!(doctor_should_print_remediation(false));
        assert!(!doctor_should_print_remediation(true));
        let quiet = DoctorHumanProfile {
            full: false,
            quiet: true,
        };
        assert!(!doctor_should_print_remediation(quiet.quiet));
        // Full + quiet still suppress remediations (quiet orthogonal to full).
        let full_quiet = DoctorHumanProfile {
            full: true,
            quiet: true,
        };
        assert!(!doctor_should_print_remediation(full_quiet.quiet));
    }

    /// 0226 invert of 0174 hook-template collapse: Warn/Gate is visible by default.
    #[test]
    fn doctor_human_hook_template_stale_visible_on_default() {
        use crate::commands::doctor::{DoctorCategory, DoctorFinding, summarize};

        let findings = vec![
            DoctorFinding::warn("hook-template-stale", DoctorCategory::Gate, "hooks stale"),
            DoctorFinding::info("tool-gemini", DoctorCategory::Optional, "gemini"),
            DoctorFinding::info("scip-rust-missing", DoctorCategory::Optional, "scip"),
            DoctorFinding::info("sccache-hint", DoctorCategory::Optional, "sccache"),
            DoctorFinding::warn("impact-stale", DoctorCategory::Index, "impact stale"),
        ];
        let (index, _, hygiene) = partition_doctor_findings_for_human(&findings, false);
        let codes: Vec<&str> = index.iter().map(|f| f.code.as_str()).collect();
        assert!(codes.contains(&"hook-template-stale"));
        assert!(codes.contains(&"impact-stale"));
        assert!(!codes.contains(&"tool-gemini"));
        assert!(!codes.contains(&"scip-rust-missing"));
        assert_eq!(hygiene, 3);

        let tools: Vec<(String, ExecutableStatus)> = Vec::new();
        let report = DoctorReport {
            platform: "test",
            shell: "test",
            tools: &tools,
            path_display: "test",
            path_kind: "test",
            work_root: "test",
            state_dir: "test/.ledgerful",
            is_wsl_mounted: false,
            embedding_model_status: "OK".to_string(),
            embedding_model_failed: false,
            completion_model_status: "OK".to_string(),
            native_graph_status: "Ready".to_string(),
            active_ask_backend: "test".to_string(),
            index_health: Vec::new(),
            target_triple: "test",
        };
        let counts = summarize(&findings);
        let summary = DoctorSummaryCounts {
            block: counts.block,
            warn: counts.warn,
            info: counts.info,
        };
        let mut buf = Vec::new();
        print_doctor_report_to(
            &mut buf,
            &report,
            &summary,
            &findings,
            DoctorHumanProfile {
                full: false,
                quiet: true,
            },
        )
        .expect("write");
        let text = String::from_utf8(buf).expect("utf8");
        assert!(text.contains("[hook-template-stale]"), "{text}");
        assert!(text.contains("[impact-stale]"), "{text}");
        assert!(!text.contains("[tool-gemini]"), "{text}");
        assert!(!text.contains("[scip-rust-missing]"), "{text}");
    }

    /// 0226 DoD-1 / DoD-4: acked later signing omits bodies and later trailer.
    #[test]
    fn doctor_human_acked_signing_omits_bodies_not_later_trailer() {
        use crate::commands::doctor::{DoctorCategory, DoctorFinding, SessionPriority, summarize};

        fn acked_later(code: &str, msg: &str) -> DoctorFinding {
            let mut f = DoctorFinding::warn(code, DoctorCategory::Signing, msg);
            f.session_priority = SessionPriority::Later;
            f.acknowledged = true;
            f
        }

        let findings = vec![
            acked_later("PHANTOM_PROMOTED_WITHOUT_VERIFY", "phantoms"),
            acked_later("sig-pin", "no keys"),
            acked_later("sig-version", "v1 rows"),
            DoctorFinding::warn(
                "binary-behind-tree",
                DoctorCategory::Tools,
                "PATH binary lags tree",
            ),
        ];
        let tools: Vec<(String, ExecutableStatus)> = Vec::new();
        let report = DoctorReport {
            platform: "test",
            shell: "test",
            tools: &tools,
            path_display: "test",
            path_kind: "test",
            work_root: "test",
            state_dir: "test/.ledgerful",
            is_wsl_mounted: false,
            embedding_model_status: "OK".to_string(),
            embedding_model_failed: false,
            completion_model_status: "OK".to_string(),
            native_graph_status: "Ready".to_string(),
            active_ask_backend: "test".to_string(),
            index_health: Vec::new(),
            target_triple: "test",
        };
        let counts = summarize(&findings);
        let summary = DoctorSummaryCounts {
            block: counts.block,
            warn: counts.warn,
            info: counts.info,
        };
        let mut buf = Vec::new();
        print_doctor_report_to(
            &mut buf,
            &report,
            &summary,
            &findings,
            DoctorHumanProfile {
                full: false,
                quiet: true,
            },
        )
        .expect("write");
        let text = String::from_utf8(buf).expect("utf8");
        assert!(!text.contains("[sig-pin]"), "{text}");
        assert!(!text.contains("[sig-version]"), "{text}");
        assert!(
            !text.contains("[PHANTOM_PROMOTED_WITHOUT_VERIFY]"),
            "{text}"
        );
        assert!(text.contains("[binary-behind-tree]"), "{text}");
        assert!(
            !text.contains("signing finding(s) deferred"),
            "acked codes must not inflate later trailer: {text}"
        );
        let json = serde_json::to_value(&findings).expect("json");
        for code in ["sig-pin", "sig-version", "PHANTOM_PROMOTED_WITHOUT_VERIFY"] {
            let row = json
                .as_array()
                .unwrap()
                .iter()
                .find(|v| v["code"] == code)
                .unwrap_or_else(|| panic!("{code}"));
            assert_eq!(row["acknowledged"], true);
            assert_eq!(row["sessionPriority"], "later");
        }
    }

    #[test]
    fn dead_code_honesty_strings_present() {
        assert!(DEAD_CODE_HONESTY_FOOTER.contains("Heuristic evidence"));
        assert!(DEAD_CODE_HONESTY_FOOTER.contains("not proof of dead code"));
        assert!(DEAD_CODE_EMPTY_STATE.contains("heuristic analysis"));
        assert!(!DEAD_CODE_EMPTY_STATE.contains("No dead code found"));
    }

    #[test]
    fn wsl_support_line_mounted_and_unmounted() {
        assert_eq!(
            wsl_support_line(true),
            Some("WSL Support:         Active (Mounted)")
        );
        assert_eq!(wsl_support_line(false), None);
    }
}
