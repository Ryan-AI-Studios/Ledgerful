pub mod ask;
pub mod ask_routing;
pub mod bridge;
pub mod config;
pub mod config_verify;
#[cfg(feature = "daemon")]
pub mod daemon;
pub mod data_models;
pub mod dead_code;
pub mod dependencies;
pub mod deploy;
pub mod doctor;
pub mod dx1_templates;
pub mod endpoints;
pub mod federate;
pub mod helpers;
pub mod hook_commit_msg;
pub mod hook_post_commit;
pub mod hook_repair;
pub mod hotspots;
pub mod impact;
pub mod index;
pub mod init;
pub mod intent;
pub mod ledger;
pub mod ledger_adr;
pub mod ledger_audit;
pub mod ledger_graph;
pub mod ledger_re_sign;
pub mod ledger_register;
pub mod ledger_search;
pub mod ledger_stack;
#[cfg(feature = "mcp")]
pub mod mcp;
pub mod observability;
#[cfg(any(feature = "viz-server", feature = "web"))]
pub mod pid;
pub mod reset;
pub mod scan;
pub mod schedule;
pub mod search;
pub mod security;
pub mod services_diff;
pub mod setup;
#[cfg(feature = "sync")]
pub mod sync;
pub mod test_mapping;
pub mod update;
#[cfg(feature = "usage-metrics")]
pub mod usage;
pub mod verify;
pub mod viz;
#[cfg(feature = "viz-server")]
pub mod viz_server;
pub mod watch;
#[cfg(feature = "web")]
pub mod web;

use miette::Diagnostic;
use thiserror::Error;

#[derive(Debug, Error, Diagnostic)]
pub enum CommandError {
    #[error("Failed to discover repository root")]
    RepoDiscoveryFailed,

    #[error("I/O error during command execution")]
    IoError(#[from] std::io::Error),

    #[error("Verification failed: {0}")]
    Verify(String),
}
