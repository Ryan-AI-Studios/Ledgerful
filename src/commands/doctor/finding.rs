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
    /// Optional multi-step copy-paste remediation (exact CLI lines).
    /// Machine source of truth when present; omitted from JSON when `None`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remediation: Option<String>,
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
            remediation: None,
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

    /// Attach a multi-step remediation block (exact CLI lines).
    pub fn with_remediation(mut self, remediation: impl Into<String>) -> Self {
        self.remediation = Some(remediation.into());
        self
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

/// Action-critical findings for dashboard failures, sidecar top-N, and human
/// progressive disclosure Index Health expand list (0138 / 0174).
///
/// Block always; warn when category != Optional; info never.
pub(crate) fn is_action_critical(f: &DoctorFinding) -> bool {
    match f.severity {
        DoctorSeverity::Block => true,
        DoctorSeverity::Warn => f.category != DoctorCategory::Optional,
        DoctorSeverity::Info => false,
    }
}

/// Hygiene findings collapse by default in human doctor output (0174 3-tier).
///
/// Equivalent to `!is_action_critical`: Optional category (any severity) or
/// Info severity (any category). Block is never hygiene.
pub(crate) fn is_hygiene(f: &DoctorFinding) -> bool {
    !is_action_critical(f)
}

/// Dashboard health `failures` field (§2.3b):
/// `count(block) + count(warn WHERE category != Optional)`.
///
/// Optional-category findings never contribute, whether `info` or `warn`.
/// Info never contributes. Orthogonal to [`ready_for_publish`].
/// Shares eligibility with sidecar top-N via [`is_action_critical`].
pub fn dashboard_failures(findings: &[DoctorFinding]) -> u64 {
    findings.iter().filter(|f| is_action_critical(f)).count() as u64
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
    fn is_action_critical_matrix() {
        // Optional warn is ambient — not action-critical (0138 topFindings hygiene).
        assert!(!is_action_critical(&f(
            "completion-unreachable",
            DoctorSeverity::Warn,
            DoctorCategory::Optional,
        )));
        // Optional block still surfaces (severity-first B1; should not exist product-wise).
        assert!(is_action_critical(&f(
            "optional-block",
            DoctorSeverity::Block,
            DoctorCategory::Optional,
        )));
        // Non-optional signing warn remains action-critical.
        assert!(is_action_critical(&f(
            "sig-pin",
            DoctorSeverity::Warn,
            DoctorCategory::Signing,
        )));
        // Tools non-optional (e.g. binary-behind-tree) remains eligible.
        assert!(is_action_critical(&f(
            "binary-behind-tree",
            DoctorSeverity::Warn,
            DoctorCategory::Tools,
        )));
        // Info never action-critical.
        assert!(!is_action_critical(&f(
            "sccache-hint",
            DoctorSeverity::Info,
            DoctorCategory::Optional,
        )));
        assert!(!is_action_critical(&f(
            "hook-template-stale",
            DoctorSeverity::Info,
            DoctorCategory::Gate,
        )));
    }

    /// 0174 T1–T4: 3-tier classification (expand vs hygiene collapse).
    #[test]
    fn is_hygiene_three_tier_matrix() {
        // T1: optional warn → hygiene (collapsed default).
        let optional_warn = f(
            "completion-unreachable",
            DoctorSeverity::Warn,
            DoctorCategory::Optional,
        );
        assert!(is_hygiene(&optional_warn));
        assert!(!is_action_critical(&optional_warn));

        // T2: info any category → hygiene (hook-template-stale pin).
        let hook_stale = f(
            "hook-template-stale",
            DoctorSeverity::Info,
            DoctorCategory::Gate,
        );
        assert!(is_hygiene(&hook_stale));
        assert!(!is_action_critical(&hook_stale));
        let info_optional = f(
            "sccache-hint",
            DoctorSeverity::Info,
            DoctorCategory::Optional,
        );
        assert!(is_hygiene(&info_optional));
        let surfaces_gated = f(
            "surfaces-gated",
            DoctorSeverity::Info,
            DoctorCategory::Optional,
        );
        assert!(is_hygiene(&surfaces_gated));
        assert!(!is_action_critical(&surfaces_gated));

        // T3: action-critical warn expanded (sig-pin).
        let sig_pin = f("sig-pin", DoctorSeverity::Warn, DoctorCategory::Signing);
        assert!(!is_hygiene(&sig_pin));
        assert!(is_action_critical(&sig_pin));

        // T4: block always expanded (never hygiene).
        let block = f("tool-git", DoctorSeverity::Block, DoctorCategory::Tools);
        assert!(!is_hygiene(&block));
        assert!(is_action_critical(&block));
        // Block remains action-critical even under Optional category.
        let opt_block = f(
            "optional-block",
            DoctorSeverity::Block,
            DoctorCategory::Optional,
        );
        assert!(!is_hygiene(&opt_block));
        assert!(is_action_critical(&opt_block));
    }

    #[test]
    fn serde_severity_and_category_lowercase() {
        let finding = DoctorFinding::warn("sig-pin", DoctorCategory::Signing, "no keys pinned");
        let v = serde_json::to_value(&finding).expect("serialize");
        assert_eq!(v["severity"], "warn");
        assert_eq!(v["category"], "signing");
        assert_eq!(v["code"], "sig-pin");
        assert!(v.get("message").is_some());
        // remediation omitted when None (skip_serializing_if)
        assert!(v.get("remediation").is_none());
    }

    #[test]
    fn serde_remediation_present_when_some() {
        let finding = DoctorFinding::warn("sig-pin", DoctorCategory::Signing, "msg")
            .with_remediation("ledgerful doctor --json");
        let v = serde_json::to_value(&finding).expect("serialize");
        assert_eq!(v["remediation"], "ledgerful doctor --json");
    }
}
