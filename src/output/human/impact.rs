use crate::impact::packet::{BlastRadius, ImpactPacket, RiskLevel, TemporalCoupling};
use crate::observability::signal::{ObservabilitySignal, SignalSeverity};
use crate::output::table::{apply_table_style, resolve_table_style};
use comfy_table::{Cell, Color, Table};
use owo_colors::{OwoColorize, Stream, Style};

pub fn print_impact_summary(packet: &ImpactPacket) {
    print_impact_summary_with_full(packet, false);
}

pub fn print_impact_summary_with_full(packet: &ImpactPacket, full: bool) {
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

    let docs_mode = packet.glossary.is_some();
    if docs_mode {
        println!(
            "\n{}",
            crate::impact::lead::format_actionable_section(packet)
        );
    }

    if !packet.hotspots.is_empty() {
        super::print_hotspots(&packet.hotspots);
    }

    if let Some(ref blast) = packet.blast_radius
        && !blast.is_empty_for_serde()
    {
        print_structural_blast(blast);
    }

    if !packet.temporal_couplings.is_empty() {
        if docs_mode && !full {
            let remaining = crate::impact::lead::remaining_coupling_count(packet);
            if remaining > 0 {
                println!(
                    "\n{}",
                    crate::impact::lead::format_more_couplings_line(remaining)
                );
            }
        } else {
            print_temporal_couplings(&packet.temporal_couplings);
        }
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
    if packet.glossary.is_some() {
        println!("{}", crate::impact::lead::format_actionable_section(packet));
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
