//! `ledgerful init` command: starter config, dual-brand git hooks, status print.
//!
//! Barrel split of the former `commands/init.rs` (0264). Public path stays
//! `crate::commands::init::execute_init`.

mod execute;
mod hooks;
mod pack;
mod print;

pub use execute::{execute_init, execute_init_with};

#[cfg(test)]
mod tests;
