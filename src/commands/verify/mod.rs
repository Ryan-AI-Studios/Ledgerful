//! `ledgerful verify` command barrel (0251: `execute_verify` lives in `execute.rs`).
//!
//! Public path stays `crate::commands::verify`.

mod dto;
mod execute;
mod health;
mod mapping;
mod signatures;

pub use dto::{
    VERIFY_JSON_SCHEMA_VERSION, VerifyCliJson, VerifyCliStepJson, step_status_from_exit_code,
};
pub use execute::{ExecuteVerifyOpts, execute_verify};
pub use mapping::{TestMappingState, explain_test_mappings};
pub use signatures::{
    SigEntryClass, SigEntryStream, SignatureAggregateCounts, class_for_sig_entry,
    enumerate_invalid_ledger_entries, enumerate_invalid_ledger_entries_with_policy,
    format_signature_success_line_colored, sig_entry_stream, sig_exit, tally_signature_classes,
    verify_ledger_signatures, verify_ledger_signatures_with_options,
};

#[cfg(test)]
mod test_support;
