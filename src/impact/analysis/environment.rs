use crate::config::model::Config;
use crate::impact::analysis::ImpactProvider;
use crate::impact::packet::{ImpactPacket, RiskImpact};
use crate::policy::rules::Rules;
use miette::Result;

/// Provider that analyzes environment-related risk: Infra, Env Vars, ADRs, and Advisories.
pub struct EnvironmentImpactProvider;

impl ImpactProvider for EnvironmentImpactProvider {
    fn name(&self) -> &'static str {
        "Environment Impact Provider"
    }

    fn analyze(
        &self,
        packet: &ImpactPacket,
        _rules: &Rules,
        config: &Config,
    ) -> Result<RiskImpact> {
        let mut total_weight = 0;
        let mut reasons = Vec::new();

        // 1. Env Var Risk
        for dep in &packet.env_var_deps {
            if !dep.declared {
                reasons.push(format!(
                    "New environment variable dependency: {}",
                    dep.var_name
                ));
                total_weight += 15;
            }
        }

        // 2. Runtime Usage Delta Risk
        for delta in &packet.runtime_usage_delta {
            if delta.env_vars_current_count > delta.env_vars_previous_count {
                reasons.push(format!(
                    "New environment variable references in {} ({} -> {})",
                    delta.file_path, delta.env_vars_previous_count, delta.env_vars_current_count
                ));
                total_weight += 20;
            }
            if delta.env_vars_current_count == delta.env_vars_previous_count
                && delta.env_vars_current_count > 0
                && !delta.env_vars_previous.is_empty()
                && delta.env_vars_previous != delta.env_vars_current
            {
                let added: Vec<&str> = delta
                    .env_vars_current
                    .iter()
                    .filter(|v| !delta.env_vars_previous.contains(v))
                    .map(|s| s.as_str())
                    .collect();
                let removed: Vec<&str> = delta
                    .env_vars_previous
                    .iter()
                    .filter(|v| !delta.env_vars_current.contains(v))
                    .map(|s| s.as_str())
                    .collect();
                reasons.push(format!(
                    "Environment variable identities changed in {} (added: {:?}, removed: {:?})",
                    delta.file_path, added, removed
                ));
                total_weight += 20;
            }
            if delta.config_keys_current_count > delta.config_keys_previous_count {
                reasons.push(format!(
                    "Configuration key references changed in {}",
                    delta.file_path
                ));
                total_weight += 20;
            }
        }

        // 3. Infrastructure Risk (Manifests)
        for manifest in &packet.deploy_manifest_changes {
            reasons.push(format!(
                "Deployment manifest change: {}",
                manifest.file.display()
            ));
            total_weight += 3;
        }

        // 4. Observability Risk
        for drift in &packet.trace_config_drift {
            reasons.push(format!(
                "Observability config drift: {}",
                drift.file.display()
            ));
            total_weight += drift.risk_weight as u32;
        }

        // 5. ADR Staleness (Advisory)
        if config.coverage.adr_staleness.enabled {
            for decision in &packet.relevant_decisions {
                if let Some(days) = decision.staleness_days
                    && days > config.coverage.adr_staleness.threshold_days
                {
                    reasons.push(format!(
                        "Stale architectural context: {} ({} days old)",
                        decision.file_path.display(),
                        days
                    ));
                }
            }
        }

        Ok(RiskImpact {
            weight: total_weight,
            reasons,
        })
    }
}
