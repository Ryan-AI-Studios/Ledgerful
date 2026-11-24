//! Pure delta engine for analysis baselines (Track 0601).
//!
//! No I/O. Incomparable results carry no counts and no items; `"none"` is only
//! reachable from the comparable, both-sides-complete, no-growth path.

use super::identity::{BASELINE_SCHEMA, BaselineIdentity, EdgeCoverage, FileEdgeSet, PAYLOAD_KIND};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Per-edge classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "web", derive(utoipa::ToSchema))]
#[serde(rename_all = "lowercase")]
pub(crate) enum DeltaClass {
    Added,
    Removed,
    Worsened,
    Improved,
    Unchanged,
}

/// Why a comparison is not comparable. Emitted sorted by token.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "web", derive(utoipa::ToSchema))]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub(crate) enum IncomparableReason {
    BaselineDigestMismatch,
    BaselineIncomplete,
    CurrentIncomplete,
    EdgeSourcesMismatch,
    ParserVersionMismatch,
    SchemaMismatch,
    ScopeMismatch,
}

impl IncomparableReason {
    pub(crate) fn token(self) -> &'static str {
        match self {
            Self::BaselineDigestMismatch => "BASELINE_DIGEST_MISMATCH",
            Self::BaselineIncomplete => "BASELINE_INCOMPLETE",
            Self::CurrentIncomplete => "CURRENT_INCOMPLETE",
            Self::EdgeSourcesMismatch => "EDGE_SOURCES_MISMATCH",
            Self::ParserVersionMismatch => "PARSER_VERSION_MISMATCH",
            Self::SchemaMismatch => "SCHEMA_MISMATCH",
            Self::ScopeMismatch => "SCOPE_MISMATCH",
        }
    }
}

/// Three-valued regression verdict. Never a boolean.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "web", derive(utoipa::ToSchema))]
#[serde(rename_all = "lowercase")]
pub(crate) enum Regression {
    None,
    Present,
    Unknown,
}

impl Regression {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Present => "present",
            Self::Unknown => "unknown",
        }
    }
}

/// Comparability of the two sides.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Comparability {
    Comparable,
    Incomparable(Vec<IncomparableReason>),
}

impl Comparability {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Comparable => "comparable",
            Self::Incomparable(_) => "incomparable",
        }
    }

    pub(crate) fn reasons(&self) -> Vec<String> {
        match self {
            Self::Comparable => Vec::new(),
            Self::Incomparable(reasons) => {
                let mut tokens: Vec<String> =
                    reasons.iter().map(|r| r.token().to_string()).collect();
                tokens.sort();
                tokens.dedup();
                tokens
            }
        }
    }
}

/// Stable per-item identity: the normalized path pair.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "web", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub(crate) struct EdgeKey {
    pub src_path: String,
    pub dst_path: String,
}

/// One changed (or, with `--all`, unchanged) edge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "web", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub(crate) struct DeltaItem {
    pub edge_key: EdgeKey,
    pub class: DeltaClass,
    pub baseline_weight: Option<u32>,
    pub current_weight: Option<u32>,
}

/// Per-class counts. Sums to the size of the union.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "web", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub(crate) struct DeltaCounts {
    pub added: u64,
    pub removed: u64,
    pub worsened: u64,
    pub improved: u64,
    pub unchanged: u64,
}

/// Full delta result. `counts`/`items` are `None` iff incomparable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EdgeSetDelta {
    pub status: Comparability,
    pub regression: Regression,
    pub regression_reasons: Vec<String>,
    pub notes: Vec<String>,
    pub counts: Option<DeltaCounts>,
    pub items: Option<Vec<DeltaItem>>,
}

/// Compare two extracted edge sets.
pub(crate) fn compare_edge_sets(baseline: &FileEdgeSet, current: &FileEdgeSet) -> EdgeSetDelta {
    compare_edge_sets_with(baseline, current, false)
}

