use crate::index::observability::{ErrorHandlingPattern, LoggingPattern, TelemetryPattern};
use miette::Result;

pub mod calls;
pub mod common;
pub mod models;
pub mod observability;
pub mod routes;
pub mod symbols;

pub use calls::extract_calls;
pub use models::extract_data_models;
pub use routes::extract_routes;
pub use symbols::extract_symbols;

pub fn extract_logging_patterns(content: &str) -> Result<Vec<LoggingPattern>> {
    let (telemetry, _) = observability::extract_observability(content, &[])?;
    Ok(telemetry
        .into_iter()
        .map(|t| LoggingPattern {
            line_start: t.line_start,
            level: t.level,
            framework: t.framework,
            in_test: t.in_test,
            confidence: t.confidence,
            evidence: t.evidence,
        })
        .collect())
}

pub fn extract_error_handling(content: &str) -> Result<Vec<ErrorHandlingPattern>> {
    let (_, errors) = observability::extract_observability(content, &[])?;
    Ok(errors)
}

pub fn extract_telemetry_patterns(content: &str) -> Result<Vec<TelemetryPattern>> {
    let (telemetry, _) = observability::extract_observability(content, &[])?;
    Ok(telemetry)
}
