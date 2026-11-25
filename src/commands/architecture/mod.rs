//! Read-only dependency boundaries and cycle regression over observed call edges.

mod classify;
mod components;
mod config;
mod cycles;
pub(crate) mod proposal;
mod storage;

#[cfg(test)]
mod test_support;
#[cfg(test)]
mod tests;

use std::io;
use std::path::Path;

use miette::Result;
use serde::Serialize;

use crate::cli::args::ArchitectureCommands;
use crate::commands::baseline::{
    BaselineError, Comparability, CompletenessState, EdgeCoverage, FileEdgeSet, compare_edge_sets,
    load_baseline,
};
use crate::commands::helpers::{get_layout, load_ledger_config};
use crate::output::json::emit;
use crate::output::requested_exit::request_exit;
use crate::state::storage::{StorageManager, baselines::validate_baseline_name};

use self::classify::{
    ArchitectureDelta, BaselineVerdictInput, ClassifiedCycleGroup, ClassifiedViolation,
    DeltaCounts, Observation, Regression, RemovedViolation, Status, VerdictInputs, derive_verdicts,
    evaluate_against_baseline,
};
use self::components::{ArchitectureAnalysis, ComponentMap, MappingSummary, Violation, analyze};
use self::config::{
    ArchitectureConfig, ArchitectureError, ConfigSource, DEFAULT_CONFIG_REL, config_digest,
    invalid_config, parse_architecture_toml,
};
use self::cycles::CycleGroup;

pub(crate) fn execute_architecture(
    command: ArchitectureCommands,
    verified_binary_startup: bool,
) -> Result<()> {
    let ArchitectureCommands::Check {
        config,
        baseline,
        json,
        fail,
    } = command
    else {
        unreachable!("architecture proposals dispatch before ordinary setup");
    };
    let layout = get_layout()?;
    let loaded = read_config(layout.root.as_std_path(), config.as_deref())?;
    let map = ComponentMap::build(&loaded.config)?;
    let engine_config = if json {
        // Config loading can warn about unrelated verify steps. A machine check
        // reserves stderr for its own diagnostic and still propagates load errors.
        tracing::subscriber::with_default(tracing::subscriber::NoSubscriber::default(), || {
            load_ledger_config(&layout)
        })
    } else {
        load_ledger_config(&layout)
    }?;
    let has_baseline = baseline.is_some();
    let (current, baseline) =
        storage::read_snapshots(&layout, &engine_config, baseline, verified_binary_startup)?;
    let envelope = build_envelope(EnvelopeInputs {
        loaded,
        map: &map,
        current,
        baseline,
    });

    if json {
        emit(&envelope)?;
    } else {
        print!("{}", render_human(&envelope));
    }

    if fail && fail_requested(has_baseline, envelope.regression, envelope.observation) {
        request_exit(1);
        if has_baseline {
            return Err(miette::miette!(
                "architecture check failed: regression {}",
                envelope.regression.as_str()
            ));
        }
        return Err(miette::miette!(
            "architecture check failed: observation {}",
            envelope.observation.as_str()
        ));
    }
    Ok(())
}

