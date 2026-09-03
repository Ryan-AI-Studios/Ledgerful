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
}

fn default_path_mode() -> String {
    "code".to_string()
}

fn default_analysis_mode() -> String {
    "working_tree".to_string()
}

impl ImpactPacket {
    pub fn is_empty(&self) -> bool {
        self.changes.is_empty()
            && self.temporal_couplings.is_empty()
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
            dead_code_findings: Vec::new(),
            path_mode: default_path_mode(),
            demoted_temporal_count: 0,
            analysis_mode: default_analysis_mode(),
            prospective_paths: Vec::new(),
            actionable_lead: Vec::new(),
            glossary: None,
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
    pub fn truncate_for_context(&mut self, target_chars: usize) -> bool {
        let current_json = serde_json::to_string(self).unwrap_or_default();
        if current_json.len() <= target_chars {
            return false;
        }

        // Phase 1: Clear verification output
        for res in &mut self.verification_results {
            if !res.stdout.is_empty() || !res.stderr.is_empty() {
                res.stdout = "[TRUNCATED]".to_string();
                res.stderr = "[TRUNCATED]".to_string();
                res.truncated = true;
            }
        }

        let current_json = serde_json::to_string(self).unwrap_or_default();
        if current_json.len() <= target_chars {
            return true;
        }

        // Phase 2: Strip detailed analysis for non-staged files
        for change in &mut self.changes {
            if !change.is_staged {
                change.symbols = None;
                change.imports = None;
                change.runtime_usage = None;
            }
        }

        let current_json = serde_json::to_string(self).unwrap_or_default();
        if current_json.len() <= target_chars {
            return true;
        }

        // Phase 3: Strip temporal and structural couplings
        self.temporal_couplings.clear();
        self.structural_couplings.clear();
        self.blast_radius = None;
        self.centrality_risks.clear();
        self.logging_coverage_delta.clear();
        self.error_handling_delta.clear();
        self.telemetry_coverage_delta.clear();
        self.infrastructure_dirs.clear();
        self.env_var_deps.clear();
        self.test_coverage.clear();
        self.test_gaps = None;
        self.affected_flows = None;
        self.runtime_usage_delta.clear();
        self.signature_deltas.clear();
        self.relevant_decisions.clear();
        // CRITICAL: Clear observability signals which can contain unbounded log excerpts
        self.observability.clear();
        self.affected_contracts.clear();
        self.ai_insights.clear();
        self.data_flow_matches.clear();
        self.trace_config_drift.clear();
        self.trace_env_vars.clear();
        self.sdk_dependencies_delta = None;
        self.deploy_manifest_changes.clear();
        self.ci_config_change = None;
        self.ci_predictions.clear();
        self.service_impact.clear();
        self.service_map_delta = None;
        self.dead_code_findings.clear();

        let current_json = serde_json::to_string(self).unwrap_or_default();
        if current_json.len() <= target_chars {
            return true;
        }

        // Phase 4: Strip hotspots
        self.hotspots.clear();

        let current_json = serde_json::to_string(self).unwrap_or_default();
        if current_json.len() <= target_chars {
            return true;
        }

        // Phase 5: Last resort - keep only file paths in changes
        for change in &mut self.changes {
            change.symbols = None;
            change.imports = None;
            change.runtime_usage = None;
        }

        true
    }
}
