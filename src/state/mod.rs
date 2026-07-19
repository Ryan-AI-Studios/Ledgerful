pub mod cozo;
pub mod delta;
pub mod graph_kinds;
pub mod graph_traversal;
pub mod layout;
pub mod locks;
pub mod migration;
pub mod migration_prompt;
pub mod migrations;
pub mod reports;
pub mod rollup;
pub mod storage;
pub mod storage_cozo;

use miette::Diagnostic;
use thiserror::Error;

#[derive(Debug, Error, Diagnostic)]
pub enum StateError {
    #[error("Failed to create directory: {path}")]
    #[diagnostic(
        code(state::mkdir_failed),
        help("Check directory permissions at {path}")
    )]
    MkdirFailed {
        path: String,
        #[source]
        source: std::io::Error,
    },

    #[error("Failed to migrate Ledgerful state from {old_path} to {new_path}")]
    #[diagnostic(
        code(state::legacy_migration_failed),
        help(
            "Close running Ledgerful processes, then retry. The existing state was left untouched."
        )
    )]
    LegacyMigrationFailed {
        old_path: String,
        new_path: String,
        #[source]
        source: std::io::Error,
    },

    #[error("Failed to resolve base directory")]
    #[diagnostic(code(state::resolve_base_failed))]
    ResolveBaseFailed,

    #[error("Failed to write report: {path}")]
    #[diagnostic(
        code(state::write_report_failed),
        help("Check directory permissions at {path}")
    )]
    WriteReportFailed {
        path: String,
        #[source]
        source: std::io::Error,
    },

    #[error("Storage error: {0}")]
    Storage(String),

    #[error("Schema mismatch detected.")]
    #[diagnostic(
        code(state::schema_mismatch),
        help("Run 'ledgerful update --migrate' to upgrade the database schema.")
    )]
    SchemaMismatch,
}
