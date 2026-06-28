use miette::Diagnostic;
use thiserror::Error;

#[derive(Error, Debug, Diagnostic)]
pub enum SyncError {
    #[error("Invalid HLC format: {0}")]
    #[diagnostic(
        code(sync::invalid_hlc),
        help("HLC format must be <physical_ms>-<logical:04>-<node_id>")
    )]
    InvalidHLC(String),

    #[error("IO error: {0}")]
    #[diagnostic(code(sync::io_error))]
    Io(#[from] std::io::Error),

    #[error("Database error: {0}")]
    #[diagnostic(code(sync::db_error))]
    Sqlite(#[from] rusqlite::Error),

    #[error("Unsupported sync target: {0}")]
    #[diagnostic(
        code(sync::unsupported_target),
        help("Sync target must start with a supported scheme like dir://")
    )]
    UnsupportedTarget(String),

    #[error("{0}")]
    #[diagnostic(code(sync::other))]
    Other(String),

    #[error("No new entries to extract")]
    #[diagnostic(code(sync::no_new_entries))]
    NoNewEntries,
}
