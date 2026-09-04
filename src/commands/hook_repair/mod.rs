//! Repair already-installed git hooks that invoke the retired binary name
//! instead of the canonical `ledgerful` binary, and normalize legacy
//! idempotency marker comments so `init` recognises its own blocks.
//!
//! `ledgerful init` (see `src/commands/init.rs`) already writes hooks that
//! call `ledgerful`. This module fixes up hooks that were installed by an
//! older version of `init` (or hand-written).
//!
//! Replacements cover:
//! - Exact command invocations (`command -v`, `ledger`, `verify`, `scan`,
//!   `internal hook-`)
//! - Legacy marker comments (`# <legacy>-ledger-gate` etc.) so a subsequent
//!   `init` upgrades in place instead of appending a duplicate block (0094)
//!
//! Two-tier de-duplication when both legacy and current markers are present:
//! - Tier 1: exact match of a known generated block → auto-remove
//! - Tier 2: marker-bounded block with only recognised invocations → report
//!   with text, never auto-delete
//!
//! Discovery honours `core.hooksPath` and linked-worktree `commondir`; a
//! hooks directory outside the repository is reported and never rewritten.
//! Third-party manager detection re-runs against the resolved hooks path.

mod detect;
mod doctor;
mod resolve;
mod rewrite;

pub use detect::{
    ThirdPartyHookManager, detect_third_party_at_hooks_dir, detect_third_party_hook_manager,
};
pub use doctor::doctor_legacy_hook_findings;
pub use resolve::{HooksDirResolution, resolve_hooks_dir};
#[cfg(test)]
pub(crate) use rewrite::LEGACY_BINARY;
pub use rewrite::{HookRepairReport, execute_hook_repair, repair_hooks_at};
pub(crate) use rewrite::{
    contains_legacy_gate_marker, contains_legacy_gate_suffix, repair_content,
};

#[cfg(test)]
mod tests;
