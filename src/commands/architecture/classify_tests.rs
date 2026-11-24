use super::super::config::{ComponentDef, config_digest, parse_architecture_toml};
use super::super::cycles::ComponentEdge;
use super::*;
use crate::commands::architecture::test_support::{
    COMPONENTS, FORBID_B_TO_A, INCOMPLETE_REASONS, edge_set, fixture_config,
};
use crate::commands::baseline::InputCompleteness;
use proptest::prelude::*;

const FORWARD: (&str, &str, u32) = ("src/a/x.rs", "src/b/x.rs", 1);
const FORBIDDEN: (&str, &str, u32) = ("src/b/x.rs", "src/a/x.rs", 2);

fn difference(before: &[(&str, &str, u32)], after: &[(&str, &str, u32)]) -> ArchitectureDelta {
    let config = fixture_config();
    let map = ComponentMap::build(&config).unwrap();
    evaluate_against_baseline(&edge_set(before), &edge_set(after), &map, &config).2
}

fn group(members: &[&str], edges: &[(&str, &str)]) -> CycleGroup {
    CycleGroup {
        members: members.iter().map(|member| (*member).to_owned()).collect(),
        component_edges: edges
            .iter()
            .map(|&(source, destination)| ComponentEdge {
                src: source.to_owned(),
                dst: destination.to_owned(),
                weight: 1,
                witness_count: 0,
                witness_truncated: false,
                witnesses: Vec::new(),
            })
            .collect(),
    }
}

fn cycle_difference(before: Vec<CycleGroup>, after: Vec<CycleGroup>) -> ArchitectureDelta {
    let before = ArchitectureAnalysis {
        cycle_groups: before,
        ..ArchitectureAnalysis::default()
    };
    let after = ArchitectureAnalysis {
        cycle_groups: after,
        ..ArchitectureAnalysis::default()
    };
    classify(&before, &after)
}

fn verdict_input() -> VerdictInputs {
    VerdictInputs {
        current_completeness_complete: true,
        current_reasons: Vec::new(),
        current_coverage: EdgeCoverage::default(),
        current_mapping_ambiguous: 0,
        current_cross_component_edges: 1,
        current_findings: 0,
        baseline: BaselineVerdictInput::NotRequested,
    }
}

fn comparable(
    coverage: EdgeCoverage,
    overlap: u64,
    cross: u64,
    findings: u64,
) -> BaselineVerdictInput {
    BaselineVerdictInput::Comparable {
        baseline_coverage: coverage,
        baseline_mapping_ambiguous: overlap,
        baseline_cross_component_edges: cross,
        regressing_findings: findings,
    }
}

#[test]
fn architecture_classify_violation_new() {
    let delta = difference(&[], &[FORBIDDEN]);
    assert_eq!(delta.violations[0].class, ViolationClass::New);
    assert_eq!(delta.violations[0].baseline_weight, None);
    assert_eq!(delta.counts.violations.new, 1);
}

#[test]
fn architecture_classify_violation_worsened_plus_one() {
    let delta = difference(&[FORBIDDEN], &[("src/b/x.rs", "src/a/x.rs", 3)]);
    assert_eq!(delta.violations[0].class, ViolationClass::Worsened);
    assert_eq!(delta.violations[0].baseline_weight, Some(2));
    assert_eq!(delta.counts.violations.worsened, 1);
}

#[test]
fn architecture_classify_violation_existing_equal() {
    let delta = difference(&[FORBIDDEN], &[FORBIDDEN]);
    assert_eq!(delta.violations[0].class, ViolationClass::Existing);
    assert_eq!(delta.violations[0].baseline_weight, Some(2));
}

#[test]
fn architecture_classify_violation_existing_when_lower() {
    let delta = difference(&[FORBIDDEN], &[("src/b/x.rs", "src/a/x.rs", 1)]);
    assert_eq!(delta.violations[0].class, ViolationClass::Existing);
    assert_eq!(delta.violations[0].baseline_weight, Some(2));
    assert_eq!(delta.violations[0].violation.weight, 1);
}

