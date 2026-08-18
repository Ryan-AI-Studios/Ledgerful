//! Agent change-context packet (track 0114).
//!
//! Composes impact (in-memory only), doctor sidecar, ledger pending, and a
//! budgeted `readSet` into one versioned camelCase JSON packet for agents.
//! Never calls `execute_impact_silent*` (does not rewrite `latest-impact.json`).

mod build;
mod emit;
mod packet;
mod storage;

pub use build::{build_change_context, build_change_context_from_cwd};
pub(crate) use build::{not_ready_packet, read_doctor_section};
pub(crate) use emit::emit_packet;
pub(crate) use packet::NotReadyErrorClass;
pub use packet::{
    ActiveTxEntry, AffectedFlowsSummary, AgentSummary, BlastSummary, CHANGE_CONTEXT_SCHEMA_VERSION,
    ChangeContextDetail, ChangeContextOpts, ChangeContextPacket, ChangedClassCounts,
    DEFAULT_MAX_FILES, DoctorSection, DoctorTopFinding, GREENFIELD_SUGGESTED_TESTS_ACTION,
    LedgerSection, ReadSetEntry, TestCoverageSummary,
};
pub(crate) use storage::{open_storage_for_change_context, storage_unavailable_reason};

use miette::Result;

/// CLI entrypoint: resolve layout/storage/config, build packet, print human or JSON.
pub fn execute_change_context(opts: ChangeContextOpts, json: bool) -> Result<()> {
    let layout = match crate::commands::helpers::get_layout() {
        Ok(l) => l,
        Err(e) => {
            let packet = not_ready_packet(
                format!("layout unavailable: {e}"),
                opts.base_ref.clone(),
                DoctorSection {
                    status: "missing".to_string(),
                    ready_for_publish: false,
                    block: 0,
                    warn: 0,
                    info: 0,
                    top_findings: Vec::new(),
                },
                LedgerSection {
                    pending_count: 0,
                    active_tx: Vec::new(),
                },
                NotReadyErrorClass::LayoutUnavailable,
            );
            return emit_packet(&packet, json);
        }
    };

    let config = crate::config::load::load_config(&layout).unwrap_or_default();
    let storage = match open_storage_for_change_context(&layout) {
        Ok(s) => s,
        Err((e, class)) => {
            let packet = not_ready_packet(
                storage_unavailable_reason(&e, class),
                opts.base_ref.clone(),
                read_doctor_section(&layout),
                LedgerSection {
                    pending_count: 0,
                    active_tx: Vec::new(),
                },
                class,
            );
            return emit_packet(&packet, json);
        }
    };

    let packet = build_change_context(&opts, &layout, &storage, &config)?;
    let _ = storage.shutdown();
    emit_packet(&packet, json)
}

#[cfg(test)]
mod tests;
