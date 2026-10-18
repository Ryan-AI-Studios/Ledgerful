use crate::contracts::AffectedContract;
use crate::index::env_schema::EnvVarDep;
use crate::observability::signal::ObservabilitySignal;
use crate::util::clock::Clock;
use chrono::Utc;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ImpactPacket {
    pub schema_version: String,
    pub timestamp_utc: String, // ISO 8601 string
    pub head_hash: Option<String>,
    pub branch_name: Option<String>,
    #[serde(default)]
    pub tree_clean: bool,
    pub risk_level: super::RiskLevel,
    pub risk_reasons: Vec<String>,
    pub changes: Vec<super::ChangedFile>,
    pub temporal_couplings: Vec<super::TemporalCoupling>,
    /// Change-filtered audit pairs (leaves, governance files, and tree partners).
    /// Omitted when empty. Honesty list; not the default risk list.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub temporal_couplings_audit: Vec<super::TemporalCoupling>,
    /// Tree paths that appear on a change-filtered audit pair before truncation.
    /// Omitted when empty. Not truncated with the coupling lists.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub discarded_tree_paths: Vec<std::path::PathBuf>,
    pub structural_couplings: Vec<super::StructuralCoupling>,
    /// Structural call-graph blast radius (≠ deploy `highBlastResources`).
    /// Omitted from JSON when empty.
    #[serde(default, skip_serializing_if = "super::blast_radius_skip")]
    pub blast_radius: Option<super::BlastRadius>,
    pub centrality_risks: Vec<super::CentralityRisk>,
    #[serde(default)]
    pub logging_coverage_delta: Vec<super::CoverageDelta>,
    #[serde(default)]
    pub error_handling_delta: Vec<super::CoverageDelta>,
    #[serde(default)]
    pub telemetry_coverage_delta: Vec<super::CoverageDelta>,
    #[serde(default)]
    pub infrastructure_dirs: Vec<String>,
    #[serde(default)]
    pub env_var_deps: Vec<EnvVarDep>,
    #[serde(default)]
    pub test_coverage: Vec<super::TestCoverage>,
    /// Change-set test-gap summary (0115). Omitted when not computed.
    /// Empty `test_coverage` vec is **not** full cover — use this status field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub test_gaps: Option<crate::impact::enrichment::test_gaps::TestGapsReport>,
    /// Change-set affected HTTP flows (0118). Omitted when not computed.
    /// `available` + flowCount 0 means no registered routes touched (success).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub affected_flows: Option<crate::impact::enrichment::affected_flows::AffectedFlowsReport>,
    #[serde(default)]
    pub runtime_usage_delta: Vec<super::RuntimeUsageDelta>,
    /// Function-signature deltas (not Ed25519). Empty omitted from JSON.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub signature_deltas: Vec<super::SignatureDelta>,
    pub hotspots: Vec<super::Hotspot>,
    pub verification_results: Vec<super::VerificationResult>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub relevant_decisions: Vec<super::RelevantDecision>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub observability: Vec<ObservabilitySignal>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub affected_contracts: Vec<AffectedContract>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ai_insights: Vec<super::AiInsight>,
    #[serde(default)]
    pub data_flow_matches: Vec<super::DataFlowMatch>,
    #[serde(default)]
    pub service_map_delta: Option<super::ServiceMapDelta>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub trace_config_drift: Vec<super::TraceConfigChange>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub trace_env_vars: Vec<super::TraceEnvVarChange>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sdk_dependencies_delta: Option<super::SdkDependencyDelta>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deploy_manifest_changes: Vec<super::DeployManifestChange>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ci_config_change: Option<super::CiConfigChange>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ci_predictions: Vec<super::CIPrediction>,
    #[serde(default)]
    pub knowledge_graph: Vec<super::KGImpact>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub service_impact: Vec<super::ServiceImpact>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub analysis_warnings: Vec<String>,
    /// Additive history-walk honesty (0308). Omitted when the walk completed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completeness: Option<crate::impact::budget::AnalysisCompleteness>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dead_code_findings: Vec<super::DeadCodeFinding>,
    /// Path mode for temporal demotion: `"code"` (default) or `"all"` (0173).
    /// Always serialized for honesty when the packet is emitted after analysis.
    #[serde(default = "default_path_mode")]
    pub path_mode: String,
    /// Count of temporal couplings at/above risk threshold demoted under `path_mode` (0173).
    #[serde(default)]
    pub demoted_temporal_count: u32,
    /// How structural changes were sourced: `working_tree` | `base_ref` | `prospective` (0173).
    #[serde(default = "default_analysis_mode")]
    pub analysis_mode: String,
    /// Normalized prospective paths when `analysis_mode == "prospective"` (0173).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub prospective_paths: Vec<String>,
    /// Docs-mode punchlist (0227). Omitted when empty. Cap 5, sorted. Does not
    /// replace `temporalCouplings`. Never includes `agentSummary` (change-context only).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actionable_lead: Vec<crate::impact::lead::ActionableLeadItem>,
    /// Inline explanations for `no_source_seeds` / mapped=0 (0227 docs mode).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub glossary: Option<std::collections::BTreeMap<String, String>>,
    /// Window label for the embedded hotspot list (0503). Omitted when the
    /// hotspot provider did not finish a calculation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hotspot_provenance: Option<crate::impact::budget::HotspotProvenance>,
    /// Prospective scope sentence (0503). Omitted unless `analysis_mode` is
    /// `prospective` and `tree_clean` is false.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope_note: Option<String>,
}