#[test]
fn architecture_classify_violation_removed() {
    let delta = difference(&[FORBIDDEN], &[]);
    assert!(delta.violations.is_empty());
    assert_eq!(delta.removed_violations[0].baseline_weight, 2);
    assert_eq!(delta.removed_violations[0].rule_id, "b-to-a");
    assert_eq!(delta.counts.violations.removed, 1);
}

fn basic_group() -> CycleGroup {
    group(&["a", "b"], &[("a", "b"), ("b", "a")])
}

fn three_group() -> CycleGroup {
    group(&["a", "b", "c"], &[("a", "b"), ("b", "c"), ("c", "a")])
}

#[test]
fn architecture_classify_cycle_introduced() {
    let delta = cycle_difference(vec![], vec![basic_group()]);
    assert_eq!(delta.cycle_groups[0].class, CycleClass::Introduced);
    assert_eq!(delta.counts.cycle_groups.introduced, 1);
}

#[test]
fn architecture_classify_cycle_grown_member_added() {
    let delta = cycle_difference(vec![basic_group()], vec![three_group()]);
    assert_eq!(delta.cycle_groups[0].class, CycleClass::Grown);
}

#[test]
fn architecture_classify_cycle_grown_merged_groups() {
    let before = vec![basic_group(), group(&["c", "d"], &[("c", "d"), ("d", "c")])];
    let after = vec![group(
        &["a", "b", "c", "d"],
        &[("a", "b"), ("b", "c"), ("c", "d"), ("d", "a")],
    )];
    let delta = cycle_difference(before, after);
    assert_eq!(delta.cycle_groups[0].class, CycleClass::Grown);
    assert!(delta.removed_cycle_groups.is_empty());
}

#[test]
fn architecture_classify_cycle_grown_extra_internal_pair() {
    let after = group(
        &["a", "b", "c"],
        &[("a", "b"), ("b", "c"), ("c", "a"), ("a", "c")],
    );
    let delta = cycle_difference(vec![three_group()], vec![after]);
    assert_eq!(delta.cycle_groups[0].class, CycleClass::Grown);
}

#[test]
fn architecture_classify_cycle_unchanged_same_pairs() {
    let delta = cycle_difference(vec![basic_group()], vec![basic_group()]);
    assert_eq!(delta.cycle_groups[0].class, CycleClass::Unchanged);
}

#[test]
fn architecture_classify_cycle_unchanged_weight_only_change() {
    let mut current = basic_group();
    current.component_edges[0].weight += 4;
    let delta = cycle_difference(vec![basic_group()], vec![current]);
    assert_eq!(delta.cycle_groups[0].class, CycleClass::Unchanged);
}

#[test]
fn architecture_classify_cycle_shrunk_member_removed() {
    let delta = cycle_difference(vec![three_group()], vec![basic_group()]);
    assert_eq!(delta.cycle_groups[0].class, CycleClass::Shrunk);
    assert!(delta.removed_cycle_groups.is_empty());
}

#[test]
fn architecture_classify_cycle_shrunk_pair_removed() {
    let before = group(
        &["a", "b", "c"],
        &[("a", "b"), ("b", "c"), ("c", "a"), ("a", "c")],
    );
    let delta = cycle_difference(vec![before], vec![three_group()]);
    assert_eq!(delta.cycle_groups[0].class, CycleClass::Shrunk);
}

#[test]
fn architecture_classify_cycle_removed() {
    let delta = cycle_difference(vec![basic_group()], vec![]);
    assert_eq!(delta.removed_cycle_groups[0].members, ["a", "b"]);
    assert_eq!(delta.counts.cycle_groups.removed, 1);
}

#[test]
fn architecture_classify_cycle_uncovered_case_is_grown() {
    let after = group(&["a", "b", "c"], &[("a", "c"), ("c", "b"), ("b", "a")]);
    let delta = cycle_difference(vec![three_group()], vec![after]);
    assert_eq!(delta.cycle_groups[0].class, CycleClass::Grown);
}

