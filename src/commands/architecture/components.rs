use std::collections::{BTreeMap, btree_map::Entry};
use std::convert::Infallible;

#[cfg(test)]
#[path = "components_tests.rs"]
mod tests;

use globset::{GlobSet, GlobSetBuilder};
use miette::Result;
use serde::Serialize;

use crate::commands::baseline::{FileEdge, FileEdgeSet};

use super::config::{ArchitectureConfig, BoundaryRule, RuleEffect, compile_glob, invalid_config};
use super::cycles::{self, ComponentEdge, CrossEdge, CycleGroup};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Assignment {
    Unassigned,
    Assigned(String),
    Overlapping(Vec<String>),
}

pub(super) struct ComponentMap {
    sets: Vec<(String, GlobSet)>,
}

impl ComponentMap {
    pub(super) fn build(config: &ArchitectureConfig) -> Result<Self> {
        let mut sets = Vec::with_capacity(config.components.len());
        for component in &config.components {
            let mut builder = GlobSetBuilder::new();
            for path in &component.paths {
                builder.add(compile_glob(&component.name, path)?);
            }
            let set = builder.build().map_err(|error| {
                invalid_config(format!(
                    "component {:?} has an invalid path set: {error}",
                    component.name
                ))
            })?;
            sets.push((component.name.clone(), set));
        }
        sets.sort_by(|left, right| left.0.cmp(&right.0));
        Ok(Self { sets })
    }

    pub(super) fn assign(&self, path: &str) -> Assignment {
        match self.assign_checked(path, &mut || Ok::<_, Infallible>(())) {
            Ok(assignment) => assignment,
            Err(never) => match never {},
        }
    }

