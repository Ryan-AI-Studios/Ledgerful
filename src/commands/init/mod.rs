//! `ledgerful init` command: starter config, dual-brand git hooks, status print.
//!
//! Barrel split of the former `commands/init.rs` (0264). Public path stays
//! `crate::commands::init::execute_init`.

mod execute;
mod hooks;
mod print;

pub use execute::execute_init;

#[cfg(test)]
mod tests;