/// Compare with an explicit choice about emitting `unchanged` items.
pub(crate) fn compare_edge_sets_with(
    baseline: &FileEdgeSet,
    current: &FileEdgeSet,
    include_unchanged: bool,
) -> EdgeSetDelta {
    let mut reasons = gate_reasons(Some(&baseline.identity), &current.identity);
    reasons.sort();
    reasons.dedup();

    if !reasons.is_empty() {
        return EdgeSetDelta {
            status: Comparability::Incomparable(reasons),
            regression: Regression::Unknown,
            regression_reasons: Vec::new(),
            notes: analyzer_notes(baseline, current),
            counts: None,
            items: None,
        };
    }

    let baseline_map: BTreeMap<EdgeKey, u32> = baseline
        .edges
        .iter()
        .map(|e| {
            (
                EdgeKey {
                    src_path: e.src_path.clone(),
                    dst_path: e.dst_path.clone(),
                },
                e.weight,
            )
        })
        .collect();
    let current_map: BTreeMap<EdgeKey, u32> = current
        .edges
        .iter()
        .map(|e| {
            (
                EdgeKey {
                    src_path: e.src_path.clone(),
                    dst_path: e.dst_path.clone(),
                },
                e.weight,
            )
        })
        .collect();

    let mut counts = DeltaCounts::default();
    let mut items = Vec::new();
    let mut union: Vec<&EdgeKey> = baseline_map.keys().chain(current_map.keys()).collect();
    union.sort();
    union.dedup();
    let union_len = union.len();

    for key in &union {
        let key = *key;
        let before = baseline_map.get(key).copied();
        let after = current_map.get(key).copied();
        let class = match (before, after) {
            (None, Some(_)) => DeltaClass::Added,
            (Some(_), None) => DeltaClass::Removed,
            (Some(b), Some(c)) if c > b => DeltaClass::Worsened,
            (Some(b), Some(c)) if c < b => DeltaClass::Improved,
            (Some(_), Some(_)) => DeltaClass::Unchanged,
            (None, None) => continue,
        };
        match class {
            DeltaClass::Added => counts.added += 1,
            DeltaClass::Removed => counts.removed += 1,
            DeltaClass::Worsened => counts.worsened += 1,
            DeltaClass::Improved => counts.improved += 1,
            DeltaClass::Unchanged => counts.unchanged += 1,
        }
        if include_unchanged || class != DeltaClass::Unchanged {
            items.push(DeltaItem {
                edge_key: (*key).clone(),
                class,
                baseline_weight: before,
                current_weight: after,
            });
        }
    }

    let classified =
        counts.added + counts.removed + counts.worsened + counts.improved + counts.unchanged;
    debug_assert_eq!(
        u64::try_from(union_len).unwrap_or(u64::MAX),
        classified,
        "every edge in the union must be classified exactly once"
    );

    let (regression, regression_reasons) =
        regression_for(&counts, &baseline.coverage, &current.coverage);

    let mut notes = analyzer_notes(baseline, current);
    if counts.removed > 0 {
        let current_coverage = u128::from(current.coverage.edges_unresolved)
            + u128::from(current.coverage.edges_ambiguous);
        let baseline_coverage = u128::from(baseline.coverage.edges_unresolved)
            + u128::from(baseline.coverage.edges_ambiguous);
        if current_coverage >= baseline_coverage {
            notes.push("RESOLVED_EDGES_REMOVED".to_string());
        }
    }
    notes.sort();
    notes.dedup();

    EdgeSetDelta {
        status: Comparability::Comparable,
        regression,
        regression_reasons,
        notes,
        counts: Some(counts),
        items: Some(items),
    }
}

/// Apply the authoritative coverage and class-count regression rule.
pub(crate) fn regression_for(
    counts: &DeltaCounts,
    baseline: &EdgeCoverage,
    current: &EdgeCoverage,
) -> (Regression, Vec<String>) {
    if counts.added > 0 || counts.worsened > 0 {
        return (Regression::Present, Vec::new());
    }
    let mut reasons = Vec::new();
    // Compare the counters separately; a net-zero sum can hide ambiguity growth.
    if current.edges_ambiguous > baseline.edges_ambiguous {
        reasons.push("AMBIGUOUS_GREW".to_string());
    }
    if current.edges_unresolved > baseline.edges_unresolved {
        reasons.push("UNRESOLVED_GREW".to_string());
    }
    let regression = if reasons.is_empty() {
        Regression::None
    } else {
        Regression::Unknown
    };
    (regression, reasons)
}