#[test]
fn architecture_classify_existing_debt_is_regression_none() {
    let set = [FORWARD, FORBIDDEN];
    let delta = difference(&set, &set);
    assert_eq!(delta.violations[0].class, ViolationClass::Existing);
    assert_eq!(delta.cycle_groups[0].class, CycleClass::Unchanged);
    let mut input = verdict_input();
    input.current_findings = 2;
    input.baseline = comparable(
        EdgeCoverage::default(),
        0,
        2,
        delta.counts.regressing_findings(),
    );
    let verdict = derive_verdicts(&input);
    assert_eq!(verdict.observation, Observation::Findings);
    assert_eq!(verdict.regression, Regression::None);
}

#[test]
fn architecture_classify_new_forbidden_edge_in_violating_pair_is_present() {
    let delta = difference(
        &[FORWARD, FORBIDDEN],
        &[FORWARD, FORBIDDEN, ("src/b/y.rs", "src/a/x.rs", 1)],
    );
    assert_eq!(delta.violations[1].class, ViolationClass::New);
    let mut input = verdict_input();
    input.current_findings = 3;
    input.baseline = comparable(
        EdgeCoverage::default(),
        0,
        2,
        delta.counts.regressing_findings(),
    );
    assert_eq!(derive_verdicts(&input).regression, Regression::Present);
}

#[test]
fn architecture_classify_config_edit_applies_symmetrically() {
    let before = format!("{COMPONENTS}{FORBID_B_TO_A}");
    let renamed = before
        .replace("name = \"b\"", "name = \"renamed\"")
        .replace("from = \"b\"", "from = \"renamed\"");
    for text in [renamed.as_str(), COMPONENTS] {
        let config = parse_architecture_toml(text).unwrap();
        let map = ComponentMap::build(&config).unwrap();
        let set = edge_set(&[FORWARD, FORBIDDEN]);
        let (_, _, delta) = evaluate_against_baseline(&set, &set, &map, &config);
        assert_ne!(
            config_digest(text.as_bytes()),
            config_digest(before.as_bytes())
        );
        assert_eq!(delta.cycle_groups[0].class, CycleClass::Unchanged);
        assert!(delta.removed_violations.is_empty());
        assert!(delta.removed_cycle_groups.is_empty());
        assert_eq!(delta.counts.regressing_findings(), 0);
        let mut input = verdict_input();
        input.baseline = comparable(EdgeCoverage::default(), 0, 2, 0);
        assert_eq!(derive_verdicts(&input).regression, Regression::None);
    }
}

#[test]
fn architecture_classify_both_sides_use_same_map_and_config() {
    let config = fixture_config();
    let map = ComponentMap::build(&config).unwrap();
    let before = edge_set(&[FORWARD]);
    let after = edge_set(&[FORWARD, FORBIDDEN]);
    let (baseline, current, delta) = evaluate_against_baseline(&before, &after, &map, &config);
    assert_eq!(baseline, analyze(&before, &map, &config));
    assert_eq!(current, analyze(&after, &map, &config));
    assert_eq!(delta, classify(&baseline, &current));
}

#[test]
fn architecture_honesty_incomplete_current_one_case_per_reason() {
    for &reason in INCOMPLETE_REASONS {
        let mut input = verdict_input();
        input.current_completeness_complete = false;
        input.current_reasons.push(reason.to_owned());
        let verdict = derive_verdicts(&input);
        assert_eq!(verdict.status, Status::Unknown);
        assert_eq!(verdict.status_reasons, [reason]);
        assert_eq!(verdict.observation, Observation::Unknown);
        assert_eq!(verdict.regression, Regression::NotEvaluated);
        input.baseline = BaselineVerdictInput::Incomparable {
            reasons: vec!["CURRENT_INCOMPLETE".to_owned()],
        };
        assert_eq!(derive_verdicts(&input).regression, Regression::Unknown);
    }
}

