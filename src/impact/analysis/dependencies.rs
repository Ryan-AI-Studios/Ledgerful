use crate::config::model::Config;
use crate::impact::analysis::ImpactProvider;
use crate::impact::packet::{ImpactPacket, RiskImpact};
use crate::policy::rules::Rules;
use miette::Result;

/// Provider that analyzes dependency-related risk: coupling, data contracts, and APIs.
pub struct DependencyImpactProvider;

impl ImpactProvider for DependencyImpactProvider {
    fn name(&self) -> &'static str {
        "Dependency Impact Provider"
    }

    fn analyze(
        &self,
        packet: &ImpactPacket,
        _rules: &Rules,
        config: &Config,
    ) -> Result<RiskImpact> {
        let mut total_weight = 0;
        let mut reasons = Vec::new();

        // 1. Structural Coupling Risk
        let weight_per_caller = 15;
        let mut structural_weight = 0;
        for (i, coupling) in packet.structural_couplings.iter().enumerate() {
            if i >= 2 {
                break;
            } // Cap at 2 to avoid weight explosion
            structural_weight += weight_per_caller;
            reasons.push(format!(
                "Structurally coupled: {} calls {}",
                coupling.caller_symbol_name, coupling.callee_symbol_name
            ));
        }
        total_weight += structural_weight;

        // 2. Data Contract Risk
        // 2. Data Contract Risk
        for change in &packet.changes {
            let weight_mult = config.impact.get_path_weight(&change.path);
            for model in &change.data_models {
                let weight = if model.model_kind == "GENERATED" {
                    20
                } else {
                    35
                };
                total_weight += (weight as f64 * weight_mult) as u32;
                reasons.push(format!(
                    "Data model changed: {} ({})",
                    model.model_name, model.model_kind
                ));
            }
        }

        // 3. API Surface Risk
        for change in &packet.changes {
            let weight_mult = config.impact.get_path_weight(&change.path);
            for route in &change.api_routes {
                reasons.push(format!(
                    "Public API route change: {} {}",
                    route.method, route.path_pattern
                ));
                total_weight += (30.0 * weight_mult) as u32;
            }
        }

        // 4. SDK Dependencies
        if let Some(ref delta) = packet.sdk_dependencies_delta {
            for sdk in &delta.added {
                reasons.push(format!("New SDK dependency: {}", sdk.sdk_name));
                total_weight += 5;
            }
            for sdk in &delta.modified {
                reasons.push(format!("Modified SDK dependency: {}", sdk.sdk_name));
                total_weight += 3;
            }
        }

        // 5. Cross-Service Impact
        if let Some(ref delta) = packet.service_map_delta {
            let count = delta.affected_services.len();
            if count >= config.coverage.services.cross_service_elevation_threshold as usize {
                reasons.push(format!("Cross-service change affecting {} services", count));
                total_weight += 10;
            }
        }

        Ok(RiskImpact {
            weight: total_weight,
            reasons,
        })
    }
}
