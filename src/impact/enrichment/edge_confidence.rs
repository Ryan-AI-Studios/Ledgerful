//! Shared edge confidence classifier (track 0117).
//!
//! Product tier class is a pure function of `resolution_status` + `evidence`.
//! Used by blast hop expansion, pair-collapse priority, summaries, and human print.
//! Does not invent a fourth independent score beyond status/evidence/float.

use serde::{Deserialize, Serialize};

/// Product confidence class for a structural edge (SCREAMING_SNAKE in JSON).
///
/// Derived from binding status + evidence path — not CRG EXTRACTED/INFERRED rename.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ConfidenceClass {
    /// `evidence` starts with `scip:` (checked before status).
    ScipBound,
    /// status == RESOLVED and not SCIP — unique local candidate heuristic floor.
    Resolved,
    /// status == AMBIGUOUS — never expand; production blast hop-1 count is 0
    /// (null callee by construction).
    Ambiguous,
    /// status == UNRESOLVED — null-callee by construction; not on blast hop-1.
    Unresolved,
    /// status == CAPPED — resolution budget; do not expand.
    Capped,
    /// Anything else — fail-soft; never promote to SCIP_BOUND / RESOLVED.
    /// Also the serde default for pre-0117 snapshots missing `confidenceClass`
    /// (hydrate from status+evidence after load when possible).
    #[default]
    Unknown,
}

impl ConfidenceClass {
    /// Pair-collapse priority (higher wins). Bit-identical to pre-0117
    /// `confidence_priority`: SCIP_BOUND=4, RESOLVED=3, AMBIGUOUS=2,
    /// UNRESOLVED=1, CAPPED=0, UNKNOWN=0.
    pub fn collapse_priority(self) -> u8 {
        match self {
            ConfidenceClass::ScipBound => 4,
            ConfidenceClass::Resolved => 3,
            ConfidenceClass::Ambiguous => 2,
            ConfidenceClass::Unresolved => 1,
            ConfidenceClass::Capped => 0,
            ConfidenceClass::Unknown => 0,
        }
    }

    /// SCREAMING_SNAKE product label (matches JSON serde).
    pub fn as_str(self) -> &'static str {
        match self {
            ConfidenceClass::ScipBound => "SCIP_BOUND",
            ConfidenceClass::Resolved => "RESOLVED",
            ConfidenceClass::Ambiguous => "AMBIGUOUS",
            ConfidenceClass::Unresolved => "UNRESOLVED",
            ConfidenceClass::Capped => "CAPPED",
            ConfidenceClass::Unknown => "UNKNOWN",
        }
    }
}

impl std::fmt::Display for ConfidenceClass {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Deterministic product class from status + evidence (scip: prefix wins).
pub fn confidence_class(resolution_status: &str, evidence: &str) -> ConfidenceClass {
    if evidence.starts_with("scip:") {
        return ConfidenceClass::ScipBound;
    }
    match resolution_status {
        "RESOLVED" => ConfidenceClass::Resolved,
        "AMBIGUOUS" => ConfidenceClass::Ambiguous,
        "UNRESOLVED" => ConfidenceClass::Unresolved,
        "CAPPED" => ConfidenceClass::Capped,
        _ => ConfidenceClass::Unknown,
    }
}

/// High-confidence discovery/expansion edge (hop N+1 seed).
pub fn is_high_confidence(resolution_status: &str, evidence: &str) -> bool {
    matches!(
        confidence_class(resolution_status, evidence),
        ConfidenceClass::ScipBound | ConfidenceClass::Resolved
    )
}

/// Aggregate class counts for blast / change-context summaries.
///
/// JSON camelCase: `scipBound`, `resolved`, `ambiguous`, …
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EdgeConfidenceSummary {
    pub scip_bound: usize,
    pub resolved: usize,
    pub ambiguous: usize,
    pub unresolved: usize,
    pub capped: usize,
    pub unknown: usize,
    pub expandable: usize,
    pub total: usize,
}

impl EdgeConfidenceSummary {
    /// Count classes from `(resolution_status, evidence, expandable)` triples.
    pub fn from_edge_fields<'a>(edges: impl IntoIterator<Item = (&'a str, &'a str, bool)>) -> Self {
        let mut summary = Self::default();
        for (status, evidence, expandable) in edges {
            summary.total = summary.total.saturating_add(1);
            if expandable {
                summary.expandable = summary.expandable.saturating_add(1);
            }
            match confidence_class(status, evidence) {
                ConfidenceClass::ScipBound => {
                    summary.scip_bound = summary.scip_bound.saturating_add(1);
                }
                ConfidenceClass::Resolved => {
                    summary.resolved = summary.resolved.saturating_add(1);
                }
                ConfidenceClass::Ambiguous => {
                    summary.ambiguous = summary.ambiguous.saturating_add(1);
                }
                ConfidenceClass::Unresolved => {
                    summary.unresolved = summary.unresolved.saturating_add(1);
                }
                ConfidenceClass::Capped => {
                    summary.capped = summary.capped.saturating_add(1);
                }
                ConfidenceClass::Unknown => {
                    summary.unknown = summary.unknown.saturating_add(1);
                }
            }
        }
        summary
    }

