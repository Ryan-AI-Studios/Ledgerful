use crate::config::model::Config;
use crate::impact::analysis::ImpactProvider;
use crate::impact::packet::{ImpactPacket, RiskImpact};
use crate::policy::rules::Rules;
use miette::Result;

/// Provider that analyzes temporal risk: historical file coupling.
pub struct TemporalImpactProvider;

impl ImpactProvider for TemporalImpactProvider {
    fn name(&self) -> &'static str {
        "Temporal Impact Provider"
    }

    fn analyze(
        &self,
        packet: &ImpactPacket,
        _rules: &Rules,
        _config: &Config,
    ) -> Result<RiskImpact> {
        let mut total_weight = 0;
        let mut reasons = Vec::new();

        // 1. Temporal Coupling Risk
        for coupling in &packet.temporal_couplings {
            if coupling.score >= 0.7 {
                reasons.push(format!(
                    "High temporal coupling: {} and {} often change together ({:.0}%)",
                    coupling.file_a.display(),
                    coupling.file_b.display(),
                    coupling.score * 100.0
                ));
                total_weight += 10;
            }
        }

        Ok(RiskImpact {
            weight: total_weight,
            reasons,
        })
    }
}