#[test]
fn architecture_honesty_incomparable_one_case_per_reason() {
    for reason in [
        "BASELINE_DIGEST_MISMATCH",
        "BASELINE_INCOMPLETE",
        "CURRENT_INCOMPLETE",
        "EDGE_SOURCES_MISMATCH",
        "PARSER_VERSION_MISMATCH",
        "SCHEMA_MISMATCH",
        "SCOPE_MISMATCH",
        "BASELINE_CORRUPT",
    ] {
        let mut input = verdict_input();
        input.baseline = BaselineVerdictInput::Incomparable {
            reasons: vec![reason.to_owned()],
        };
        let verdict = derive_verdicts(&input);
        assert_eq!(verdict.regression, Regression::Unknown);
        assert_eq!(verdict.regression_reasons, [reason]);
    }
}

#[test]
fn architecture_honesty_overlap_blocks_none_and_noneobserved() {
    let mut input = verdict_input();
    input.current_mapping_ambiguous = 1;
    input.baseline = comparable(EdgeCoverage::default(), 0, 1, 0);
    let current = derive_verdicts(&input);
    assert_eq!(current.observation, Observation::Unknown);
    assert_eq!(current.observation_reasons, [OVERLAP]);
    assert_eq!(current.regression, Regression::Unknown);
    input.current_mapping_ambiguous = 0;
    input.baseline = comparable(EdgeCoverage::default(), 1, 1, 0);
    let baseline = derive_verdicts(&input);
    assert_eq!(baseline.observation, Observation::NoneObserved);
    assert_eq!(baseline.regression, Regression::Unknown);
    assert_eq!(baseline.regression_reasons, [OVERLAP]);
}

#[test]
fn architecture_honesty_unresolved_present_blocks_noneobserved() {
    let mut input = verdict_input();
    input.current_coverage.edges_unresolved = 1;
    let verdict = derive_verdicts(&input);
    assert_eq!(verdict.observation, Observation::Unknown);
    assert_eq!(verdict.observation_reasons, ["UNRESOLVED_PRESENT"]);
    input.current_findings = 1;
    assert_eq!(derive_verdicts(&input).observation, Observation::Findings);
}

#[test]
fn architecture_honesty_equal_unresolved_with_no_findings_is_regression_none() {
    let mut input = verdict_input();
    input.current_coverage.edges_unresolved = 3;
    input.baseline = comparable(input.current_coverage.clone(), 0, 1, 0);
    let verdict = derive_verdicts(&input);
    assert_eq!(verdict.observation, Observation::Unknown);
    assert_eq!(verdict.regression, Regression::None);
}

#[test]
fn architecture_honesty_unresolved_growth_is_regression_unknown() {
    for (before, current, reason) in [
        ((0, 0), (0, 1), "UNRESOLVED_GREW"),
        ((0, 0), (1, 0), "AMBIGUOUS_GREW"),
        ((2, 0), (0, 2), "UNRESOLVED_GREW"),
        ((0, 2), (2, 0), "AMBIGUOUS_GREW"),
    ] {
        let mut input = verdict_input();
        input.current_coverage.edges_ambiguous = current.0;
        input.current_coverage.edges_unresolved = current.1;
        input.baseline = comparable(
            EdgeCoverage {
                edges_ambiguous: before.0,
                edges_unresolved: before.1,
                ..EdgeCoverage::default()
            },
            0,
            1,
            0,
        );
        let verdict = derive_verdicts(&input);
        assert_eq!(verdict.regression, Regression::Unknown);
        assert_eq!(verdict.regression_reasons, [reason]);
    }
}