fn analyzer_notes(baseline: &FileEdgeSet, current: &FileEdgeSet) -> Vec<String> {
    if baseline.identity.ledgerful_version != current.identity.ledgerful_version {
        vec!["analyzerBuildDiffers".to_string()]
    } else {
        Vec::new()
    }
}

pub(super) fn gate_reasons(
    baseline: Option<&BaselineIdentity>,
    current: &BaselineIdentity,
) -> Vec<IncomparableReason> {
    let mut reasons = Vec::new();
    let supported = |identity: &BaselineIdentity| {
        identity.baseline_schema == BASELINE_SCHEMA && identity.payload_kind == PAYLOAD_KIND
    };
    if !supported(current) || baseline.is_some_and(|identity| !supported(identity)) {
        reasons.push(IncomparableReason::SchemaMismatch);
    }
    if let Some(baseline) = baseline {
        if baseline.parser_version != current.parser_version {
            reasons.push(IncomparableReason::ParserVersionMismatch);
        }
        if baseline.edge_sources != current.edge_sources {
            reasons.push(IncomparableReason::EdgeSourcesMismatch);
        }
        if baseline.scope != current.scope {
            reasons.push(IncomparableReason::ScopeMismatch);
        }
        if !baseline.input_completeness.is_complete() {
            reasons.push(IncomparableReason::BaselineIncomplete);
        }
    }
    if !current.input_completeness.is_complete() {
        reasons.push(IncomparableReason::CurrentIncomplete);
    }
    reasons
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::baseline::identity::{
        BaselineIdentity, BaselineScope, CompletenessState, EdgeCoverage, FileEdge,
        InputCompleteness,
    };
    use proptest::prelude::*;

    fn complete_identity(version: &str, parser: &str) -> BaselineIdentity {
        BaselineIdentity {
            baseline_schema: BASELINE_SCHEMA,
            payload_kind: PAYLOAD_KIND.to_string(),
            scope: BaselineScope::repo(),
            parser_version: parser.to_string(),
            edge_sources: vec!["native".to_string()],
            ledgerful_version: version.to_string(),
            index_head_hash: None,
            input_digest: "blake3:same".to_string(),
            input_completeness: InputCompleteness::complete(),
        }
    }

    fn coverage(unresolved: u64, ambiguous: u64) -> EdgeCoverage {
        EdgeCoverage {
            edges_unresolved: unresolved,
            edges_ambiguous: ambiguous,
            ..EdgeCoverage::default()
        }
    }

    fn set(edges: &[(&str, &str, u32)], cov: EdgeCoverage) -> FileEdgeSet {
        FileEdgeSet {
            identity: complete_identity("0.2.18", "1"),
            coverage: cov,
            edges: edges
                .iter()
                .map(|(s, d, w)| FileEdge {
                    src_path: (*s).to_string(),
                    dst_path: (*d).to_string(),
                    weight: *w,
                })
                .collect(),
        }
    }

    fn counts(delta: &EdgeSetDelta) -> DeltaCounts {
        delta.counts.clone().expect("comparable delta has counts")
    }

    #[test]
    fn baseline_delta_classifies_every_class() {
        let baseline = set(
            &[("a", "b", 1), ("a", "c", 2), ("a", "d", 3)],
            coverage(0, 0),
        );
        let current = set(
            &[("a", "b", 1), ("a", "c", 3), ("a", "e", 1)],
            coverage(0, 0),
        );
        let delta = compare_edge_sets(&baseline, &current);
        assert!(matches!(delta.status, Comparability::Comparable));
        let c = counts(&delta);
        assert_eq!(
            (c.added, c.removed, c.worsened, c.improved, c.unchanged),
            (1, 1, 1, 0, 1)
        );
        assert_eq!(delta.regression, Regression::Present);
    }

    #[test]
    fn baseline_delta_improved_and_unchanged_boundaries() {
        let baseline = set(&[("a", "b", 2), ("a", "c", 1)], coverage(0, 0));
        let current = set(&[("a", "b", 1), ("a", "c", 1)], coverage(0, 0));
        let delta = compare_edge_sets(&baseline, &current);
        let c = counts(&delta);
        assert_eq!((c.improved, c.unchanged), (1, 1));
        assert_eq!(delta.regression, Regression::None);
    }

    #[test]
    fn baseline_delta_boundary_zero_edges_both_sides() {
        let delta = compare_edge_sets(&set(&[], coverage(0, 0)), &set(&[], coverage(0, 0)));
        assert!(matches!(delta.status, Comparability::Comparable));
        assert_eq!(delta.regression, Regression::None);
        assert_eq!(counts(&delta).added + counts(&delta).removed, 0);
        assert_eq!(delta.items.as_ref().map(Vec::len), Some(0));
    }

    #[test]
    fn baseline_delta_items_sorted_deterministically() {
        let baseline = set(&[("b", "z", 1), ("a", "y", 1)], coverage(0, 0));
        let current = set(&[("c", "x", 1), ("a", "y", 2)], coverage(0, 0));
        let delta = compare_edge_sets(&baseline, &current);
        let keys: Vec<_> = delta
            .items
            .unwrap()
            .into_iter()
            .map(|i| (i.edge_key.src_path, i.edge_key.dst_path))
            .collect();
        assert_eq!(
            keys,
            vec![
                ("a".to_string(), "y".to_string()),
                ("b".to_string(), "z".to_string()),
                ("c".to_string(), "x".to_string()),
            ]
        );
    }

    #[test]
    fn baseline_delta_identical_snapshot_all_unchanged_regression_none() {
        let baseline = set(&[("a", "b", 1), ("a", "c", 2)], coverage(1, 0));
        let current = set(&[("a", "b", 1), ("a", "c", 2)], coverage(1, 0));
        let delta = compare_edge_sets_with(&baseline, &current, true);
        assert_eq!(counts(&delta).unchanged, 2);
        assert_eq!(delta.regression, Regression::None);
        assert!(
            delta
                .items
                .unwrap()
                .iter()
                .all(|i| i.class == DeltaClass::Unchanged)
        );
    }

    #[test]
    fn baseline_delta_equal_digest_different_analyzer_is_not_all_unchanged() {
        let mut baseline = set(&[("a", "b", 1)], coverage(0, 0));
        baseline.identity.ledgerful_version = "0.2.18 (aaaaaaaaaaaa)".to_string();
        let mut current = set(&[("a", "c", 1)], coverage(0, 0));
        current.identity.ledgerful_version = "0.2.18 (bbbbbbbbbbbb)".to_string();
        current.identity.input_digest = baseline.identity.input_digest.clone();
        let delta = compare_edge_sets(&baseline, &current);
        assert_eq!(counts(&delta).added, 1);
        assert_eq!(counts(&delta).removed, 1);
        assert_eq!(counts(&delta).unchanged, 0);
        assert!(delta.notes.contains(&"analyzerBuildDiffers".to_string()));
    }

    #[test]
    fn baseline_compare_gate_reasons_are_sorted_tokens() {
        let baseline = set(&[("a", "b", 1)], coverage(0, 0));
        let mut current = set(&[("a", "b", 1)], coverage(0, 0));
        current.identity.parser_version = "2".to_string();
        current.identity.edge_sources = vec!["native".to_string(), "scip".to_string()];
        current.identity.scope = BaselineScope {
            kind: "component".to_string(),
        };
        current.identity.input_completeness =
            InputCompleteness::incomplete(vec!["CONTENT_DRIFT".to_string()]);
        let delta = compare_edge_sets(&baseline, &current);
        assert_eq!(
            delta.status.reasons(),
            vec![
                "CURRENT_INCOMPLETE".to_string(),
                "EDGE_SOURCES_MISMATCH".to_string(),
                "PARSER_VERSION_MISMATCH".to_string(),
                "SCOPE_MISMATCH".to_string(),
            ]
        );
        assert_eq!(delta.regression, Regression::Unknown);
        assert!(delta.counts.is_none() && delta.items.is_none());
    }

    #[test]
    fn baseline_incomparable_baseline_incomplete_has_no_counts_or_items() {
        let mut baseline = set(&[("a", "b", 1)], coverage(0, 0));
        baseline.identity.input_completeness =
            InputCompleteness::incomplete(vec!["SCAN_INCOMPLETE".to_string()]);
        let delta = compare_edge_sets(&baseline, &set(&[("a", "b", 1)], coverage(0, 0)));
        assert_eq!(delta.status.reasons(), vec!["BASELINE_INCOMPLETE"]);
        assert!(delta.counts.is_none() && delta.items.is_none());
        assert_eq!(delta.regression, Regression::Unknown);
    }

    #[test]
    fn baseline_incomparable_schema_mismatch_from_payload_kind() {
        let baseline = set(&[("a", "b", 1)], coverage(0, 0));
        let mut current = set(&[("a", "b", 1)], coverage(0, 0));
        current.identity.payload_kind = "symbolGraph".to_string();
        let delta = compare_edge_sets(&baseline, &current);
        assert_eq!(delta.status.reasons(), vec!["SCHEMA_MISMATCH"]);
    }

    #[test]
    fn baseline_regression_unknown_when_only_unresolved_grew() {
        let delta = compare_edge_sets(
            &set(&[("a", "b", 1)], coverage(1, 0)),
            &set(&[("a", "b", 1)], coverage(2, 0)),
        );
        assert_eq!(delta.regression, Regression::Unknown);
        assert_eq!(delta.regression_reasons, vec!["UNRESOLVED_GREW"]);
    }

    #[test]
    fn baseline_regression_unknown_when_ambiguous_grew_as_unresolved_shrank() {
        let delta = compare_edge_sets(
            &set(&[("a", "b", 1)], coverage(3, 0)),
            &set(&[("a", "b", 1)], coverage(1, 2)),
        );
        assert_eq!(delta.regression, Regression::Unknown);
        assert_eq!(delta.regression_reasons, vec!["AMBIGUOUS_GREW"]);
    }

    #[test]
    fn baseline_regression_both_coverage_reasons_sorted() {
        let delta = compare_edge_sets(
            &set(&[("a", "b", 1)], coverage(0, 0)),
            &set(&[("a", "b", 1)], coverage(1, 1)),
        );
        assert_eq!(
            delta.regression_reasons,
            vec!["AMBIGUOUS_GREW".to_string(), "UNRESOLVED_GREW".to_string()]
        );
    }

    #[test]
    fn baseline_regression_none_with_removed_edges_has_resolution_note() {
        let delta = compare_edge_sets(
            &set(&[("a", "b", 1), ("a", "c", 1)], coverage(1, 0)),
            &set(&[("a", "b", 1)], coverage(1, 0)),
        );
        assert_eq!(delta.regression, Regression::None);
        assert!(delta.notes.contains(&"RESOLVED_EDGES_REMOVED".to_string()));
    }

    #[test]
    fn baseline_delta_improvement_with_coverage_relief_has_no_removal_note() {
        let delta = compare_edge_sets(
            &set(&[("a", "b", 1), ("a", "c", 1)], coverage(4, 0)),
            &set(&[("a", "b", 1)], coverage(3, 0)),
        );
        assert_eq!(delta.regression, Regression::None);
        assert!(
            !delta.notes.contains(&"RESOLVED_EDGES_REMOVED".to_string()),
            "coverage relief means the removal is explained"
        );
    }

    proptest! {
        #[test]
        fn baseline_regression_none_implies_comparable_and_no_growth(
            baseline_edges in proptest::collection::btree_map("[a-c]", 1u32..4, 0..6),
            current_edges in proptest::collection::btree_map("[a-c]", 1u32..4, 0..6),
            baseline_unresolved in 0u64..4,
            baseline_ambiguous in 0u64..4,
            current_unresolved in 0u64..4,
            current_ambiguous in 0u64..4,
            base_complete in any::<bool>(),
            cur_complete in any::<bool>(),
            base_parser in "[12]",
            cur_parser in "[12]",
            base_sources in proptest::collection::btree_set("[a-z]{4,6}", 1..3),
            cur_sources in proptest::collection::btree_set("[a-z]{4,6}", 1..3),
            base_scope in "[a-z]{3,5}",
            cur_scope in "[a-z]{3,5}",
            base_schema in 1u32..3,
            cur_schema in 1u32..3,
            base_kind in "[a-z]{4,8}",
            cur_kind in "[a-z]{4,8}",
        ) {
            let to_set = |map: &BTreeMap<String, u32>, unresolved: u64, ambiguous: u64| {
                set(
                    &map.iter()
                        .map(|(k, w)| (k.as_str(), k.as_str(), *w))
                        .collect::<Vec<_>>(),
                    coverage(unresolved, ambiguous),
                )
            };
            let mut baseline = to_set(&baseline_edges, baseline_unresolved, baseline_ambiguous);
            let mut current = to_set(&current_edges, current_unresolved, current_ambiguous);
            baseline.identity.parser_version = base_parser;
            current.identity.parser_version = cur_parser;
            baseline.identity.edge_sources = base_sources.into_iter().collect();
            current.identity.edge_sources = cur_sources.into_iter().collect();
            baseline.identity.scope = BaselineScope { kind: base_scope };
            current.identity.scope = BaselineScope { kind: cur_scope };
            baseline.identity.baseline_schema = base_schema;
            current.identity.baseline_schema = cur_schema;
            baseline.identity.payload_kind = base_kind;
            current.identity.payload_kind = cur_kind;
            if !base_complete {
                baseline.identity.input_completeness =
                    InputCompleteness::incomplete(vec!["CONTENT_DRIFT".to_string()]);
            }
            if !cur_complete {
                current.identity.input_completeness =
                    InputCompleteness::incomplete(vec!["CONTENT_DRIFT".to_string()]);
            }

            let delta = compare_edge_sets(&baseline, &current);
            if delta.regression == Regression::None {
                prop_assert!(matches!(delta.status, Comparability::Comparable));
                let c = delta.counts.clone().unwrap();
                prop_assert_eq!(c.added + c.worsened, 0);
                prop_assert!(current.coverage.edges_unresolved <= baseline.coverage.edges_unresolved);
                prop_assert!(current.coverage.edges_ambiguous <= baseline.coverage.edges_ambiguous);
                // "none" is unreachable unless both sides are COMPLETE with a
                // matching parser/schema/scope/edge-source identity.
                prop_assert_eq!(baseline.identity.input_completeness.state, CompletenessState::Complete);
                prop_assert_eq!(current.identity.input_completeness.state, CompletenessState::Complete);
                prop_assert_eq!(baseline.identity.parser_version.as_str(), current.identity.parser_version.as_str());
                prop_assert_eq!(baseline.identity.edge_sources.clone(), current.identity.edge_sources.clone());
                prop_assert_eq!(baseline.identity.scope.clone(), current.identity.scope.clone());
                prop_assert_eq!(baseline.identity.baseline_schema, current.identity.baseline_schema);
                prop_assert_eq!(baseline.identity.payload_kind.as_str(), current.identity.payload_kind.as_str());
            }
        }
    }
}

