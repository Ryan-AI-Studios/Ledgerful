use super::super::config::{ComponentDef, parse_architecture_toml};
use super::*;
use crate::commands::architecture::test_support::{
    COMPONENTS, edge_set, fixture_config, permutations,
};

fn overlap_config() -> ArchitectureConfig {
    let mut config = fixture_config();
    config.components.insert(
        0,
        ComponentDef {
            name: "z".to_owned(),
            paths: vec!["src/a/shared/**".to_owned()],
        },
    );
    config
}

fn run(config: &ArchitectureConfig, edges: &[(&str, &str, u32)]) -> ArchitectureAnalysis {
    analyze(
        &edge_set(edges),
        &ComponentMap::build(config).unwrap(),
        config,
    )
}

fn rule(id: &str, effect: RuleEffect, from: &str, to: &str) -> BoundaryRule {
    BoundaryRule {
        id: id.to_owned(),
        effect,
        from: from.to_owned(),
        to: to.to_owned(),
    }
}

fn with_rules(rules: Vec<BoundaryRule>) -> ArchitectureConfig {
    let mut config = fixture_config();
    config.rules = rules;
    config
}

#[test]
fn architecture_assign_three_outcomes() {
    let map = ComponentMap::build(&overlap_config()).unwrap();
    assert_eq!(map.assign("unassigned.rs"), Assignment::Unassigned);
    assert_eq!(
        map.assign("src/b/file.rs"),
        Assignment::Assigned("b".to_owned())
    );
    assert_eq!(
        map.assign("src/a/shared/file.rs"),
        Assignment::Overlapping(vec!["a".to_owned(), "z".to_owned()])
    );
}

#[test]
fn architecture_assign_literal_separator() {
    let narrow = parse_architecture_toml(&COMPONENTS.replace("src/a/**", "src/a/*")).unwrap();
    let narrow = ComponentMap::build(&narrow).unwrap();
    let recursive = ComponentMap::build(&fixture_config()).unwrap();
    assert_eq!(
        narrow.assign("src/a/x.rs"),
        Assignment::Assigned("a".to_owned())
    );
    assert_eq!(narrow.assign("src/a/sub/x.rs"), Assignment::Unassigned);
    assert_eq!(
        recursive.assign("src/a/sub/x.rs"),
        Assignment::Assigned("a".to_owned())
    );
}

#[test]
fn architecture_assign_case_sensitive() {
    let map = ComponentMap::build(&fixture_config()).unwrap();
    assert_eq!(map.assign("SRC/a/file.rs"), Assignment::Unassigned);
    assert_eq!(map.assign("src/A/file.rs"), Assignment::Unassigned);
}

#[test]
fn architecture_assign_overlapping_list_sorted() {
    let mut config = overlap_config();
    config.components.insert(
        0,
        ComponentDef {
            name: "m".to_owned(),
            paths: vec!["src/a/shared/**".to_owned()],
        },
    );
    let map = ComponentMap::build(&config).unwrap();
    assert_eq!(
        map.assign("src/a/shared/x.rs"),
        Assignment::Overlapping(vec!["a".to_owned(), "m".to_owned(), "z".to_owned()]),
    );
}

#[test]
fn architecture_edge_category_each_of_five() {
    let map = ComponentMap::build(&overlap_config()).unwrap();
    for (source, destination, expected) in [
        ("src/a/x.rs", "src/a/x.rs", EdgeCategory::SelfEdge),
        (
            "src/a/shared/x.rs",
            "src/b/x.rs",
            EdgeCategory::AmbiguousMapping,
        ),
        ("src/a/x.rs", "outside.rs", EdgeCategory::Unmapped),
        ("src/a/x.rs", "src/a/y.rs", EdgeCategory::IntraComponent),
        (
            "src/a/x.rs",
            "src/b/x.rs",
            EdgeCategory::CrossComponent { from: "a", to: "b" },
        ),
    ] {
        let set = edge_set(&[(source, destination, 1)]);
        assert_eq!(
            categorize(&set.edges[0], &map.assign(source), &map.assign(destination)),
            expected
        );
    }
}

#[test]
fn architecture_edge_category_order_is_honored() {
    let map = ComponentMap::build(&overlap_config()).unwrap();
    let set = edge_set(&[
        ("src/a/shared/x.rs", "outside.rs", 1),
        ("outside.rs", "outside.rs", 1),
        ("src/a/shared/x.rs", "src/a/shared/x.rs", 1),
    ]);
    let categories: Vec<_> = set
        .edges
        .iter()
        .map(|edge| {
            match categorize(
                edge,
                &map.assign(&edge.src_path),
                &map.assign(&edge.dst_path),
            ) {
                EdgeCategory::AmbiguousMapping => "ambiguous",
                EdgeCategory::SelfEdge => "self",
                _ => "unexpected",
            }
        })
        .collect();
    assert_eq!(categories, ["ambiguous", "self", "self"]);
}

