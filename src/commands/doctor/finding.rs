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

/// Warn counts split into action-critical vs optional (0209).
///
/// `total` equals [`summarize`].warn (all severity=warn). Info, including
/// `tool-gemini`, does not increment `optional`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct DoctorWarnSplit {
    pub total: u64,
    pub action: u64,
    pub optional: u64,
}

/// Split severity=warn findings into action-critical vs optional category.
pub(crate) fn split_doctor_warns(findings: &[DoctorFinding]) -> DoctorWarnSplit {
    let mut action = 0;
    let mut optional = 0;
    for f in findings {
        if f.severity != DoctorSeverity::Warn {
            continue;
        }
        if f.category == DoctorCategory::Optional {
            optional += 1;
        } else {
            action += 1;
        }
    }
    DoctorWarnSplit {
        total: action + optional,
        action,
        optional,
    }
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

    fn assert_warn_split(
        findings: &[DoctorFinding],
        expect_total: u64,
        expect_action: u64,
        expect_optional: u64,
    ) {
        let split = split_doctor_warns(findings);
        let summary = summarize(findings);
        assert_eq!(split.total, expect_total);
        assert_eq!(split.action, expect_action);
        assert_eq!(split.optional, expect_optional);
        assert_eq!(split.total, split.action + split.optional);
        assert_eq!(split.total, summary.warn);
        assert_eq!(summary.warn, split.action + split.optional);
    }

    /// 0209 DoD-1 six-row fixture: warn == warnAction + warnOptional == summarize().warn.
    #[test]
    fn split_doctor_warns_six_row_matrix() {
        let signing = [
            f("sig-pin", DoctorSeverity::Warn, DoctorCategory::Signing),
            f("sig-version", DoctorSeverity::Warn, DoctorCategory::Signing),
            f(
                "sig-pin-extra",
                DoctorSeverity::Warn,
                DoctorCategory::Signing,
            ),
        ];
        let optional_warn = f(
            "completion-unreachable",
            DoctorSeverity::Warn,
            DoctorCategory::Optional,
        );

        // Row 1: 3 signing warns + 1 optional warn → 4 / 3 / 1
        let mut row1 = signing.to_vec();
        row1.push(optional_warn.clone());
        assert_warn_split(&row1, 4, 3, 1);

        // Row 2: 3 signing warns only → 3 / 3 / 0
        assert_warn_split(&signing, 3, 3, 0);

        // Row 3: 1 optional warn only → 1 / 0 / 1
        assert_warn_split(std::slice::from_ref(&optional_warn), 1, 0, 1);

        // Row 4: 1 block + 2 signing warns + 1 optional → split 2 / 1 (block ignored)
        let row4 = vec![
            f("tool-git", DoctorSeverity::Block, DoctorCategory::Tools),
            signing[0].clone(),
            signing[1].clone(),
            optional_warn.clone(),
        ];
        assert_warn_split(&row4, 3, 2, 1);

        // Row 5: empty → 0 / 0 / 0
        assert_warn_split(&[], 0, 0, 0);

        // Row 6: info only (3) → 0 / 0 / 0
        let row6 = vec![
            f(
                "tool-gemini",
                DoctorSeverity::Info,
                DoctorCategory::Optional,
            ),
            f(
                "sccache-hint",
                DoctorSeverity::Info,
                DoctorCategory::Optional,
            ),
            f(
                "hook-template-stale",
                DoctorSeverity::Info,
                DoctorCategory::Gate,
            ),
        ];
        assert_warn_split(&row6, 0, 0, 0);
        assert_eq!(summarize(&row6).info, 3);
    }

    /// 0209-D: tool-gemini info/optional must not increment warnOptional.
    #[test]
    fn split_doctor_warns_info_tool_gemini_does_not_increment_optional() {
        let findings = vec![
            f(
                "tool-gemini",
                DoctorSeverity::Info,
                DoctorCategory::Optional,
            ),
            f(
                "completion-unreachable",
                DoctorSeverity::Warn,
                DoctorCategory::Optional,
            ),
            f(
                "sccache-hint",
                DoctorSeverity::Info,
                DoctorCategory::Optional,
            ),
        ];
        assert_warn_split(&findings, 1, 0, 1);
        assert_eq!(summarize(&findings).info, 2);
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
        // 0205: behind-latest is warn/tools (action-critical); ahead is info (not).
        assert!(is_action_critical(&f(
            "binary-behind-latest",
            DoctorSeverity::Warn,
            DoctorCategory::Tools,
        )));
        assert!(!is_action_critical(&f(
            "binary-ahead-of-latest",
            DoctorSeverity::Info,
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