#[cfg(test)]
mod incomparable_reason_tests {
    use super::*;
    use crate::commands::baseline::identity::{
        BASELINE_SCHEMA, BaselineIdentity, BaselineScope, EdgeCoverage, InputCompleteness,
        PAYLOAD_KIND,
    };

    fn base() -> FileEdgeSet {
        FileEdgeSet {
            identity: BaselineIdentity {
                baseline_schema: BASELINE_SCHEMA,
                payload_kind: PAYLOAD_KIND.to_string(),
                scope: BaselineScope::repo(),
                parser_version: "1".to_string(),
                edge_sources: vec!["native".to_string()],
                ledgerful_version: "0.2.18".to_string(),
                index_head_hash: None,
                input_digest: "blake3:x".to_string(),
                input_completeness: InputCompleteness::complete(),
            },
            coverage: EdgeCoverage::default(),
            edges: Vec::new(),
        }
    }

    fn assert_incomparable(current: FileEdgeSet, token: &str) {
        let delta = compare_edge_sets(&base(), &current);
        assert_eq!(delta.status.reasons(), vec![token.to_string()]);
        assert_eq!(delta.regression, Regression::Unknown);
        assert!(delta.counts.is_none(), "{token}: counts must be absent");
        assert!(delta.items.is_none(), "{token}: items must be absent");
    }