#[test]
fn architecture_edge_categories_sum_to_file_edges() {
    let result = run(
        &overlap_config(),
        &[
            ("src/a/x.rs", "src/a/x.rs", 1),
            ("src/a/shared/x.rs", "src/b/x.rs", 3),
            ("src/a/x.rs", "outside.rs", 4),
            ("src/a/x.rs", "src/a/y.rs", 12),
            ("src/a/x.rs", "src/b/x.rs", 94),
        ],
    );
    let summary = result.mapping;
    assert_eq!(summary.file_edges, 5);
    assert_eq!(
        [
            summary.self_edges,
            summary.ambiguous_mapping_edges,
            summary.unmapped_edges,
            summary.intra_component_edges,
            summary.cross_component_edges,
        ],
        [1, 1, 1, 1, 1],
    );
}

#[test]
fn architecture_edge_self_edge_never_violation_or_cycle() {
    let config = with_rules(vec![rule("all", RuleEffect::Forbid, "*", "*")]);
    let analysis = run(&config, &[("src/a/x.rs", "src/a/x.rs", 60)]);
    assert_eq!(analysis.mapping.self_edges, 1);
    assert!(analysis.violations.is_empty());
    assert!(analysis.cycle_groups.is_empty());
}

#[test]
fn architecture_edge_overlapping_endpoint_files_sorted_endpoint_only() {
    let result = run(
        &overlap_config(),
        &[
            ("src/a/shared/z.rs", "outside.rs", 1),
            ("src/a/shared/a.rs", "src/a/shared/z.rs", 2),
        ],
    );
    assert_eq!(result.mapping.endpoint_files, 3);
    assert_eq!(result.mapping.endpoint_files_unassigned, 1);
    assert_eq!(result.mapping.endpoint_files_overlapping, 2);
    assert_eq!(
        result
            .mapping
            .overlapping_endpoint_files
            .iter()
            .map(|file| file.path.as_str())
            .collect::<Vec<_>>(),
        ["src/a/shared/a.rs", "src/a/shared/z.rs"],
    );
}

#[test]
fn architecture_rule_specificity_matrix() {
    let config = with_rules(vec![
        rule("fallback", RuleEffect::Forbid, "*", "*"),
        rule("from-a", RuleEffect::Allow, "a", "*"),
        rule("a-to-b", RuleEffect::Forbid, "a", "b"),
    ]);
    assert_eq!(
        decide_rule(&config, "a", "b"),
        RuleDecision::Forbidden { rule_id: "a-to-b" }
    );
    assert_eq!(
        decide_rule(&config, "a", "c"),
        RuleDecision::Allowed { rule_id: "from-a" }
    );
    assert_eq!(
        decide_rule(&config, "c", "b"),
        RuleDecision::Forbidden {
            rule_id: "fallback"
        }
    );
}

#[test]
fn architecture_rule_tie_forbid_wins_smallest_id() {
    let mut config = with_rules(vec![
        rule("a-allow", RuleEffect::Allow, "a", "*"),
        rule("z-forbid", RuleEffect::Forbid, "*", "b"),
    ]);
    assert_eq!(
        decide_rule(&config, "a", "b"),
        RuleDecision::Forbidden {
            rule_id: "z-forbid"
        }
    );
    config.rules[0].effect = RuleEffect::Forbid;
    config.rules[0].id = "a-forbid".to_owned();
    assert_eq!(
        decide_rule(&config, "a", "b"),
        RuleDecision::Forbidden {
            rule_id: "a-forbid"
        }
    );
}

#[test]
fn architecture_rule_allow_specific_beats_forbid_star() {
    use RuleEffect::{Allow, Forbid};
    let config = with_rules(vec![
        rule("allow", Allow, "b", "a"),
        rule("reject", Forbid, "*", "*"),
    ]);
    assert_eq!(
        decide_rule(&config, "b", "a"),
        RuleDecision::Allowed { rule_id: "allow" }
    );
}

#[test]
fn architecture_rule_no_match_is_unconstrained_and_counted() {
    let config = fixture_config();
    assert_eq!(decide_rule(&config, "a", "b"), RuleDecision::Unconstrained);
    let result = run(&config, &[("src/a/x.rs", "src/b/x.rs", 2)]);
    assert_eq!(result.unconstrained_cross_edges, 1);
    assert!(result.violations.is_empty());
}

#[test]
fn architecture_rule_intra_component_never_evaluated() {
    let config = with_rules(vec![rule("all", RuleEffect::Forbid, "*", "*")]);
    let result = run(&config, &[("src/a/x.rs", "src/a/y.rs", 11)]);
    assert_eq!(result.mapping.intra_component_edges, 1);
    assert!(result.violations.is_empty());
    assert_eq!(result.unconstrained_cross_edges, 0);
}

