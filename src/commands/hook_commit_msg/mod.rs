//! Commit-msg hook: intent gate, sidecar GC, provenance SoT, intent, record.
//!
//! Barrel split of the former `commands/hook_commit_msg.rs` (0270). Public
//! path stays `crate::commands::hook_commit_msg`.

mod execute;
mod gc;
mod helpers;
mod intent;
mod record;
mod sot;
mod staged;

// Re-export for tests / historical imports.
pub use crate::commands::hook_sidecar::PendingHookTx;

pub use execute::execute_hook_commit_msg;
pub use helpers::{
    extract_trailers, is_trivial_commit, is_well_formed_conventional, parse_category_from_message,
    risk_from_category,
};
pub use intent::is_tui_skip_disposition;
pub use record::{SKIPPED_COVERAGE_RISK, SKIPPED_SUMMARY_PREFIX, skipped_coverage_summary};
pub use sot::{
    LedgerRefStatus, ProvenanceSotClass, classify_provenance_sot, extract_ledger_tx_ref,
};
pub use staged::canonical_entity;

#[cfg(test)]
mod tests;