    #[test]
    fn baseline_incomparable_parser_version_mismatch() {
        let mut current = base();
        current.identity.parser_version = "2".to_string();
        assert_incomparable(current, "PARSER_VERSION_MISMATCH");
    }

    #[test]
    fn baseline_incomparable_edge_sources_mismatch() {
        let mut current = base();
        current.identity.edge_sources = vec!["native".to_string(), "scip".to_string()];
        assert_incomparable(current, "EDGE_SOURCES_MISMATCH");
    }

    #[test]
    fn baseline_incomparable_scope_mismatch() {
        let mut current = base();
        current.identity.scope = BaselineScope {
            kind: "component".to_string(),
        };
        assert_incomparable(current, "SCOPE_MISMATCH");
    }

    #[test]
    fn baseline_incomparable_current_incomplete() {
        let mut current = base();
        current.identity.input_completeness =
            InputCompleteness::incomplete(vec!["SCAN_INCOMPLETE".to_string()]);
        assert_incomparable(current, "CURRENT_INCOMPLETE");
    }
}

#[cfg(test)]
mod invariant_tests {
    use super::*;
    use crate::commands::baseline::identity::{
        BASELINE_SCHEMA, BaselineIdentity, BaselineScope, EdgeCoverage, FileEdge,
        InputCompleteness, PAYLOAD_KIND,
    };

