use miette::Diagnostic;
use thiserror::Error;

pub mod batch;
pub mod debounce;
pub mod filters;
pub mod normalize;

#[derive(Error, Debug, Diagnostic)]
pub enum WatchError {
    #[error("Failed to compile glob pattern")]
    #[diagnostic(code(watch::glob_error))]
    GlobError(#[from] globset::Error),

    #[error("Watcher error: {0}")]
    #[diagnostic(code(watch::notify_error))]
    NotifyError(String),

    #[error("IO error: {0}")]
    #[diagnostic(code(watch::io_error))]
    IoError(#[from] std::io::Error),
}
