//! `ledgerful scan` command: validate flags, git name-status, execute, tests.
//!
//! Barrel split of the former `commands/scan.rs` (0249). Public path stays
//! `crate::commands::scan`.

mod execute;
mod git;
mod validate;

pub(crate) use execute::{compute_pr_scan_affected_flows, compute_pr_scan_test_gaps};
pub use execute::{execute_scan, execute_scan_with_blast_depth, execute_scan_with_opts};
pub(crate) use git::{
    files_changed_between, files_changed_since, parse_pr_range, resolve_commit_oid,
};

#[cfg(test)]
mod tests;