#[test]
fn architecture_rule_result_independent_of_rule_order() {
    use RuleEffect::{Allow, Forbid};
    let mut rules = [
        rule("fallback", Forbid, "*", "*"),
        rule("from-a", Allow, "a", "*"),
        rule("to-b", Forbid, "*", "b"),
        rule("a-to-b", Allow, "a", "b"),
    ];
    let original = with_rules(rules.to_vec());
    permutations(&mut rules, &mut |order| {
        let current = with_rules(order.to_vec());
        for from in ["a", "b", "c"] {
            for to in ["a", "b", "c"] {
                assert_eq!(
                    decide_rule(&current, from, to),
                    decide_rule(&original, from, to)
                );
            }
        }
    });
}

#[test]
fn architecture_rule_allow_only_config_yields_no_violations() {
    let config = with_rules(vec![rule("all", RuleEffect::Allow, "*", "*")]);
    let result = run(&config, &[("src/b/x.rs", "src/a/x.rs", 3)]);
    assert_eq!(result.mapping.cross_component_edges, 1);
    assert_eq!(result.unconstrained_cross_edges, 0);
    assert!(result.violations.is_empty());
}

#[test]
fn architecture_violation_fields_and_sort_order() {
    let result = run(
        &fixture_config(),
        &[
            ("src/b/z.rs", "src/a/x.rs", 9),
            ("src/b/a.rs", "src/a/z.rs", 11),
            ("src/b/a.rs", "src/a/a.rs", 5),
        ],
    );
    assert_eq!(
        result
            .violations
            .iter()
            .map(|entry| (&*entry.src_path, &*entry.dst_path))
            .collect::<Vec<_>>(),
        [
            ("src/b/a.rs", "src/a/a.rs"),
            ("src/b/a.rs", "src/a/z.rs"),
            ("src/b/z.rs", "src/a/x.rs"),
        ],
    );
    let first = &result.violations[0];
    assert_eq!(
        (
            &*first.rule_id,
            &*first.src_component,
            &*first.dst_component,
            first.weight
        ),
        ("b-to-a", "b", "a", 5)
    );
}

#[test]
fn architecture_analyze_acyclic_chain_has_no_cycle_groups() {
    let result = run(
        &fixture_config(),
        &[
            ("src/a/x.rs", "src/b/x.rs", 1),
            ("src/b/x.rs", "src/c/x.rs", 2),
        ],
    );
    assert!(result.cycle_groups.is_empty());
    assert_eq!(result.component_edges.len(), 2);
}

#[test]
fn architecture_analyze_two_component_cycle_reports_group() {
    let result = run(
        &fixture_config(),
        &[
            ("src/a/x.rs", "src/b/x.rs", 1),
            ("src/b/y.rs", "src/a/y.rs", 2),
        ],
    );
    assert_eq!(result.cycle_groups[0].members, ["a", "b"]);
    let edges = &result.cycle_groups[0].component_edges;
    assert_eq!(edges[0].witnesses[0].src_path, "src/a/x.rs");
    assert_eq!(edges[1].witnesses[0].src_path, "src/b/y.rs");
}

#[test]
fn architecture_analyze_cycle_through_unassigned_file_not_reported() {
    let result = run(
        &fixture_config(),
        &[
            ("src/a/x.rs", "src/b/x.rs", 1),
            ("src/b/x.rs", "unassigned.rs", 2),
            ("unassigned.rs", "src/a/x.rs", 3),
        ],
    );
    assert_eq!(result.mapping.unmapped_edges, 2);
    assert_eq!(result.mapping.cross_component_edges, 1);
    assert!(result.cycle_groups.is_empty());
}

#[test]
fn architecture_analyze_intra_component_edges_make_no_group() {
    let result = run(
        &fixture_config(),
        &[
            ("src/a/x.rs", "src/a/y.rs", 1),
            ("src/a/y.rs", "src/a/x.rs", 2),
        ],
    );
    assert_eq!(result.mapping.intra_component_edges, 2);
    assert!(result.cycle_groups.is_empty());
}

#[test]
fn architecture_analyze_is_independent_of_edge_input_order() {
    let config = fixture_config();
    let mut edges = [
        ("src/b/x.rs", "src/a/x.rs", 1),
        ("src/a/x.rs", "src/b/x.rs", 2),
        ("src/b/y.rs", "src/a/y.rs", 3),
        ("src/b/x.rs", "src/c/x.rs", 4),
        ("src/c/x.rs", "src/b/x.rs", 5),
        ("src/b/z.rs", "outside.rs", 6),
    ];
    let expected = serde_json::to_vec(&run(&config, &edges)).unwrap();
    let mut count = 0;
    permutations(&mut edges, &mut |order| {
        assert_eq!(serde_json::to_vec(&run(&config, order)).unwrap(), expected);
        count += 1;
    });
    assert_eq!(count, 720);
}
