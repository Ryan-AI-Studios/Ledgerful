//! Structured doctor findings (0109).
//!
//! One [`DoctorFinding`] list drives human aggregate, `doctor --json`, exit code,
//! and `doctor-results.json`. Severity is never re-derived from coloured strings.

use serde::Serialize;

/// Severity grades for doctor findings (GitHub Actions-style mapping).
///
/// Wire form: `"block" | "warn" | "info"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DoctorSeverity {
    Block,
    Warn,
    Info,
}

impl DoctorSeverity {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Block => "block",
            Self::Warn => "warn",
            Self::Info => "info",
        }
    }
}

/// Category for dashboard failures formula and agent filtering.
///
/// Wire form: snake-ish lowercase labels matching the product table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DoctorCategory {
    Lifecycle,
    Signing,
    Tools,
    Index,
    Optional,
    Migration,
    Layout,
    Gate,
    Other,
}

impl DoctorCategory {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Lifecycle => "lifecycle",
            Self::Signing => "signing",
            Self::Tools => "tools",
            Self::Index => "index",
            Self::Optional => "optional",
            Self::Migration => "migration",
            Self::Layout => "layout",
            Self::Gate => "gate",
            Self::Other => "other",
        }
    }
}

/// A single doctor finding with stable machine identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DoctorFinding {
    pub code: String,
    pub severity: DoctorSeverity,
    pub category: DoctorCategory,
    /// Human message without ANSI colour codes.
    pub message: String,
}

impl DoctorFinding {
    pub fn new(
        code: impl Into<String>,
        severity: DoctorSeverity,
        category: DoctorCategory,
        message: impl Into<String>,
    ) -> Self {
        Self {
            code: code.into(),
            severity,
            category,
            message: message.into(),
        }
    }

    pub fn block(
        code: impl Into<String>,
        category: DoctorCategory,
        message: impl Into<String>,
    ) -> Self {
        Self::new(code, DoctorSeverity::Block, category, message)
    }

    pub fn warn(
        code: impl Into<String>,
        category: DoctorCategory,
        message: impl Into<String>,
    ) -> Self {
        Self::new(code, DoctorSeverity::Warn, category, message)
    }

    pub fn info(
        code: impl Into<String>,
        category: DoctorCategory,
        message: impl Into<String>,
    ) -> Self {
        Self::new(code, DoctorSeverity::Info, category, message)
    }
}

/// Aggregate counts by severity.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct DoctorSummary {
    pub block: u64,
    pub warn: u64,
    pub info: u64,
}

/// `true` iff zero findings have severity `block`.
pub fn ready_for_publish(findings: &[DoctorFinding]) -> bool {
    !findings.iter().any(|f| f.severity == DoctorSeverity::Block)
}

/// Count findings by severity.
pub fn summarize(findings: &[DoctorFinding]) -> DoctorSummary {
    let mut summary = DoctorSummary::default();
    for f in findings {
        match f.severity {
            DoctorSeverity::Block => summary.block += 1,
            DoctorSeverity::Warn => summary.warn += 1,
            DoctorSeverity::Info => summary.info += 1,
        }
    }
    summary
}