    fn set(edges: &[(&str, &str, u32)]) -> FileEdgeSet {
        FileEdgeSet {
            identity: BaselineIdentity {
                baseline_schema: BASELINE_SCHEMA,
                payload_kind: PAYLOAD_KIND.to_string(),
                scope: BaselineScope::repo(),
                parser_version: "1".to_string(),
                edge_sources: vec!["native".to_string()],
                ledgerful_version: "0.2.18".to_string(),
                index_head_hash: None,
                input_digest: "blake3:x".to_string(),
                input_completeness: InputCompleteness::complete(),
            },
            coverage: EdgeCoverage::default(),
            edges: edges
                .iter()
                .map(|(s, d, w)| FileEdge {
                    src_path: (*s).to_string(),
                    dst_path: (*d).to_string(),
                    weight: *w,
                })
                .collect(),
        }
    }

    #[test]
    fn baseline_delta_counts_sum_equals_union() {
        let baseline = set(&[("a", "b", 1), ("a", "c", 2), ("a", "d", 1)]);
        let current = set(&[("a", "b", 1), ("a", "c", 1), ("a", "e", 1), ("a", "d", 3)]);
        let union: std::collections::BTreeSet<(&str, &str)> = baseline
            .edges
            .iter()
            .chain(current.edges.iter())
            .map(|e| (e.src_path.as_str(), e.dst_path.as_str()))
            .collect();
        let delta = compare_edge_sets_with(&baseline, &current, true);
        let c = delta.counts.clone().unwrap();
        let sum = c.added + c.removed + c.worsened + c.improved + c.unchanged;
        assert_eq!(
            sum,
            union.len() as u64,
            "every union edge is classified once"
        );
        assert_eq!(delta.items.unwrap().len() as u64, sum);
    }
}
