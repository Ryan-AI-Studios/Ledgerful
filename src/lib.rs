pub mod ai;
pub mod bridge;
pub(crate) mod change_evidence;
pub mod cli;
pub mod commands;
pub mod config;
pub mod contracts;
pub mod coverage;
pub mod docs;
pub(crate) mod doctor;
pub mod embed;
pub mod exec;
pub mod federated;
pub mod gemini;
pub mod git;
pub mod impact;
pub mod index;
pub mod ledger;
pub mod local_model;
pub mod observability;
pub mod output;
pub mod platform;
pub mod policy;
pub mod retrieval;
pub mod scip;
pub mod search;
pub mod semantic;
pub mod state;
pub(crate) mod surfaces;
pub mod ui;
pub mod util;
pub mod verify;
pub mod watch;

pub mod sync;

#[cfg(test)]
pub mod tests;

pub const BINARY_NAME: &str = "ledgerful";

#[cfg(feature = "export")]
pub mod export;

#[cfg(feature = "daemon")]
pub mod daemon;

/// Establish the optional Linux native producer before the binary starts
/// its application thread. A clean refusal leaves ordinary SQLite available.
///
/// # Safety
/// Only the reviewed executable entry point may call this, before any
/// SQLite initialization/configuration and before spawning other threads.
#[cfg(all(target_os = "linux", target_arch = "x86_64", target_env = "gnu"))]
#[doc(hidden)]
pub unsafe fn install_stock_evidence_sqlite_before_threads() -> Result<bool, &'static str> {
    // SAFETY: the entry point guarantees the exclusive pre-init boundary.
    // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
    unsafe { change_evidence::install_stock_writer_before_sqlite() }
}

/// Prepare the optional native passive web lane before worker startup.
///
/// # Safety
/// Only the executable entry point may call this after parsing `web start`
/// and before any SQLite configuration, initialization, or thread creation.
/// Background startup must run on the entry thread through daemonization.
#[cfg(all(
    target_os = "linux",
    target_arch = "x86_64",
    target_env = "gnu",
    feature = "web"
))]
#[doc(hidden)]
pub unsafe fn prepare_stock_passive_web_before_threads(
    background: bool,
) -> Result<(), &'static str> {
    // SAFETY: the executable owns the startup boundary.
    // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
    unsafe { change_evidence::passive_startup::prepare_before_threads(background) }
}