fn default_path_mode() -> String {
    "code".to_string()
}

fn default_analysis_mode() -> String {
    "working_tree".to_string()
}

/// Packet text exactly as the Ask prompt embeds it: `ai_insights` fenced,
/// pretty JSON, backticks escaped. `build_user_prompt` and the
/// `truncate_for_context` meter both call this, so they cannot drift.
///
/// This is the one place `impact` reaches into `crate::ai`; `ai::escape` is a
/// std-only leaf, so there is no cycle.
pub(crate) fn prompt_packet_text(packet: &ImpactPacket) -> Result<String, serde_json::Error> {
    let mut view = packet.clone();
    for insight in &mut view.ai_insights {
        insight.content = crate::ai::fence_bridge_insight(&insight.content);
    }
    serde_json::to_string_pretty(&view).map(|json| crate::ai::escape_code_chunk(&json))
}

/// Bytes a serialized field contributes to the prompt when nested in `depth`
/// containers. `None` on serialize error; the running length is left unchanged
/// so a failed field cannot look under-budget.
///
/// The standalone pretty text indents inner lines from zero; nested in `depth`
/// containers every line after the first sits `2 * depth` spaces further in
/// (pretty JSON newlines are structural, string newlines are escaped). Each
/// backtick grows from 1 byte to 2 under `escape_code_chunk`.
fn field_prompt_len<T: Serialize>(value: &T, depth: usize) -> Option<usize> {
    let text = serde_json::to_string_pretty(value).ok()?;
    let newlines = text.bytes().filter(|&b| b == b'\n').count();
    let backticks = text.bytes().filter(|&b| b == b'`').count();
    Some(
        text.len()
            .saturating_add(newlines.saturating_mul(2 * depth))
            .saturating_add(backticks),
    )
}

#[cfg(test)]
thread_local! {
    static FULL_PACKET_SERIALIZE_COUNT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static FORCE_PROMPT_SERIALIZE_ERR: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static LAST_CONFIRM: std::cell::Cell<Option<(usize, usize)>> = const { std::cell::Cell::new(None) };
}

/// Full-packet prompt text length in bytes. The test meter increments only
/// here, including `Err` (the call still happened).
pub(crate) fn prompt_packet_len(packet: &ImpactPacket) -> Result<usize, serde_json::Error> {
    #[cfg(test)]
    {
        FULL_PACKET_SERIALIZE_COUNT.with(|c| c.set(c.get().saturating_add(1)));
        // serde_json 1.0.151 writes value NaN/Inf as JSON null; this hook
        // still exercises the production Err path (warn + over-budget).
        if FORCE_PROMPT_SERIALIZE_ERR.with(|c| c.replace(false)) {
            return Err(serde::ser::Error::custom("NaN is not allowed in JSON"));
        }
    }
    prompt_packet_text(packet).map(|s| s.len())
}

