use miette::{IntoDiagnostic, Result};
use serde::Serialize;

/// Pretty-print a serializable value as JSON.
/// Stub implementation — full integration with `--format json` deferred (YAGNI).
pub fn format_json<T: Serialize>(value: &T) -> Result<String> {
    serde_json::to_string_pretty(value).into_diagnostic()
}