/// Dashboard health `failures` field (§2.3b):
/// `count(block) + count(warn WHERE category != Optional)`.
///
/// Optional-category findings never contribute, whether `info` or `warn`.
/// Info never contributes. Orthogonal to [`ready_for_publish`].
pub fn dashboard_failures(findings: &[DoctorFinding]) -> u64 {
    findings
        .iter()
        .filter(|f| match f.severity {
            DoctorSeverity::Block => true,
            DoctorSeverity::Warn => f.category != DoctorCategory::Optional,
            DoctorSeverity::Info => false,
        })
        .count() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(code: &str, severity: DoctorSeverity, category: DoctorCategory) -> DoctorFinding {
        DoctorFinding::new(code, severity, category, code)
    }

    #[test]
    fn ready_for_publish_true_when_no_blocks() {
        let findings = vec![
            f("sig-pin", DoctorSeverity::Warn, DoctorCategory::Signing),
            f(
                "embed-unreachable",
                DoctorSeverity::Warn,
                DoctorCategory::Optional,
            ),
            f(
                "tool-gemini",
                DoctorSeverity::Info,
                DoctorCategory::Optional,
            ),
        ];
        assert!(ready_for_publish(&findings));
    }

    #[test]
    fn ready_for_publish_false_on_block() {
        let findings = vec![
            f(
                "PROMOTE_ORPHAN",
                DoctorSeverity::Block,
                DoctorCategory::Lifecycle,
            ),
            f(
                "embed-not-configured",
                DoctorSeverity::Info,
                DoctorCategory::Optional,
            ),
        ];
        assert!(!ready_for_publish(&findings));
    }

    #[test]
    fn dashboard_failures_excludes_optional_warn_and_all_info() {
        let findings = vec![
            f(
                "PROMOTE_ORPHAN",
                DoctorSeverity::Block,
                DoctorCategory::Lifecycle,
            ),
            f(
                "search-corrupt",
                DoctorSeverity::Warn,
                DoctorCategory::Index,
            ),
            f(
                "embed-unreachable",
                DoctorSeverity::Warn,
                DoctorCategory::Optional,
            ),
            f(
                "embed-partial-config",
                DoctorSeverity::Warn,
                DoctorCategory::Optional,
            ),
            f(
                "tool-gemini",
                DoctorSeverity::Info,
                DoctorCategory::Optional,
            ),
            f(
                "graph-not-initialized",
                DoctorSeverity::Info,
                DoctorCategory::Index,
            ),
            f("sig-pin", DoctorSeverity::Warn, DoctorCategory::Signing),
        ];
        // block + search-corrupt + sig-pin = 3; optional warn/info excluded
        assert_eq!(dashboard_failures(&findings), 3);
    }

    #[test]
    fn dashboard_failures_git_missing_is_block() {
        let findings = vec![f("tool-git", DoctorSeverity::Block, DoctorCategory::Tools)];
        assert_eq!(dashboard_failures(&findings), 1);
        assert!(!ready_for_publish(&findings));
    }

    #[test]
    fn impact_corrupt_warn_counts_in_dashboard_not_ready() {
        let findings = vec![f(
            "impact-corrupt",
            DoctorSeverity::Warn,
            DoctorCategory::Index,
        )];
        assert!(ready_for_publish(&findings));
        assert_eq!(dashboard_failures(&findings), 1);
    }

    #[test]
    fn optional_backends_never_block_or_dashboard() {
        let findings = vec![
            f(
                "embed-not-configured",
                DoctorSeverity::Info,
                DoctorCategory::Optional,
            ),
            f(
                "completion-not-configured",
                DoctorSeverity::Info,
                DoctorCategory::Optional,
            ),
            f(
                "embed-unreachable",
                DoctorSeverity::Warn,
                DoctorCategory::Optional,
            ),
            f(
                "completion-unreachable",
                DoctorSeverity::Warn,
                DoctorCategory::Optional,
            ),
            f(
                "tool-gemini",
                DoctorSeverity::Info,
                DoctorCategory::Optional,
            ),
            f(
                "scip-rust-missing",
                DoctorSeverity::Info,
                DoctorCategory::Optional,
            ),
            f(
                "sccache-hint",
                DoctorSeverity::Info,
                DoctorCategory::Optional,
            ),
        ];
        assert!(ready_for_publish(&findings));
        assert_eq!(dashboard_failures(&findings), 0);
        let s = summarize(&findings);
        assert_eq!(s.block, 0);
        assert_eq!(s.warn, 2);
        assert_eq!(s.info, 5);
    }

    #[test]
    fn summarize_counts_all_severities() {
        let findings = vec![
            f("a", DoctorSeverity::Block, DoctorCategory::Lifecycle),
            f("b", DoctorSeverity::Block, DoctorCategory::Tools),
            f("c", DoctorSeverity::Warn, DoctorCategory::Index),
            f("d", DoctorSeverity::Info, DoctorCategory::Optional),
        ];
        assert_eq!(
            summarize(&findings),
            DoctorSummary {
                block: 2,
                warn: 1,
                info: 1
            }
        );
    }

    #[test]
    fn serde_severity_and_category_lowercase() {
        let finding = DoctorFinding::warn("sig-pin", DoctorCategory::Signing, "no keys pinned");
        let v = serde_json::to_value(&finding).expect("serialize");
        assert_eq!(v["severity"], "warn");
        assert_eq!(v["category"], "signing");
        assert_eq!(v["code"], "sig-pin");
        assert!(v.get("message").is_some());
    }
}
