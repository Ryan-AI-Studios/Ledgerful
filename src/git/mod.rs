pub mod classify;
pub mod commit;
pub mod diff;
pub mod ignore;
pub mod metadata;
pub mod repo;
pub mod status;

use miette::Diagnostic;
use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Error, Diagnostic)]
pub enum GitError {
    #[error("Failed to read .gitignore at {path}")]
    ReadIgnoreFailed {
        path: String,
        #[source]
        source: std::io::Error,
    },

    #[error("Failed to write .gitignore at {path}")]
    WriteIgnoreFailed {
        path: String,
        #[source]
        source: std::io::Error,
    },

    #[error("Failed to discover git repository at {path}")]
    #[diagnostic(help("Ensure the directory is within a git repository."))]
    RepoDiscoveryFailed {
        path: String,
        #[source]
        source: Box<gix::discover::Error>,
    },

    #[error("Failed to open git repository at {path}")]
    RepoOpenFailed {
        path: String,
        #[source]
        source: Box<gix::open::Error>,
    },

    #[error("Failed to get repository metadata")]
    MetadataError {
        #[from]
        source: anyhow::Error,
    },

    #[error("Shallow clone detected")]
    #[diagnostic(help(
        "Temporal analysis requires full history. Run `git fetch --unshallow` to fix."
    ))]
    ShallowClone,

    #[error(
        "Insufficient history for temporal analysis (found {found} commits, need at least {required})"
    )]
    InsufficientHistory { found: usize, required: usize },
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum ChangeType {
    Added,
    Modified,
    Deleted,
    Renamed { old_path: PathBuf },
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct FileChange {
    pub path: PathBuf,
    pub change_type: ChangeType,
    pub is_staged: bool,
}

#[derive(Debug, Clone)]
pub struct RepoSnapshot {
    pub head_hash: Option<String>,
    pub branch_name: Option<String>,
    pub is_clean: bool,
    pub changes: Vec<FileChange>,
}