#[cfg(test)]
fn reset_truncate_serialize_count() {
    FULL_PACKET_SERIALIZE_COUNT.with(|c| c.set(0));
    LAST_CONFIRM.with(|c| c.set(None));
}

#[cfg(test)]
fn take_truncate_serialize_count() -> usize {
    FULL_PACKET_SERIALIZE_COUNT.with(|c| c.replace(0))
}

/// `(running, actual)` of the last confirm serialize, for drift tests.
#[cfg(test)]
pub(crate) fn take_last_confirm() -> Option<(usize, usize)> {
    LAST_CONFIRM.with(|c| c.take())
}

/// Second full serialize after a strip. It compares nothing for control flow;
/// it only warns when the running estimate fell below the true length.
fn confirm_prompt_len(packet: &ImpactPacket, running: usize) {
    match prompt_packet_len(packet) {
        Ok(actual) => {
            #[cfg(test)]
            LAST_CONFIRM.with(|c| c.set(Some((running, actual))));
            if running < actual {
                tracing::warn!(
                    running,
                    actual,
                    "impact packet prompt-length estimate under-counted"
                );
            }
        }
        Err(err) => {
            tracing::warn!(
                error = %err,
                "impact packet prompt serialize failed; treating as over-budget"
            );
        }
    }
}

fn debit_running<T: Serialize>(running: &mut usize, old: &T, new: &T, depth: usize) {
    let (Some(old_len), Some(new_len)) =
        (field_prompt_len(old, depth), field_prompt_len(new, depth))
    else {
        return;
    };
    *running = running.saturating_sub(old_len).saturating_add(new_len);
}

fn debit_clear_vec<T: Serialize>(running: &mut usize, field: &mut Vec<T>, depth: usize) {
    if field.is_empty() {
        return;
    }
    let old_len = field_prompt_len(field, depth);
    field.clear();
    if let (Some(old_len), Some(new_len)) = (old_len, field_prompt_len(field, depth)) {
        *running = running.saturating_sub(old_len).saturating_add(new_len);
    }
}

fn debit_take_opt<T: Serialize>(running: &mut usize, field: &mut Option<T>, depth: usize) {
    if field.is_none() {
        return;
    }
    let old = std::mem::take(field);
    if let (Some(old_len), Some(new_len)) = (
        field_prompt_len(&old, depth),
        field_prompt_len(field, depth),
    ) {
        *running = running.saturating_sub(old_len).saturating_add(new_len);
    }
}

/// `ai_insights` contribute their fenced view to the prompt, so the debit
/// measures that, never the raw content.
fn debit_clear_ai_insights(running: &mut usize, field: &mut Vec<super::AiInsight>, depth: usize) {
    if field.is_empty() {
        return;
    }
    let fenced: Vec<super::AiInsight> = field
        .iter()
        .map(|insight| super::AiInsight {
            content: crate::ai::fence_bridge_insight(&insight.content),
            ..insight.clone()
        })
        .collect();
    let old_len = field_prompt_len(&fenced, depth);
    field.clear();
    if let (Some(old_len), Some(new_len)) = (old_len, field_prompt_len(field, depth)) {
        *running = running.saturating_sub(old_len).saturating_add(new_len);
    }
}

/// `blast_radius_skip` omits a `Some(empty)` value from the output, so it was
/// never in the starting measure and must not be debited.
fn debit_take_blast_radius(
    running: &mut usize,
    field: &mut Option<super::BlastRadius>,
    depth: usize,
) {
    let was_serialized = !super::blast_radius_skip(field);
    if !was_serialized {
        *field = None;
        return;
    }
    debit_take_opt(running, field, depth);
}

