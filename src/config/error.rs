use miette::Diagnostic;
use thiserror::Error;

#[derive(Debug, Error, Diagnostic)]
pub enum ConfigError {
    #[error("Failed to write config file at {path}")]
    WriteFailed {
        path: String,
        #[source]
        source: std::io::Error,
    },

    #[error("Failed to read config file at {path}")]
    ReadFailed {
        path: String,
        #[source]
        source: std::io::Error,
    },

    #[error("Failed to parse config TOML")]
    ParseFailed {
        #[source]
        source: crate::config::model::TomlError,
    },

    #[error("Invalid configuration: {reason}")]
    ValidationFailed { reason: String },

    #[error(
        "atomic no-clobber publication is unavailable for {path}; \
         config creation was refused safely"
    )]
    AtomicPublicationUnavailable { path: String },
}
