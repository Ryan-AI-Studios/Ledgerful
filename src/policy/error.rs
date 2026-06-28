use miette::Diagnostic;
use thiserror::Error;

#[derive(Debug, Error, Diagnostic)]
pub enum PolicyError {
    #[error("Failed to read rules file at {path}")]
    ReadFailed {
        path: String,
        #[source]
        source: std::io::Error,
    },

    #[error("Failed to parse rules TOML")]
    ParseFailed {
        #[source]
        source: toml::de::Error,
    },

    #[error("Invalid glob pattern: {pattern}")]
    InvalidPattern {
        pattern: String,
        #[source]
        source: globset::Error,
    },

    #[error("Invalid policy: {reason}")]
    ValidationFailed { reason: String },
}