impl ImpactPacket {
    pub fn is_empty(&self) -> bool {
        self.changes.is_empty()
            && self.temporal_couplings.is_empty()
            && self.temporal_couplings_audit.is_empty()
            && self.discarded_tree_paths.is_empty()
            && self.structural_couplings.is_empty()
            && self.blast_radius.is_none()
            && self.centrality_risks.is_empty()
            && self.logging_coverage_delta.is_empty()
            && self.error_handling_delta.is_empty()
            && self.telemetry_coverage_delta.is_empty()
            && self.infrastructure_dirs.is_empty()
            && self.env_var_deps.is_empty()
            && self.test_coverage.is_empty()
            && self.test_gaps.is_none()
            && self.affected_flows.is_none()
            && self.runtime_usage_delta.is_empty()
            && self.signature_deltas.is_empty()
            && self.hotspots.is_empty()
            && self.verification_results.is_empty()
            && self.relevant_decisions.is_empty()
            && self.observability.is_empty()
            && self.affected_contracts.is_empty()
            && self.ai_insights.is_empty()
            && self.data_flow_matches.is_empty()
            && self.service_map_delta.is_none()
            && self.trace_config_drift.is_empty()
            && self.trace_env_vars.is_empty()
            && self.sdk_dependencies_delta.is_none()
            && self.deploy_manifest_changes.is_empty()
            && self.ci_config_change.is_none()
            && self.ci_predictions.is_empty()
            && self.knowledge_graph.is_empty()
            && self.service_impact.is_empty()
            && self.analysis_warnings.is_empty()
            && self.dead_code_findings.is_empty()
            && self.prospective_paths.is_empty()
            && self.hotspot_provenance.is_none()
            && self.scope_note.is_none()
        // path_mode / demoted_temporal_count / analysis_mode / actionable_lead /
        // glossary are metadata or presentation overlays — they do not make a
        // clean packet "non-empty" for empty-tree checks.
    }
}

impl Default for ImpactPacket {
    fn default() -> Self {
        Self {
            schema_version: "v1".to_string(),
            timestamp_utc: Utc::now().to_rfc3339(),
            head_hash: None,
            branch_name: None,
            tree_clean: false,
            risk_level: super::RiskLevel::Medium,
            risk_reasons: Vec::new(),
            changes: Vec::new(),
            temporal_couplings: Vec::new(),
            temporal_couplings_audit: Vec::new(),
            discarded_tree_paths: Vec::new(),
            structural_couplings: Vec::new(),
            blast_radius: None,
            centrality_risks: Vec::new(),
            logging_coverage_delta: Vec::new(),
            error_handling_delta: Vec::new(),
            telemetry_coverage_delta: Vec::new(),
            infrastructure_dirs: Vec::new(),
            env_var_deps: Vec::new(),
            test_coverage: Vec::new(),
            test_gaps: None,
            affected_flows: None,
            runtime_usage_delta: Vec::new(),
            signature_deltas: Vec::new(),
            hotspots: Vec::new(),
            verification_results: Vec::new(),
            relevant_decisions: Vec::new(),
            observability: Vec::new(),
            affected_contracts: Vec::new(),
            ai_insights: Vec::new(),
            service_map_delta: None,
            data_flow_matches: Vec::new(),
            trace_config_drift: Vec::new(),
            trace_env_vars: Vec::new(),
            sdk_dependencies_delta: None,
            deploy_manifest_changes: Vec::new(),
            ci_config_change: None,
            ci_predictions: Vec::new(),
            knowledge_graph: Vec::new(),
            service_impact: Vec::new(),
            analysis_warnings: Vec::new(),
            completeness: None,
            dead_code_findings: Vec::new(),
            path_mode: default_path_mode(),
            demoted_temporal_count: 0,
            analysis_mode: default_analysis_mode(),
            prospective_paths: Vec::new(),
            actionable_lead: Vec::new(),
            glossary: None,
            hotspot_provenance: None,
            scope_note: None,
        }
    }
}

impl ImpactPacket {
    pub fn with_clock(clock: &dyn Clock) -> Self {
        Self {
            timestamp_utc: clock.now().to_rfc3339(),
            ..Self::default()
        }
    }

    pub fn set_affected_contracts(&mut self, affected_contracts: Vec<AffectedContract>) {
        self.affected_contracts = affected_contracts;
    }

    pub fn set_trace_config_drift(&mut self, trace_config_drift: Vec<super::TraceConfigChange>) {
        self.trace_config_drift = trace_config_drift;
    }

    pub fn set_trace_env_vars(&mut self, trace_env_vars: Vec<super::TraceEnvVarChange>) {
        self.trace_env_vars = trace_env_vars;
    }

    pub fn set_sdk_dependencies_delta(
        &mut self,
        sdk_dependencies_delta: Option<super::SdkDependencyDelta>,
    ) {
        self.sdk_dependencies_delta = sdk_dependencies_delta;
    }

