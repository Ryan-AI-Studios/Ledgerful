pub mod ai;
pub mod bridge;
pub mod cli;
pub mod commands;
pub mod config;
pub mod contracts;
pub mod coverage;
pub mod docs;
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
pub mod ui;
pub mod util;
pub mod verify;
pub mod watch;

pub mod sync;

pub const BINARY_NAME: &str = "ledgerful";

#[cfg(feature = "export")]
pub mod export;

#[cfg(feature = "daemon")]
pub mod daemon;
