//! Human / JSON emit for session briefing.

use super::packet::SessionEnvelope;
use miette::Result;

pub(crate) fn emit_session(envelope: &SessionEnvelope, json: bool) -> Result<()> {
    if json {
        let out = serde_json::to_string_pretty(envelope)
            .map_err(|e| miette::miette!("Failed to serialize session: {e}"))?;
        println!("{out}");
    } else {
        print_human(envelope);
    }
    Ok(())
}

/// Ten-line human summary. Must not parse as JSON (0093).
pub(crate) fn format_human(envelope: &SessionEnvelope) -> String {
    let dirty_display = if envelope.git.dirty_paths.is_empty() {
        "(none)".to_string()
    } else {
        envelope.git.dirty_paths.join(", ")
    };
    let next_display = envelope
        .next
        .first()
        .cloned()
        .unwrap_or_else(|| "(none)".to_string());
    let branch = if envelope.git.branch.is_empty() {
        "(unknown)"
    } else {
        envelope.git.branch.as_str()
    };
    let head = if envelope.git.head.is_empty() {
        "(unknown)"
    } else {
        envelope.git.head.as_str()
    };
    let risk = if envelope.change_context.risk_level.is_empty() {
        "(none)"
    } else {
        envelope.change_context.risk_level.as_str()
    };
    [
        "Ledgerful session".to_string(),
        format!("  git: {branch} {head} dirty {}", envelope.git.dirty_count),
        format!("  dirty: {dirty_display}"),
        format!(
            "  ledger: {} pending, {} drift  workRoot={}",
            envelope.ledger.pending_count,
            envelope.ledger.unaudited_drift,
            envelope.ledger.work_root
        ),
        format!("  collisions: {}", envelope.ledger.collisions.len()),
        format!(
            "  doctor: ready={} block={} warn={} info={}",
            envelope.doctor.ready_for_publish,
            envelope.doctor.block,
            envelope.doctor.warn,
            envelope.doctor.info
        ),
        format!(
            "  change-context: {} risk={} readSet {}/{} capped={}",
            envelope.change_context.status,
            risk,
            envelope.change_context.read_set.len(),
            envelope.change_context.read_set_total_candidates,
            envelope.change_context.read_set_capped
        ),
        format!(
            "  hotspots: {} files (tests excluded)",
            envelope.hotspots.files.len()
        ),
        format!(
            "  impactCache: present={} validForHead={} treeClean={}",
            envelope.impact_cache.present,
            envelope.impact_cache.valid_for_head,
            envelope.impact_cache.tree_clean
        ),
        format!("  next: {next_display}"),
    ]
    .join("\n")
}

fn print_human(envelope: &SessionEnvelope) {
    println!("{}", format_human(envelope));
}