#[test]
fn architecture_honesty_property_none_implications() {
    let mut config = fixture_config();
    config.components.push(ComponentDef {
        name: "overlap".to_owned(),
        paths: vec!["src/a/shared/**".to_owned()],
    });
    let map = ComponentMap::build(&config).unwrap();
    let graph_edges = [
        FORWARD,
        FORBIDDEN,
        ("src/a/x.rs", "src/a/y.rs", 1),
        ("src/a/x.rs", "outside.rs", 1),
        ("src/a/shared/x.rs", "src/b/x.rs", 1),
    ];

    for index in [None, Some(2), Some(3)] {
        let edges: Vec<_> = index.map(|i| graph_edges[i]).into_iter().collect();
        let empty_projection = analyze(&edge_set(&edges), &map, &config);
        assert_eq!(empty_projection.mapping.cross_component_edges, 0);
        let mut input = verdict_input();
        input.current_cross_component_edges = 0;
        input.baseline = comparable(EdgeCoverage::default(), 0, 1, 0);
        let verdict = derive_verdicts(&input);
        assert_eq!(verdict.observation, Observation::Unknown);
        assert_eq!(verdict.regression, Regression::Unknown);
        assert_eq!(verdict.regression_reasons, [NO_CROSS_EDGES]);
        input.current_cross_component_edges = 1;
        input.baseline = comparable(EdgeCoverage::default(), 0, 0, 0);
        assert_eq!(derive_verdicts(&input).regression_reasons, [NO_CROSS_EDGES]);
    }

    proptest!(|(
        baseline_mask in any::<u8>(), current_mask in any::<u8>(),
        baseline_complete in any::<bool>(), current_complete in any::<bool>(),
        baseline_ambiguous in 0u64..4, baseline_unresolved in 0u64..4,
        current_ambiguous in 0u64..4, current_unresolved in 0u64..4,
        baseline_requested in any::<bool>(),
    )| {
        let selected = |mask| {
            let edges: Vec<_> = graph_edges.iter().enumerate()
                .filter(|(index, _)| mask & (1u8 << index) != 0)
                .map(|(_, &edge)| edge)
                .collect();
            edge_set(&edges)
        };
        let mut before = selected(baseline_mask);
        let mut current = selected(current_mask);
        if !baseline_complete {
            before.identity.input_completeness = InputCompleteness::incomplete(vec!["INDEX_STALE".to_owned()]);
        }
        if !current_complete {
            current.identity.input_completeness = InputCompleteness::incomplete(vec!["INDEX_STALE".to_owned()]);
        }
        before.coverage.edges_ambiguous = baseline_ambiguous;
        before.coverage.edges_unresolved = baseline_unresolved;
        current.coverage.edges_ambiguous = current_ambiguous;
        current.coverage.edges_unresolved = current_unresolved;

        let (old, live, delta) = evaluate_against_baseline(&before, &current, &map, &config);
        let comparison = if !baseline_requested {
            BaselineVerdictInput::NotRequested
        } else if baseline_complete && current_complete {
            comparable(
                before.coverage.clone(), old.mapping.ambiguous_mapping_edges,
                old.mapping.cross_component_edges, delta.counts.regressing_findings(),
            )
        } else {
            BaselineVerdictInput::Incomparable { reasons: vec!["INCOMPLETE".to_owned()] }
        };
        let findings = live.violations.len() as u64 + live.cycle_groups.len() as u64;
        let result = derive_verdicts(&VerdictInputs {
            current_completeness_complete: current.identity.input_completeness.is_complete(),
            current_reasons: current.identity.input_completeness.reasons.clone(),
            current_coverage: current.coverage.clone(),
            current_mapping_ambiguous: live.mapping.ambiguous_mapping_edges,
            current_cross_component_edges: live.mapping.cross_component_edges,
            current_findings: findings,
            baseline: comparison,
        });
        if result.regression == Regression::None {
            prop_assert!(baseline_requested && baseline_complete && current_complete);
            prop_assert_eq!(delta.counts.regressing_findings(), 0);
            prop_assert!(current_ambiguous <= baseline_ambiguous && current_unresolved <= baseline_unresolved);
            prop_assert!(old.mapping.cross_component_edges > 0 && live.mapping.cross_component_edges > 0);
            prop_assert_eq!(old.mapping.ambiguous_mapping_edges + live.mapping.ambiguous_mapping_edges, 0);
        }
        if result.observation == Observation::NoneObserved {
            prop_assert!(current_complete);
            prop_assert_eq!(findings, 0);
            prop_assert_eq!(current_ambiguous + current_unresolved, 0);
            prop_assert_eq!(live.mapping.ambiguous_mapping_edges, 0);
            prop_assert!(live.mapping.cross_component_edges > 0);
        }
    });
}
