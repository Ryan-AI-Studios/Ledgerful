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
    #[serde(default)]
    pub runtime_usage_delta: Vec<super::RuntimeUsageDelta>,
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
}

impl ImpactPacket {
    pub fn is_empty(&self) -> bool {
        self.changes.is_empty()
            && self.temporal_couplings.is_empty()
            && self.structural_couplings.is_empty()
            && self.centrality_risks.is_empty()
            && self.logging_coverage_delta.is_empty()
            && self.error_handling_delta.is_empty()
            && self.telemetry_coverage_delta.is_empty()
            && self.infrastructure_dirs.is_empty()
            && self.env_var_deps.is_empty()
            && self.test_coverage.is_empty()
            && self.runtime_usage_delta.is_empty()
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
            centrality_risks: Vec::new(),
            logging_coverage_delta: Vec::new(),
            error_handling_delta: Vec::new(),
            telemetry_coverage_delta: Vec::new(),
            infrastructure_dirs: Vec::new(),
            env_var_deps: Vec::new(),
            test_coverage: Vec::new(),
            runtime_usage_delta: Vec::new(),
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

    /// Finalizes the packet by sorting all internal collections deterministically.
    pub fn finalize(&mut self) {
        self.risk_reasons.sort_unstable();

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
        self.centrality_risks.sort_unstable();
        self.logging_coverage_delta.sort_unstable();
        self.error_handling_delta.sort_unstable();
        self.telemetry_coverage_delta.sort_unstable();
        self.infrastructure_dirs.sort_unstable();
        self.env_var_deps.sort_unstable();
        self.env_var_deps.dedup();
        self.test_coverage.sort_unstable();
        self.runtime_usage_delta.sort_unstable();
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
        self.centrality_risks.clear();
        self.logging_coverage_delta.clear();
        self.error_handling_delta.clear();
        self.telemetry_coverage_delta.clear();
        self.infrastructure_dirs.clear();
        self.env_var_deps.clear();
        self.test_coverage.clear();
        self.runtime_usage_delta.clear();
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
