use serde::{Deserialize, Serialize};

use crate::impact::redact::{DEFAULT_MAX_BYTES, sanitize_prompt};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum SignalSeverity {
    Normal,
    Warning,
    Critical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RiskElevation {
    None,
    Elevated,
    High,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ObservabilitySignal {
    pub signal_type: String,
    pub signal_label: String,
    pub value: f64,
    pub severity: SignalSeverity,
    pub excerpt: String,
    pub source: String,
}

impl Eq for ObservabilitySignal {}

impl PartialOrd for ObservabilitySignal {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for ObservabilitySignal {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        other
            .severity
            .cmp(&self.severity)
            .then_with(|| self.signal_type.cmp(&other.signal_type))
            .then_with(|| self.signal_label.cmp(&other.signal_label))
            .then_with(|| {
                self.value
                    .partial_cmp(&other.value)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
    }
}

impl ObservabilitySignal {
    pub fn new(
        signal_type: &str,
        signal_label: &str,
        value: f64,
        severity: SignalSeverity,
        excerpt: &str,
        source: &str,
    ) -> Self {
        let result = sanitize_prompt(excerpt, DEFAULT_MAX_BYTES);
        Self {
            signal_type: signal_type.to_string(),
            signal_label: signal_label.to_string(),
            value,
            severity,
            excerpt: result.sanitized,
            source: source.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_signal_severity_ordering() {
        assert!(SignalSeverity::Normal < SignalSeverity::Warning);
        assert!(SignalSeverity::Warning < SignalSeverity::Critical);
        assert!(SignalSeverity::Critical > SignalSeverity::Normal);
    }

    #[test]
    fn test_observability_signal_new_sanitizes_excerpt() {
        let signal = ObservabilitySignal::new(
            "error_rate",
            "svc",
            0.05,
            SignalSeverity::Warning,
            "api_key = \"sk-abcdefghijklmnopqrstuvwxyz123456\"",
            "prometheus",
        );
        assert!(!signal.excerpt.contains("sk-abcdefghijklmnopqrstuvwxyz"));
        assert!(signal.excerpt.contains("REDACTED"));
    }
}