struct LoadedConfig {
    config: ArchitectureConfig,
    metadata: ConfigMetadata,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ConfigMetadata {
    path: String,
    source: ConfigSource,
    digest: String,
    component_count: usize,
    rule_count: usize,
}

fn read_config(root: &Path, explicit: Option<&Path>) -> Result<LoadedConfig> {
    let path = explicit.map_or_else(|| root.join(DEFAULT_CONFIG_REL), Path::to_path_buf);
    let raw = std::fs::read(&path).map_err(|source| {
        if source.kind() == io::ErrorKind::NotFound {
            ArchitectureError::NotFound { path: path.clone() }
        } else {
            ArchitectureError::Unreadable {
                path: path.clone(),
                source,
            }
        }
    })?;
    let text = std::str::from_utf8(&raw)
        .map_err(|error| invalid_config(format!("{} is not UTF-8: {error}", path.display())))?;
    let config = parse_architecture_toml(text)?;
    let (reported_path, source) = match explicit {
        None => (DEFAULT_CONFIG_REL.to_owned(), ConfigSource::Local),
        Some(path) => (
            path.to_str()
                .ok_or_else(|| invalid_config("the explicit config path must be UTF-8"))?
                .to_owned(),
            ConfigSource::TrustedPath,
        ),
    };
    let metadata = ConfigMetadata {
        path: reported_path,
        source,
        digest: config_digest(&raw),
        component_count: config.components.len(),
        rule_count: config.rules.len(),
    };
    Ok(LoadedConfig { config, metadata })
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
enum BaselineStatus {
    Comparable,
    Incomparable,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BaselineMetadata {
    name: String,
    status: BaselineStatus,
    incomparable_reasons: Vec<String>,
    notes: Vec<String>,
}

struct BaselineInput {
    metadata: BaselineMetadata,
    /// Present only when both inputs passed 0601 comparability.
    comparable_set: Option<FileEdgeSet>,
}

struct BaselineObservation {
    name: String,
    loaded: std::result::Result<FileEdgeSet, &'static str>,
}

fn read_baseline(storage: &StorageManager, name: String) -> Result<BaselineObservation> {
    validate_baseline_name(&name)?;
    let loaded = match load_baseline(storage, &name) {
        Ok(set) => Ok(set),
        Err(error) => Err(match error.downcast_ref::<BaselineError>() {
            Some(BaselineError::DigestMismatch { .. }) => "BASELINE_DIGEST_MISMATCH",
            Some(BaselineError::RowSchemaMismatch { .. }) => "SCHEMA_MISMATCH",
            Some(BaselineError::Corrupt { .. }) => "BASELINE_CORRUPT",
            _ => return Err(error),
        }),
    };
    Ok(BaselineObservation { name, loaded })
}

impl BaselineObservation {
    fn evaluate(self, current: &FileEdgeSet) -> BaselineInput {
        match self.loaded {
            Ok(set) => baseline_from_verified(self.name, set, current),
            Err(reason) => {
                let mut reasons = vec![reason.to_owned()];
                if !current.identity.input_completeness.is_complete() {
                    reasons.push("CURRENT_INCOMPLETE".to_owned());
                }
                reasons.sort_unstable();
                BaselineInput {
                    metadata: BaselineMetadata {
                        name: self.name,
                        status: BaselineStatus::Incomparable,
                        incomparable_reasons: reasons,
                        notes: Vec::new(),
                    },
                    comparable_set: None,
                }
            }
        }
    }
}

fn baseline_from_verified(name: String, set: FileEdgeSet, current: &FileEdgeSet) -> BaselineInput {
    let comparison = compare_edge_sets(&set, current);
    let comparable = matches!(comparison.status, Comparability::Comparable);
    let metadata = BaselineMetadata {
        name,
        status: if comparable {
            BaselineStatus::Comparable
        } else {
            BaselineStatus::Incomparable
        },
        incomparable_reasons: comparison.status.reasons(),
        notes: comparison
            .notes
            .into_iter()
            .filter(|note| note == "analyzerBuildDiffers")
            .collect(),
    };
    BaselineInput {
        metadata,
        comparable_set: comparable.then_some(set),
    }
}

struct EnvelopeInputs<'a> {
    loaded: LoadedConfig,
    map: &'a ComponentMap,
    current: FileEdgeSet,
    baseline: Option<BaselineInput>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ArchitectureEnvelope {
    schema_version: u32,
    kind: &'static str,
    config: ConfigMetadata,
    evidence: Evidence,
    status: Status,
    status_reasons: Vec<String>,
    observation: Observation,
    observation_reasons: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    baseline: Option<BaselineMetadata>,
    regression: Regression,
    regression_reasons: Vec<String>,
    #[serde(flatten, skip_serializing_if = "Option::is_none")]
    findings: Option<CurrentFindings>,
    #[serde(flatten, skip_serializing_if = "Option::is_none")]
    comparison: Option<ComparedFindings>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Evidence {
    state: CompletenessState,
    reasons: Vec<String>,
    edge_sources: Vec<String>,
    parser_version: String,
    input_digest: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    index_head_hash: Option<String>,
    coverage: EdgeCoverage,
    evidence_qualification: &'static str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CurrentFindings {
    mapping: MappingSummary,
    unconstrained_cross_edges: u64,
    violations: Vec<ReportedViolation>,
    cycle_groups: Vec<ReportedCycleGroup>,
}

#[derive(Serialize)]
#[serde(untagged)]
enum ReportedViolation {
    Unclassified(Violation),
    Classified(ClassifiedViolation),
}

#[derive(Serialize)]
#[serde(untagged)]
enum ReportedCycleGroup {
    Unclassified(CycleGroup),
    Classified(ClassifiedCycleGroup),
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ComparedFindings {
    removed_violations: Vec<RemovedViolation>,
    removed_cycle_groups: Vec<CycleGroup>,
    counts: DeltaCounts,
}

fn build_envelope(inputs: EnvelopeInputs<'_>) -> ArchitectureEnvelope {
    let EnvelopeInputs {
        loaded,
        map,
        current,
        baseline,
    } = inputs;
    let completeness = &current.identity.input_completeness;
    let complete = completeness.is_complete();
    let baseline_metadata;
    let analyzed_current;
    let delta;
    let baseline_verdict;

    if let Some(BaselineInput {
        metadata,
        comparable_set: Some(set),
    }) = baseline
    {
        let (prior, analyzed, comparison) =
            evaluate_against_baseline(&set, &current, map, &loaded.config);
        baseline_verdict = BaselineVerdictInput::Comparable {
            baseline_coverage: set.coverage,
            baseline_mapping_ambiguous: prior.mapping.ambiguous_mapping_edges,
            baseline_cross_component_edges: prior.mapping.cross_component_edges,
            regressing_findings: comparison.counts.regressing_findings(),
        };
        baseline_metadata = Some(metadata);
        analyzed_current = Some(analyzed);
        delta = Some(comparison);
    } else {
        baseline_metadata = baseline.map(|input| input.metadata);
        baseline_verdict =
            baseline_metadata
                .as_ref()
                .map_or(BaselineVerdictInput::NotRequested, |metadata| {
                    BaselineVerdictInput::Incomparable {
                        reasons: metadata.incomparable_reasons.clone(),
                    }
                });
        analyzed_current = complete.then(|| analyze(&current, map, &loaded.config));
        delta = None;
    }

    let verdicts = derive_verdicts(&VerdictInputs {
        current_completeness_complete: complete,
        current_reasons: completeness.reasons.clone(),
        current_coverage: current.coverage.clone(),
        current_mapping_ambiguous: analyzed_current
            .as_ref()
            .map_or(0, |analysis| analysis.mapping.ambiguous_mapping_edges),
        current_cross_component_edges: analyzed_current
            .as_ref()
            .map_or(0, |analysis| analysis.mapping.cross_component_edges),
        current_findings: analyzed_current.as_ref().map_or(0, |analysis| {
            analysis.violations.len() as u64 + analysis.cycle_groups.len() as u64
        }),
        baseline: baseline_verdict,
    });
    let (findings, comparison) = analyzed_current.map_or((None, None), |analysis| {
        let (findings, comparison) = pack_findings(analysis, delta);
        (Some(findings), comparison)
    });

    let identity = current.identity;
    ArchitectureEnvelope {
        schema_version: 1,
        kind: "architectureCheck",
        config: loaded.metadata,
        evidence: Evidence {
            state: identity.input_completeness.state,
            reasons: identity.input_completeness.reasons,
            edge_sources: identity.edge_sources,
            parser_version: identity.parser_version,
            input_digest: identity.input_digest,
            index_head_hash: identity.index_head_hash,
            coverage: current.coverage,
            evidence_qualification: "unqualified",
        },
        status: verdicts.status,
        status_reasons: verdicts.status_reasons,
        observation: verdicts.observation,
        observation_reasons: verdicts.observation_reasons,
        baseline: baseline_metadata,
        regression: verdicts.regression,
        regression_reasons: verdicts.regression_reasons,
        findings,
        comparison,
    }
}

fn pack_findings(
    analysis: ArchitectureAnalysis,
    delta: Option<ArchitectureDelta>,
) -> (CurrentFindings, Option<ComparedFindings>) {
    let (violations, cycle_groups, comparison) = match delta {
        Some(delta) => (
            delta
                .violations
                .into_iter()
                .map(ReportedViolation::Classified)
                .collect(),
            delta
                .cycle_groups
                .into_iter()
                .map(ReportedCycleGroup::Classified)
                .collect(),
            Some(ComparedFindings {
                removed_violations: delta.removed_violations,
                removed_cycle_groups: delta.removed_cycle_groups,
                counts: delta.counts,
            }),
        ),
        None => (
            analysis
                .violations
                .into_iter()
                .map(ReportedViolation::Unclassified)
                .collect(),
            analysis
                .cycle_groups
                .into_iter()
                .map(ReportedCycleGroup::Unclassified)
                .collect(),
            None,
        ),
    };
    (
        CurrentFindings {
            mapping: analysis.mapping,
            unconstrained_cross_edges: analysis.unconstrained_cross_edges,
            violations,
            cycle_groups,
        },
        comparison,
    )
}

fn fail_requested(has_baseline: bool, regression: Regression, observation: Observation) -> bool {
    if has_baseline {
        regression != Regression::None
    } else {
        observation != Observation::NoneObserved
    }
}

fn render_human(envelope: &ArchitectureEnvelope) -> String {
    use std::fmt::Write as _;

    let mut text = format!(
        "Architecture check\nConfig {} ({})\nEvidence qualification {}\nStatus {}\nObservation {}\nRegression {}\n",
        envelope.config.path,
        envelope.config.digest,
        envelope.evidence.evidence_qualification,
        envelope.status.as_str(),
        envelope.observation.as_str(),
        envelope.regression.as_str(),
    );
    let coverage = &envelope.evidence.coverage;
    let _ = writeln!(
        text,
        "Evidence coverage {} files indexed, {} parse failures, {} resolved cross-file, {} resolved intra-file, {} ambiguous, {} unresolved, {} capped",
        coverage.files_indexed,
        coverage.files_parse_failed,
        coverage.edges_resolved_cross_file,
        coverage.edges_resolved_intra_file,
        coverage.edges_ambiguous,
        coverage.edges_unresolved,
        coverage.edges_capped,
    );
    if let Some(baseline) = &envelope.baseline {
        let status = match baseline.status {
            BaselineStatus::Comparable => "comparable",
            BaselineStatus::Incomparable => "incomparable",
        };
        let _ = writeln!(text, "Baseline {} {status}", baseline.name);
        if !baseline.incomparable_reasons.is_empty() {
            let _ = writeln!(
                text,
                "Baseline incomparable reasons {}",
                baseline.incomparable_reasons.join(", "),
            );
        }
    }
    for (label, reasons) in [
        ("Status", &envelope.status_reasons),
        ("Observation", &envelope.observation_reasons),
        ("Regression", &envelope.regression_reasons),
    ] {
        if !reasons.is_empty() {
            let _ = writeln!(text, "{label} reasons {}", reasons.join(", "));
        }
    }
    if let Some(findings) = &envelope.findings {
        let _ = writeln!(
            text,
            "Cross-component edges {} ({} unconstrained)\nViolations {}\nCycle groups {}",
            findings.mapping.cross_component_edges,
            findings.unconstrained_cross_edges,
            findings.violations.len(),
            findings.cycle_groups.len(),
        );
    }
    if let Some(compared) = &envelope.comparison {
        let violations = &compared.counts.violations;
        let groups = &compared.counts.cycle_groups;
        let _ = writeln!(
            text,
            "Violations by class — {} new, {} worsened, {} existing, {} removed\nCycle groups by class — {} introduced, {} grown, {} unchanged, {} shrunk, {} removed",
            violations.new,
            violations.worsened,
            violations.existing,
            violations.removed,
            groups.introduced,
            groups.grown,
            groups.unchanged,
            groups.shrunk,
            groups.removed,
        );
    }
    text
}