    /// Count from finalized blast edges (post-collapse).
    pub fn from_blast_edges(edges: &[crate::impact::packet::BlastEdge]) -> Self {
        Self::from_edge_fields(edges.iter().map(|e| {
            (
                e.resolution_status.as_str(),
                e.evidence.as_str(),
                e.expandable,
            )
        }))
    }

    /// True when every count is zero (empty blast edges).
    pub fn is_zero(&self) -> bool {
        *self == Self::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edge_confidence_class_table() {
        assert_eq!(
            confidence_class("RESOLVED", "scip:ref"),
            ConfidenceClass::ScipBound
        );
        assert_eq!(
            confidence_class("AMBIGUOUS", "scip:ref"),
            ConfidenceClass::ScipBound
        );
        assert_eq!(
            confidence_class("RESOLVED", "call_expr"),
            ConfidenceClass::Resolved
        );
        assert_eq!(
            confidence_class("AMBIGUOUS", "call_expr"),
            ConfidenceClass::Ambiguous
        );
        assert_eq!(
            confidence_class("UNRESOLVED", ""),
            ConfidenceClass::Unresolved
        );
        assert_eq!(
            confidence_class("CAPPED", "call_expr"),
            ConfidenceClass::Capped
        );
        assert_eq!(
            confidence_class("WEIRD", "call_expr"),
            ConfidenceClass::Unknown
        );
        assert_eq!(confidence_class("", ""), ConfidenceClass::Unknown);
    }

    #[test]
    fn edge_confidence_high_confidence_truth_table() {
        assert!(is_high_confidence("RESOLVED", "call_expr"));
        assert!(is_high_confidence("RESOLVED", "scip:ref"));
        assert!(is_high_confidence("AMBIGUOUS", "scip:ref"));
        assert!(is_high_confidence("UNRESOLVED", "scip:ref"));
        assert!(!is_high_confidence("AMBIGUOUS", "call_expr"));
        assert!(!is_high_confidence("UNRESOLVED", ""));
        assert!(!is_high_confidence("CAPPED", "call_expr"));
        assert!(!is_high_confidence("WEIRD", "call_expr"));
    }

    #[test]
    fn edge_confidence_collapse_priority_values() {
        assert_eq!(ConfidenceClass::ScipBound.collapse_priority(), 4);
        assert_eq!(ConfidenceClass::Resolved.collapse_priority(), 3);
        assert_eq!(ConfidenceClass::Ambiguous.collapse_priority(), 2);
        assert_eq!(ConfidenceClass::Unresolved.collapse_priority(), 1);
        assert_eq!(ConfidenceClass::Capped.collapse_priority(), 0);
        assert_eq!(ConfidenceClass::Unknown.collapse_priority(), 0);
    }

    #[test]
    fn edge_confidence_scip_wins_class_and_priority_over_resolved() {
        let scip = confidence_class("RESOLVED", "scip:ref");
        let bare = confidence_class("RESOLVED", "call_expr");
        assert_eq!(scip, ConfidenceClass::ScipBound);
        assert_eq!(bare, ConfidenceClass::Resolved);
        assert!(scip.collapse_priority() > bare.collapse_priority());
    }

    #[test]
    fn edge_confidence_summary_from_fields() {
        let summary = EdgeConfidenceSummary::from_edge_fields([
            ("RESOLVED", "scip:ref", true),
            ("RESOLVED", "call_expr", true),
            ("AMBIGUOUS", "call_expr", false),
            ("CAPPED", "call_expr", false),
            ("WEIRD", "x", false),
        ]);
        assert_eq!(summary.total, 5);
        assert_eq!(summary.scip_bound, 1);
        assert_eq!(summary.resolved, 1);
        assert_eq!(summary.ambiguous, 1);
        assert_eq!(summary.capped, 1);
        assert_eq!(summary.unknown, 1);
        assert_eq!(summary.unresolved, 0);
        assert_eq!(summary.expandable, 2);
    }

    #[test]
    fn edge_confidence_class_serde_screaming_snake() {
        let json = serde_json::to_string(&ConfidenceClass::ScipBound).unwrap();
        assert_eq!(json, "\"SCIP_BOUND\"");
        let back: ConfidenceClass = serde_json::from_str(&json).unwrap();
        assert_eq!(back, ConfidenceClass::ScipBound);
    }

    #[test]
    fn edge_confidence_summary_serde_camel_case() {
        let summary = EdgeConfidenceSummary {
            scip_bound: 1,
            resolved: 2,
            ambiguous: 0,
            unresolved: 0,
            capped: 0,
            unknown: 0,
            expandable: 3,
            total: 3,
        };
        let v = serde_json::to_value(summary).unwrap();
        assert_eq!(v["scipBound"], 1);
        assert_eq!(v["resolved"], 2);
        assert_eq!(v["expandable"], 3);
        assert_eq!(v["total"], 3);
        assert!(v.get("scip_bound").is_none());
    }
}
