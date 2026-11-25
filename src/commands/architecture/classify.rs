use std::collections::{BTreeMap, BTreeSet};
use std::convert::Infallible;

#[cfg(test)]
#[path = "classify_tests.rs"]
mod tests;

use serde::Serialize;

use crate::commands::baseline::{EdgeCoverage, FileEdgeSet};

use super::components::{ArchitectureAnalysis, ComponentMap, Violation, analyze};
use super::config::ArchitectureConfig;
use super::cycles::CycleGroup;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) enum ViolationClass {
    New,
    Worsened,
    Existing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) enum CycleClass {
    Introduced,
    Grown,
    Unchanged,
    Shrunk,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ClassifiedViolation {
    #[serde(flatten)]
    pub(super) violation: Violation,
    pub(super) class: ViolationClass,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) baseline_weight: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RemovedViolation {
    pub(super) rule_id: String,
    pub(super) src_component: String,
    pub(super) dst_component: String,
    pub(super) src_path: String,
    pub(super) dst_path: String,
    pub(super) baseline_weight: u32,
}

impl From<&Violation> for RemovedViolation {
    fn from(violation: &Violation) -> Self {
        Self {
            rule_id: violation.rule_id.clone(),
            src_component: violation.src_component.clone(),
            dst_component: violation.dst_component.clone(),
            src_path: violation.src_path.clone(),
            dst_path: violation.dst_path.clone(),
            baseline_weight: violation.weight,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ClassifiedCycleGroup {
    #[serde(flatten)]
    pub(super) group: CycleGroup,
    pub(super) class: CycleClass,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub(super) struct ViolationCounts {
    pub(super) new: u64,
    pub(super) worsened: u64,
    pub(super) existing: u64,
    pub(super) removed: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub(super) struct CycleCounts {
    pub(super) introduced: u64,
    pub(super) grown: u64,
    pub(super) unchanged: u64,
    pub(super) shrunk: u64,
    pub(super) removed: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct DeltaCounts {
    pub(super) violations: ViolationCounts,
    pub(super) cycle_groups: CycleCounts,
}

impl DeltaCounts {
    pub(super) fn regressing_findings(&self) -> u64 {
        self.violations.new
            + self.violations.worsened
            + self.cycle_groups.introduced
            + self.cycle_groups.grown
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct ArchitectureDelta {
    pub(super) violations: Vec<ClassifiedViolation>,
    pub(super) removed_violations: Vec<RemovedViolation>,
    pub(super) cycle_groups: Vec<ClassifiedCycleGroup>,
    pub(super) removed_cycle_groups: Vec<CycleGroup>,
    pub(super) counts: DeltaCounts,
}

pub(super) fn evaluate_against_baseline(
    baseline: &FileEdgeSet,
    current: &FileEdgeSet,
    map: &ComponentMap,
    config: &ArchitectureConfig,
) -> (
    ArchitectureAnalysis,
    ArchitectureAnalysis,
    ArchitectureDelta,
) {
    let baseline = analyze(baseline, map, config);
    let current = analyze(current, map, config);
    let delta = classify(&baseline, &current);
    (baseline, current, delta)
}

pub(super) fn classify(
    baseline: &ArchitectureAnalysis,
    current: &ArchitectureAnalysis,
) -> ArchitectureDelta {
    let mut delta = ArchitectureDelta::default();
    classify_violations(&baseline.violations, &current.violations, &mut delta);
    classify_cycle_groups(&baseline.cycle_groups, &current.cycle_groups, &mut delta);
    delta
}

fn violation_key(violation: &Violation) -> (&str, &str) {
    (&violation.src_path, &violation.dst_path)
}

fn classify_violations(
    baseline: &[Violation],
    current: &[Violation],
    delta: &mut ArchitectureDelta,
) {
    let mut earlier: BTreeMap<_, _> = baseline
        .iter()
        .map(|violation| (violation_key(violation), violation))
        .collect();
    for violation in current {
        let previous = earlier.remove(&violation_key(violation));
        let class = match previous {
            None => {
                delta.counts.violations.new += 1;
                ViolationClass::New
            }
            Some(old) if old.weight < violation.weight => {
                delta.counts.violations.worsened += 1;
                ViolationClass::Worsened
            }
            Some(_) => {
                delta.counts.violations.existing += 1;
                ViolationClass::Existing
            }
        };
        delta.violations.push(ClassifiedViolation {
            violation: violation.clone(),
            class,
            baseline_weight: previous.map(|old| old.weight),
        });
    }
    delta.removed_violations = earlier.into_values().map(RemovedViolation::from).collect();
    delta.counts.violations.removed = delta.removed_violations.len() as u64;
}

fn classify_cycle_groups(
    baseline: &[CycleGroup],
    current: &[CycleGroup],
    delta: &mut ArchitectureDelta,
) {
    let check = &mut || Ok::<_, Infallible>(());
    let baseline_groups_by_member = match index_group_members(baseline, check) {
        Ok(groups) => groups,
        Err(never) => match never {},
    };

    let mut represented = vec![false; baseline.len()];
    for group in current {
        let class =
            match classify_cycle_group(baseline, &baseline_groups_by_member, group, check, |id| {
                represented[id] = true
            }) {
                Ok(class) => class,
                Err(never) => match never {},
            };
        match class {
            CycleClass::Introduced => delta.counts.cycle_groups.introduced += 1,
            CycleClass::Grown => delta.counts.cycle_groups.grown += 1,
            CycleClass::Unchanged => delta.counts.cycle_groups.unchanged += 1,
            CycleClass::Shrunk => delta.counts.cycle_groups.shrunk += 1,
        }
        delta.cycle_groups.push(ClassifiedCycleGroup {
            group: group.clone(),
            class,
        });
    }

    delta.removed_cycle_groups = baseline
        .iter()
        .zip(represented)
        .filter(|(_, seen)| !seen)
        .map(|(group, _)| group.clone())
        .collect();
    delta.counts.cycle_groups.removed = delta.removed_cycle_groups.len() as u64;
}

pub(super) fn projected_cycle_change<'a, E>(
    baseline: &[CycleGroup],
    current: &'a [CycleGroup],
    endpoints: [&str; 2],
    check: &mut impl FnMut() -> Result<(), E>,
) -> Result<Option<(CycleClass, &'a [String])>, E> {
    for group in current {
        check()?;
        if endpoints
            .iter()
            .all(|endpoint| group.members.iter().any(|member| member == endpoint))
        {
            let members = index_group_members(baseline, check)?;
            let class = classify_cycle_group(baseline, &members, group, check, |_| {})?;
            return Ok(match class {
                CycleClass::Introduced | CycleClass::Grown => {
                    Some((class, group.members.as_slice()))
                }
                CycleClass::Unchanged | CycleClass::Shrunk => None,
            });
        }
    }
    Ok(None)
}

fn index_group_members<'a, E>(
    baseline: &'a [CycleGroup],
    check: &mut impl FnMut() -> Result<(), E>,
) -> Result<BTreeMap<&'a str, usize>, E> {
    let mut groups = BTreeMap::new();
    for (id, group) in baseline.iter().enumerate() {
        for member in &group.members {
            check()?;
            groups.insert(member.as_str(), id);
        }
    }
    Ok(groups)
}

fn classify_cycle_group<E>(
    baseline: &[CycleGroup],
    baseline_groups_by_member: &BTreeMap<&str, usize>,
    group: &CycleGroup,
    check: &mut impl FnMut() -> Result<(), E>,
    mut on_overlap: impl FnMut(usize),
) -> Result<CycleClass, E> {
    let mut overlaps = BTreeSet::new();
    let mut has_added_member = false;
    for member in &group.members {
        check()?;
        if let Some(&id) = baseline_groups_by_member.get(member.as_str()) {
            overlaps.insert(id);
            on_overlap(id);
        } else {
            has_added_member = true;
        }
    }
    match overlaps.first().copied() {
        None => Ok(CycleClass::Introduced),
        Some(_) if has_added_member || overlaps.len() > 1 => Ok(CycleClass::Grown),
        Some(id) => classify_overlapping_group(&baseline[id], group, check),
    }
}

fn internal_pairs<'a, E>(
    group: &'a CycleGroup,
    check: &mut impl FnMut() -> Result<(), E>,
) -> Result<BTreeSet<(&'a str, &'a str)>, E> {
    let mut pairs = BTreeSet::new();
    for edge in &group.component_edges {
        check()?;
        pairs.insert((edge.src.as_str(), edge.dst.as_str()));
    }
    Ok(pairs)
}

fn classify_overlapping_group<E>(
    baseline: &CycleGroup,
    current: &CycleGroup,
    check: &mut impl FnMut() -> Result<(), E>,
) -> Result<CycleClass, E> {
    if current.members.len() < baseline.members.len() {
        // The caller established that every current member belongs to this one baseline group.
        return Ok(CycleClass::Shrunk);
    }
    let before = internal_pairs(baseline, check)?;
    let after = internal_pairs(current, check)?;
    check()?;
    let class = if before == after {
        CycleClass::Unchanged
    } else if after.is_subset(&before) {
        CycleClass::Shrunk
    } else {
        CycleClass::Grown
    };
    check()?;
    Ok(class)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) enum Status {
    Evaluated,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) enum Observation {
    Findings,
    NoneObserved,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) enum Regression {
    Present,
    None,
    Unknown,
    NotEvaluated,
}

impl Status {
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::Evaluated => "evaluated",
            Self::Unknown => "unknown",
        }
    }
}

impl Observation {
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::Findings => "findings",
            Self::NoneObserved => "noneObserved",
            Self::Unknown => "unknown",
        }
    }
}

impl Regression {
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::Present => "present",
            Self::None => "none",
            Self::Unknown => "unknown",
            Self::NotEvaluated => "notEvaluated",
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct VerdictInputs {
    pub(super) current_completeness_complete: bool,
    pub(super) current_reasons: Vec<String>,
    pub(super) current_coverage: EdgeCoverage,
    pub(super) current_mapping_ambiguous: u64,
    pub(super) current_cross_component_edges: u64,
    pub(super) current_findings: u64,
    pub(super) baseline: BaselineVerdictInput,
}

#[derive(Debug, Clone)]
pub(super) enum BaselineVerdictInput {
    NotRequested,
    Incomparable {
        reasons: Vec<String>,
    },
    Comparable {
        baseline_coverage: EdgeCoverage,
        baseline_mapping_ambiguous: u64,
        baseline_cross_component_edges: u64,
        regressing_findings: u64,
    },
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct Verdicts {
    pub(super) status: Status,
    pub(super) status_reasons: Vec<String>,
    pub(super) observation: Observation,
    pub(super) observation_reasons: Vec<String>,
    pub(super) regression: Regression,
    pub(super) regression_reasons: Vec<String>,
}

const NO_CROSS_EDGES: &str = "NO_MAPPED_CROSS_COMPONENT_EDGES";
const OVERLAP: &str = "OVERLAPPING_COMPONENTS";

pub(super) fn observation_reasons(
    cross_component_edges: u64,
    ambiguous_mapping_edges: u64,
    coverage: &EdgeCoverage,
) -> Vec<String> {
    let mut reasons = Vec::new();
    add_reason(&mut reasons, cross_component_edges == 0, NO_CROSS_EDGES);
    add_reason(&mut reasons, ambiguous_mapping_edges > 0, OVERLAP);
    add_reason(
        &mut reasons,
        coverage.edges_ambiguous > 0 || coverage.edges_unresolved > 0,
        "UNRESOLVED_PRESENT",
    );
    reasons
}

pub(super) fn derive_verdicts(inputs: &VerdictInputs) -> Verdicts {
    let mut status_reasons = Vec::new();
    let mut observation_reasons = Vec::new();
    let (status, observation) = if inputs.current_completeness_complete {
        observation_reasons = self::observation_reasons(
            inputs.current_cross_component_edges,
            inputs.current_mapping_ambiguous,
            &inputs.current_coverage,
        );
        let observation = if inputs.current_findings > 0 {
            Observation::Findings
        } else if observation_reasons.is_empty() {
            Observation::NoneObserved
        } else {
            Observation::Unknown
        };
        (Status::Evaluated, observation)
    } else {
        status_reasons.clone_from(&inputs.current_reasons);
        (Status::Unknown, Observation::Unknown)
    };
    let (regression, mut regression_reasons) = derive_regression(inputs);
    for reasons in [
        &mut status_reasons,
        &mut observation_reasons,
        &mut regression_reasons,
    ] {
        reasons.sort_unstable();
        reasons.dedup();
    }

    Verdicts {
        status,
        status_reasons,
        observation,
        observation_reasons,
        regression,
        regression_reasons,
    }
}

fn derive_regression(inputs: &VerdictInputs) -> (Regression, Vec<String>) {
    let (
        baseline_coverage,
        baseline_mapping_ambiguous,
        baseline_cross_component_edges,
        regressing_findings,
    ) = match &inputs.baseline {
        BaselineVerdictInput::NotRequested => return (Regression::NotEvaluated, Vec::new()),
        BaselineVerdictInput::Incomparable { reasons } => {
            return (Regression::Unknown, reasons.clone());
        }
        BaselineVerdictInput::Comparable {
            baseline_coverage,
            baseline_mapping_ambiguous,
            baseline_cross_component_edges,
            regressing_findings,
        } => (
            baseline_coverage,
            *baseline_mapping_ambiguous,
            *baseline_cross_component_edges,
            *regressing_findings,
        ),
    };
    if inputs.current_completeness_complete && regressing_findings > 0 {
        return (Regression::Present, Vec::new());
    }

    let mut reasons = Vec::new();
    add_reason(
        &mut reasons,
        inputs.current_coverage.edges_ambiguous > baseline_coverage.edges_ambiguous,
        "AMBIGUOUS_GREW",
    );
    add_reason(
        &mut reasons,
        !inputs.current_completeness_complete,
        "CURRENT_INCOMPLETE",
    );
    add_reason(
        &mut reasons,
        inputs.current_cross_component_edges == 0 || baseline_cross_component_edges == 0,
        NO_CROSS_EDGES,
    );
    add_reason(
        &mut reasons,
        inputs.current_mapping_ambiguous > 0 || baseline_mapping_ambiguous > 0,
        OVERLAP,
    );
    add_reason(
        &mut reasons,
        inputs.current_coverage.edges_unresolved > baseline_coverage.edges_unresolved,
        "UNRESOLVED_GREW",
    );
    let regression = if reasons.is_empty() {
        Regression::None
    } else {
        Regression::Unknown
    };
    (regression, reasons)
}

fn add_reason(reasons: &mut Vec<String>, active: bool, reason: &str) {
    if active {
        reasons.push(reason.to_owned());
    }
}