    pub(super) fn assign_checked<E>(
        &self,
        path: &str,
        check: &mut impl FnMut() -> std::result::Result<(), E>,
    ) -> std::result::Result<Assignment, E> {
        let mut matching = Vec::new();
        for (name, set) in &self.sets {
            check()?;
            if set.is_match(path) {
                matching.push(name.clone());
            }
        }
        check()?;
        Ok(match matching.len() {
            0 => Assignment::Unassigned,
            1 => Assignment::Assigned(matching.remove(0)),
            _ => Assignment::Overlapping(matching),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EdgeCategory<'a> {
    SelfEdge,
    AmbiguousMapping,
    Unmapped,
    IntraComponent,
    CrossComponent { from: &'a str, to: &'a str },
}

fn categorize<'a>(
    edge: &FileEdge,
    source: &'a Assignment,
    destination: &'a Assignment,
) -> EdgeCategory<'a> {
    if edge.src_path == edge.dst_path {
        return EdgeCategory::SelfEdge;
    }
    match (source, destination) {
        (Assignment::Overlapping(_), _) | (_, Assignment::Overlapping(_)) => {
            EdgeCategory::AmbiguousMapping
        }
        (Assignment::Unassigned, _) | (_, Assignment::Unassigned) => EdgeCategory::Unmapped,
        (Assignment::Assigned(from), Assignment::Assigned(to)) if from == to => {
            EdgeCategory::IntraComponent
        }
        (Assignment::Assigned(from), Assignment::Assigned(to)) => {
            EdgeCategory::CrossComponent { from, to }
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct MappingSummary {
    pub(super) file_edges: u64,
    pub(super) self_edges: u64,
    pub(super) ambiguous_mapping_edges: u64,
    pub(super) unmapped_edges: u64,
    pub(super) intra_component_edges: u64,
    pub(super) cross_component_edges: u64,
    pub(super) endpoint_files: u64,
    pub(super) endpoint_files_unassigned: u64,
    pub(super) endpoint_files_overlapping: u64,
    pub(super) overlapping_endpoint_files: Vec<OverlappingFile>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct OverlappingFile {
    pub(super) path: String,
    pub(super) components: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RuleDecision<'a> {
    Forbidden { rule_id: &'a str },
    Allowed { rule_id: &'a str },
    Unconstrained,
}

pub(super) fn decide_rule<'a>(
    config: &'a ArchitectureConfig,
    from: &str,
    to: &str,
) -> RuleDecision<'a> {
    let winner = config
        .rules
        .iter()
        .filter(|rule| (rule.from == from || rule.from == "*") && (rule.to == to || rule.to == "*"))
        .max_by(|left, right| {
            specificity(left)
                .cmp(&specificity(right))
                .then_with(|| {
                    (left.effect == RuleEffect::Forbid).cmp(&(right.effect == RuleEffect::Forbid))
                })
                .then_with(|| right.id.cmp(&left.id))
        });

    match winner {
        Some(rule) if rule.effect == RuleEffect::Forbid => {
            RuleDecision::Forbidden { rule_id: &rule.id }
        }
        Some(rule) => RuleDecision::Allowed { rule_id: &rule.id },
        None => RuleDecision::Unconstrained,
    }
}

fn specificity(rule: &BoundaryRule) -> u8 {
    u8::from(rule.from != "*") + u8::from(rule.to != "*")
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Violation {
    pub(super) rule_id: String,
    pub(super) src_component: String,
    pub(super) dst_component: String,
    pub(super) src_path: String,
    pub(super) dst_path: String,
    pub(super) weight: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ArchitectureAnalysis {
    pub(super) mapping: MappingSummary,
    pub(super) unconstrained_cross_edges: u64,
    pub(super) violations: Vec<Violation>,
    pub(super) component_edges: Vec<ComponentEdge>,
    pub(super) cycle_groups: Vec<CycleGroup>,
}

pub(super) fn analyze(
    set: &FileEdgeSet,
    map: &ComponentMap,
    config: &ArchitectureConfig,
) -> ArchitectureAnalysis {
    let mut endpoints = BTreeMap::new();
    for edge in &set.edges {
        for path in [edge.src_path.as_str(), edge.dst_path.as_str()] {
            endpoints.entry(path).or_insert_with(|| map.assign(path));
        }
    }

    let mut analysis = ArchitectureAnalysis {
        mapping: summarize_endpoints(&endpoints, set.edges.len()),
        ..ArchitectureAnalysis::default()
    };
    let mut cross_edges = Vec::new();
    for edge in &set.edges {
        let source = &endpoints[edge.src_path.as_str()];
        let destination = &endpoints[edge.dst_path.as_str()];
        match categorize(edge, source, destination) {
            EdgeCategory::SelfEdge => analysis.mapping.self_edges += 1,
            EdgeCategory::AmbiguousMapping => analysis.mapping.ambiguous_mapping_edges += 1,
            EdgeCategory::Unmapped => analysis.mapping.unmapped_edges += 1,
            EdgeCategory::IntraComponent => analysis.mapping.intra_component_edges += 1,
            EdgeCategory::CrossComponent { from, to } => {
                analysis.mapping.cross_component_edges += 1;
                match decide_rule(config, from, to) {
                    RuleDecision::Forbidden { rule_id } => analysis.violations.push(Violation {
                        rule_id: rule_id.to_owned(),
                        src_component: from.to_owned(),
                        dst_component: to.to_owned(),
                        src_path: edge.src_path.clone(),
                        dst_path: edge.dst_path.clone(),
                        weight: edge.weight,
                    }),
                    RuleDecision::Unconstrained => analysis.unconstrained_cross_edges += 1,
                    RuleDecision::Allowed { .. } => {}
                }
                cross_edges.push(CrossEdge {
                    src_component: from.to_owned(),
                    dst_component: to.to_owned(),
                    src_path: edge.src_path.clone(),
                    dst_path: edge.dst_path.clone(),
                    weight: edge.weight,
                });
            }
        }
    }

    analysis.violations.sort_by(|left, right| {
        (&left.src_path, &left.dst_path).cmp(&(&right.src_path, &right.dst_path))
    });
    analysis.component_edges = cycles::build_component_edges(&cross_edges);
    analysis.cycle_groups = cycles::find_cycle_groups(&analysis.component_edges);
    analysis
}

#[derive(Default)]
pub(super) struct CycleProjection {
    pub(super) cross_component_edges: u64,
    pub(super) ambiguous_mapping_edges: u64,
    pub(super) unmapped_edges: u64,
    pub(super) groups: Vec<CycleGroup>,
}

/// Use the same assignment and edge categorization as Architecture check.
/// The counterfactual consumes component topology and coverage counts only.
pub(super) fn project_cycles<E>(
    edges: &[FileEdge],
    map: &ComponentMap,
    check: &mut impl FnMut() -> std::result::Result<(), E>,
) -> std::result::Result<CycleProjection, E> {
    let mut endpoints = BTreeMap::new();
    for edge in edges {
        check()?;
        for path in [edge.src_path.as_str(), edge.dst_path.as_str()] {
            if let Entry::Vacant(entry) = endpoints.entry(path) {
                entry.insert(map.assign_checked(path, check)?);
            }
        }
    }

    let mut projection = CycleProjection::default();
    let mut components = BTreeMap::new();
    for edge in edges {
        check()?;
        let source = &endpoints[edge.src_path.as_str()];
        let destination = &endpoints[edge.dst_path.as_str()];
        match categorize(edge, source, destination) {
            EdgeCategory::AmbiguousMapping => projection.ambiguous_mapping_edges += 1,
            EdgeCategory::Unmapped => projection.unmapped_edges += 1,
            EdgeCategory::CrossComponent { from, to } => {
                projection.cross_component_edges += 1;
                let pair = components
                    .entry((from, to))
                    .or_insert_with(|| ComponentEdge::new(from, to));
                pair.weight += u64::from(edge.weight);
            }
            EdgeCategory::SelfEdge | EdgeCategory::IntraComponent => {}
        }
    }
    check()?;
    let component_edges = components.into_values().collect::<Vec<_>>();
    projection.groups = cycles::find_cycle_groups_checked(&component_edges, check)?;
    Ok(projection)
}

fn summarize_endpoints(
    endpoints: &BTreeMap<&str, Assignment>,
    file_edges: usize,
) -> MappingSummary {
    let mut summary = MappingSummary {
        file_edges: file_edges as u64,
        endpoint_files: endpoints.len() as u64,
        ..MappingSummary::default()
    };
    for (&path, assignment) in endpoints {
        match assignment {
            Assignment::Unassigned => summary.endpoint_files_unassigned += 1,
            Assignment::Overlapping(components) => {
                summary.endpoint_files_overlapping += 1;
                summary.overlapping_endpoint_files.push(OverlappingFile {
                    path: path.to_owned(),
                    components: components.clone(),
                });
            }
            Assignment::Assigned(_) => {}
        }
    }
    summary
}