    pub fn set_data_flow_matches(&mut self, data_flow_matches: Vec<super::DataFlowMatch>) {
        self.data_flow_matches = data_flow_matches;
    }

    pub fn set_ci_config_change(&mut self, ci_config_change: Option<super::CiConfigChange>) {
        self.ci_config_change = ci_config_change;
    }

    pub fn set_deploy_manifest_changes(
        &mut self,
        deploy_manifest_changes: Vec<super::DeployManifestChange>,
    ) {
        self.deploy_manifest_changes = deploy_manifest_changes;
    }

    pub fn set_hotspots(&mut self, hotspots: Vec<super::Hotspot>) {
        self.hotspots = hotspots;
    }

    pub fn set_test_coverage(&mut self, test_coverage: Vec<super::TestCoverage>) {
        self.test_coverage = test_coverage;
    }

    pub fn set_test_gaps(
        &mut self,
        test_gaps: Option<crate::impact::enrichment::test_gaps::TestGapsReport>,
    ) {
        self.test_gaps = test_gaps;
    }

    pub fn set_blast_radius(&mut self, blast_radius: Option<super::BlastRadius>) {
        self.blast_radius = blast_radius;
    }

    pub fn set_structural_couplings(
        &mut self,
        structural_couplings: Vec<super::StructuralCoupling>,
    ) {
        self.structural_couplings = structural_couplings;
    }

    pub fn set_affected_flows(
        &mut self,
        affected_flows: Option<crate::impact::enrichment::affected_flows::AffectedFlowsReport>,
    ) {
        self.affected_flows = affected_flows;
    }

    pub fn set_temporal_couplings(&mut self, temporal_couplings: Vec<super::TemporalCoupling>) {
        self.temporal_couplings = temporal_couplings;
    }

    pub fn set_relevant_decisions(&mut self, relevant_decisions: Vec<super::RelevantDecision>) {
        self.relevant_decisions = relevant_decisions;
    }

    pub fn set_runtime_usage_delta(&mut self, runtime_usage_delta: Vec<super::RuntimeUsageDelta>) {
        self.runtime_usage_delta = runtime_usage_delta;
    }

    pub fn set_risk_level(&mut self, risk_level: super::RiskLevel) {
        self.risk_level = risk_level;
    }

    pub fn set_service_map_delta(&mut self, service_map_delta: Option<super::ServiceMapDelta>) {
        self.service_map_delta = service_map_delta;
    }

    pub fn set_signature_deltas(&mut self, signature_deltas: Vec<super::SignatureDelta>) {
        self.signature_deltas = signature_deltas;
    }

