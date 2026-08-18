//! Human / JSON emit for change-context packets.

use super::packet::ChangeContextPacket;
use miette::Result;

pub(crate) fn emit_packet(packet: &ChangeContextPacket, json: bool) -> Result<()> {
    if json {
        let out = serde_json::to_string_pretty(packet)
            .map_err(|e| miette::miette!("Failed to serialize change-context: {e}"))?;
        println!("{out}");
    } else {
        print_human(packet);
    }
    Ok(())
}

fn print_human(packet: &ChangeContextPacket) {
    use owo_colors::{OwoColorize, Stream, Style};

    println!(
        "{}",
        "Ledgerful change-context"
            .if_supports_color(Stream::Stdout, |s| s.style(Style::new().bold().underline()))
    );
    // agentSummary header first (0173) — ≤ ~12 lines, then existing sections.
    if let Some(ref agent) = packet.agent_summary {
        println!("  agentSummary:");
        println!("    risk:             {}", agent.risk_one_liner);
        println!(
            "    changed:          total={} code={} governance={} contract={}",
            agent.changed.total,
            agent.changed.code,
            agent.changed.governance,
            agent.changed.contract
        );
        if !agent.top_symbols.is_empty() {
            println!("    topSymbols:       {}", agent.top_symbols.join(", "));
        }
        if !agent.must_touch_sample.is_empty() {
            println!(
                "    mustTouch:        {}",
                agent.must_touch_sample.join(", ")
            );
        }
        if !agent.suggested_tests_sample.is_empty() {
            println!(
                "    suggestedTests:   {}",
                agent.suggested_tests_sample.join(", ")
            );
        }
        println!(
            "    demotedTemporal:  {}  pathMode={}  analysisMode={}",
            agent.demoted_temporal_count, agent.path_mode, agent.analysis_mode
        );
    }
    println!("  status:           {}", packet.status);
    println!("  summary:          {}", packet.summary);
    if let Some(ref risk) = packet.risk_level {
        println!("  risk:             {risk}");
    }
    println!(
        "  readSet:          {} (capped={}, candidates={})",
        packet.read_set.len(),
        packet.read_set_capped,
        packet.read_set_total_candidates
    );
    if let Some(ref cov) = packet.test_coverage {
        println!(
            "  testCoverage:     status={} mapped={} fileMapped={} unmapped={}",
            cov.status.as_str(),
            cov.mapped_count,
            cov.file_mapped_count,
            cov.unmapped_count
        );
        if cov.unmapped_count > 0 {
            eprintln!(
                "warning: {} production symbol(s)/file(s) lack structural test_mapping (not line coverage)",
                cov.unmapped_count
            );
        }
    }
    if let Some(ref flows) = packet.affected_flows {
        println!(
            "  affectedFlows:    status={} flowCount={}",
            flows.status.as_str(),
            flows.flow_count
        );
    }
    if let Some(ref hints) = packet.change_hints {
        println!(
            "  changeHints:      kind={} suggestedTests={} mostlyAdded={} newPrefixes={}",
            hints.kind.as_str(),
            hints.suggested_tests.len(),
            hints.mostly_added,
            hints.new_package_prefixes.len()
        );
    }
    println!(
        "  doctor:           {} readyForPublish={} (block={} warn={} info={})",
        packet.doctor.status,
        packet.doctor.ready_for_publish,
        packet.doctor.block,
        packet.doctor.warn,
        packet.doctor.info
    );
    println!(
        "  ledger:           pendingCount={}",
        packet.ledger.pending_count
    );
    if !packet.next_actions.is_empty() {
        println!("  nextActions:");
        for a in &packet.next_actions {
            println!("    - {a}");
        }
    }
}