    /// Finalizes the packet by sorting all internal collections deterministically.
    pub fn finalize(&mut self) {
        self.risk_reasons.sort_unstable();
        self.analysis_warnings.sort_unstable();
        self.analysis_warnings.dedup();

        for file in &mut self.changes {
            if let Some(ref mut symbols) = file.symbols {
                symbols.sort_unstable();
            }
            if let Some(ref mut imports) = file.imports {
                imports.imported_from.sort_unstable();
                imports.exported_symbols.sort_unstable();
            }
            if let Some(ref mut runtime_usage) = file.runtime_usage {
                runtime_usage.env_vars.sort_unstable();
                runtime_usage.config_keys.sort_unstable();
            }
            file.analysis_warnings.sort_unstable();
            file.analysis_warnings.dedup();
        }
        self.changes.sort_unstable();
        self.temporal_couplings.sort_unstable();
        self.temporal_couplings_audit.sort_unstable();
        self.discarded_tree_paths.sort_unstable();
        self.discarded_tree_paths.dedup();
        self.structural_couplings.sort_unstable();
        if let Some(ref mut blast) = self.blast_radius {
            blast.edges.sort_unstable();
            blast.must_touch_files.sort_unstable();
            blast.must_touch_files.dedup();
            blast.must_touch_symbols.sort_unstable();
            blast.must_touch_symbols.dedup();
            blast.test_hints.sort_unstable();
            blast.test_hints.dedup();
            blast.honesty_notes.sort_unstable();
            blast.honesty_notes.dedup();
        }
        self.centrality_risks.sort_unstable();
        self.logging_coverage_delta.sort_unstable();
        self.error_handling_delta.sort_unstable();
        self.telemetry_coverage_delta.sort_unstable();
        self.infrastructure_dirs.sort_unstable();
        self.env_var_deps.sort_unstable();
        self.env_var_deps.dedup();
        self.test_coverage.sort_unstable();
        if let Some(ref mut flows_report) = self.affected_flows {
            crate::impact::enrichment::affected_flows::sort_affected_flows(&mut flows_report.flows);
        }
        self.runtime_usage_delta.sort_unstable();
        self.signature_deltas.sort_unstable();
        self.hotspots.sort_unstable_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.path.cmp(&b.path))
        });
        self.verification_results.sort_unstable();
        self.relevant_decisions.sort_unstable_by(|a, b| {
            b.similarity
                .partial_cmp(&a.similarity)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.file_path.cmp(&b.file_path))
        });
        // Sort observability by severity descending
        self.observability.sort_unstable();
        // Sort affected_contracts by similarity descending, path ascending for ties
        self.affected_contracts.sort_unstable();
        self.data_flow_matches.sort_unstable();
        self.trace_config_drift.sort_unstable();
        self.trace_env_vars.sort_unstable();
        if let Some(ref mut sdk) = self.sdk_dependencies_delta {
            sdk.added.sort_unstable();
            sdk.removed.sort_unstable();
            sdk.modified.sort_unstable();
        }
        self.deploy_manifest_changes.sort_unstable();
        self.ci_predictions.sort_unstable_by(|a, b| {
            b.failure_probability
                .partial_cmp(&a.failure_probability)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.job_name.cmp(&b.job_name))
        });
        self.service_impact.sort_unstable();
        self.dead_code_findings.sort_unstable();
        self.actionable_lead.sort();
    }

    /// Escalate risk_level by one tier for observability/contract signals.
    /// High → Low→Medium or Medium→High; Elevated → Low→Medium only.
    pub fn escalate_risk(&mut self, elevation: crate::observability::signal::RiskElevation) {
        use crate::observability::signal::RiskElevation;
        match elevation {
            RiskElevation::High => {
                self.risk_level = match self.risk_level {
                    super::RiskLevel::Low => super::RiskLevel::Medium,
                    _ => super::RiskLevel::High,
                };
            }
            RiskElevation::Elevated => {
                if self.risk_level == super::RiskLevel::Low {
                    self.risk_level = super::RiskLevel::Medium;
                }
            }
            RiskElevation::None => {}
        }
    }

    /// Apply a modular risk impact to the packet.
    pub fn apply_risk_impact(&mut self, impact: super::RiskImpact, total_weight: &mut u32) {
        *total_weight += impact.weight;
        self.risk_reasons.extend(impact.reasons);
    }

    /// Finalize the risk level based on the accumulated weight.
    /// Reconciles overall risk so it does not exceed the highest individual item risk
    /// unless escalated due to change volume.
    pub fn finalize_risk_level(&mut self, total_weight: u32, has_prior_risk_signal: bool) {
        let rule_level = if total_weight > 50 {
            super::RiskLevel::High
        } else if total_weight > 20 {
            super::RiskLevel::Medium
        } else {
            super::RiskLevel::Low
        };

        if !has_prior_risk_signal || rule_level > self.risk_level {
            self.risk_level = rule_level;
        }

        // Reconcile: if risk is HIGH but there are very few changed files (≤3),
        // note the escalation so it does not appear to contradict the item-level view.
        if self.risk_level == super::RiskLevel::High && self.changes.len() <= 3 {
            let n = self.changes.len();
            let note = format!("(escalated due to {n} changed file(s))");
            if !self.risk_reasons.iter().any(|r| r.contains(&note)) {
                self.risk_reasons.push(note);
            }
        }

        // For clean tree or no changes, risk should be NONE-equivalent.
        if self.changes.is_empty() && self.risk_reasons.is_empty() {
            self.risk_level = super::RiskLevel::Low;
            self.risk_reasons.push("No changes detected".to_string());
        }

        if self.risk_reasons.is_empty() {
            self.risk_reasons
                .push("Minimal changes detected".to_string());
        }
    }

    /// Truncates the packet to fit within a target character limit.
    /// Priority:
    /// 1. Strip verification stdout/stderr
    /// 2. Strip symbol/import/runtime data for unchanged files (if any were included)
    /// 3. Strip temporal couplings
    /// 4. Strip hotspots
    ///
    /// The prompt text length (pretty JSON with escaped backticks, see
    /// [`prompt_packet_text`]) is the budget meter, in bytes. At most two
    /// full-packet serializes per call: one to decide work, one confirm if
    /// any strip ran. Per-phase early-exit uses a running length of
    /// field-level prompt bytes (value-only, nesting-depth aware), not extra
    /// full-packet serializes.
    pub fn truncate_for_context(&mut self, target_chars: usize) -> bool {
        let (mut running, measure_ok) = match prompt_packet_len(self) {
            Ok(n) if n <= target_chars => return false,
            Ok(n) => (n, true),
            Err(err) => {
                tracing::warn!(
                    error = %err,
                    "impact packet prompt serialize failed; treating as over-budget"
                );
                (usize::MAX, false)
            }
        };

        let mut stripped = false;
        let under = |running: usize| measure_ok && running <= target_chars;

        // Phase 1: Clear verification output
        for res in &mut self.verification_results {
            if res.stdout.is_empty() && res.stderr.is_empty() {
                continue;
            }
            stripped = true;
            let old_stdout = std::mem::take(&mut res.stdout);
            let old_stderr = std::mem::take(&mut res.stderr);
            let old_truncated = res.truncated;
            res.stdout = "[TRUNCATED]".to_string();
            res.stderr = "[TRUNCATED]".to_string();
            res.truncated = true;
            debit_running(&mut running, &old_stdout, &res.stdout, 3);
            debit_running(&mut running, &old_stderr, &res.stderr, 3);
            debit_running(&mut running, &old_truncated, &res.truncated, 3);
        }
        if stripped && under(running) {
            confirm_prompt_len(self, running);
            return true;
        }

        // Phase 2: Strip detailed analysis for non-staged files
        for change in &mut self.changes {
            if change.is_staged {
                continue;
            }
            if change.symbols.is_none()
                && change.imports.is_none()
                && change.runtime_usage.is_none()
            {
                continue;
            }
            stripped = true;
            let old_symbols = change.symbols.take();
            let old_imports = change.imports.take();
            let old_runtime = change.runtime_usage.take();
            debit_running(&mut running, &old_symbols, &change.symbols, 3);
            debit_running(&mut running, &old_imports, &change.imports, 3);
            debit_running(&mut running, &old_runtime, &change.runtime_usage, 3);
        }
        if stripped && under(running) {
            confirm_prompt_len(self, running);
            return true;
        }

        // Phase 3: Strip temporal and structural couplings
        let phase3_touched = !self.temporal_couplings.is_empty()
            || !self.temporal_couplings_audit.is_empty()
            || !self.discarded_tree_paths.is_empty()
            || !self.structural_couplings.is_empty()
            || self.blast_radius.is_some()
            || !self.centrality_risks.is_empty()
            || !self.logging_coverage_delta.is_empty()
            || !self.error_handling_delta.is_empty()
            || !self.telemetry_coverage_delta.is_empty()
            || !self.infrastructure_dirs.is_empty()
            || !self.env_var_deps.is_empty()
            || !self.test_coverage.is_empty()
            || self.test_gaps.is_some()
            || self.affected_flows.is_some()
            || !self.runtime_usage_delta.is_empty()
            || !self.signature_deltas.is_empty()
            || !self.relevant_decisions.is_empty()
            || !self.observability.is_empty()
            || !self.affected_contracts.is_empty()
            || !self.ai_insights.is_empty()
            || !self.data_flow_matches.is_empty()
            || !self.trace_config_drift.is_empty()
            || !self.trace_env_vars.is_empty()
            || self.sdk_dependencies_delta.is_some()
            || !self.deploy_manifest_changes.is_empty()
            || self.ci_config_change.is_some()
            || !self.ci_predictions.is_empty()
            || !self.service_impact.is_empty()
            || self.service_map_delta.is_some()
            || !self.dead_code_findings.is_empty();
        if phase3_touched {
            stripped = true;
            debit_clear_vec(&mut running, &mut self.temporal_couplings, 1);
            debit_clear_vec(&mut running, &mut self.temporal_couplings_audit, 1);
            debit_clear_vec(&mut running, &mut self.discarded_tree_paths, 1);
            debit_clear_vec(&mut running, &mut self.structural_couplings, 1);
            debit_take_blast_radius(&mut running, &mut self.blast_radius, 1);
            debit_clear_vec(&mut running, &mut self.centrality_risks, 1);
            debit_clear_vec(&mut running, &mut self.logging_coverage_delta, 1);
            debit_clear_vec(&mut running, &mut self.error_handling_delta, 1);
            debit_clear_vec(&mut running, &mut self.telemetry_coverage_delta, 1);
            debit_clear_vec(&mut running, &mut self.infrastructure_dirs, 1);
            debit_clear_vec(&mut running, &mut self.env_var_deps, 1);
            debit_clear_vec(&mut running, &mut self.test_coverage, 1);
            debit_take_opt(&mut running, &mut self.test_gaps, 1);
            debit_take_opt(&mut running, &mut self.affected_flows, 1);
            debit_clear_vec(&mut running, &mut self.runtime_usage_delta, 1);
            debit_clear_vec(&mut running, &mut self.signature_deltas, 1);
            debit_clear_vec(&mut running, &mut self.relevant_decisions, 1);
            debit_clear_vec(&mut running, &mut self.observability, 1);
            debit_clear_vec(&mut running, &mut self.affected_contracts, 1);
            debit_clear_ai_insights(&mut running, &mut self.ai_insights, 1);
            debit_clear_vec(&mut running, &mut self.data_flow_matches, 1);
            debit_clear_vec(&mut running, &mut self.trace_config_drift, 1);
            debit_clear_vec(&mut running, &mut self.trace_env_vars, 1);
            debit_take_opt(&mut running, &mut self.sdk_dependencies_delta, 1);
            debit_clear_vec(&mut running, &mut self.deploy_manifest_changes, 1);
            debit_take_opt(&mut running, &mut self.ci_config_change, 1);
            debit_clear_vec(&mut running, &mut self.ci_predictions, 1);
            debit_clear_vec(&mut running, &mut self.service_impact, 1);
            debit_take_opt(&mut running, &mut self.service_map_delta, 1);
            debit_clear_vec(&mut running, &mut self.dead_code_findings, 1);
        }
        if stripped && under(running) {
            confirm_prompt_len(self, running);
            return true;
        }

        // Phase 4: Strip hotspots and the window label that describes them.
        if !self.hotspots.is_empty() {
            stripped = true;
            debit_clear_vec(&mut running, &mut self.hotspots, 1);
        }
        if self.hotspot_provenance.is_some() {
            stripped = true;
            debit_take_opt(&mut running, &mut self.hotspot_provenance, 1);
        }
        if stripped && under(running) {
            confirm_prompt_len(self, running);
            return true;
        }

        // Phase 5: Last resort - keep only file paths in changes
        for change in &mut self.changes {
            if change.symbols.is_none()
                && change.imports.is_none()
                && change.runtime_usage.is_none()
            {
                continue;
            }
            stripped = true;
            let old_symbols = change.symbols.take();
            let old_imports = change.imports.take();
            let old_runtime = change.runtime_usage.take();
            debit_running(&mut running, &old_symbols, &change.symbols, 3);
            debit_running(&mut running, &old_imports, &change.imports, 3);
            debit_running(&mut running, &old_runtime, &change.runtime_usage, 3);
        }

        if stripped {
            confirm_prompt_len(self, running);
        }
        true
    }

    /// Reset the thread-local full-packet prompt-serialize count, run
    /// [`Self::truncate_for_context`], and return `(truncated, serialize_count)`.
    #[cfg(test)]
    pub(crate) fn truncate_for_context_metered(&mut self, target_chars: usize) -> (bool, usize) {
        reset_truncate_serialize_count();
        let truncated = self.truncate_for_context(target_chars);
        (truncated, take_truncate_serialize_count())
    }

    /// Next full-packet prompt serialize returns `Err` (serde_json maps
    /// packet NaN/Inf floats to `null`, so tests inject the failure).
    #[cfg(test)]
    pub(crate) fn force_next_truncate_serialize_error() {
        FORCE_PROMPT_SERIALIZE_ERR.with(|c| c.set(true));
    }
}

#[cfg(test)]
mod prompt_meter_tests;
