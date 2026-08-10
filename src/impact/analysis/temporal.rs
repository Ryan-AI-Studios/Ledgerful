use crate::config::model::Config;
use crate::impact::analysis::ImpactProvider;
use crate::impact::packet::{ImpactPacket, RiskImpact};
use crate::impact::path_class::should_demote_pair;
use crate::policy::rules::Rules;
use miette::Result;

/// Engine-internal temporal risk threshold (not a published CodeScene metric).
pub const TEMPORAL_RISK_THRESHOLD: f32 = 0.7;

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
        let path_mode = packet.path_mode.as_str();

        // 1. Temporal Coupling Risk (code-default demotion: 0173)
        for coupling in &packet.temporal_couplings {
            if coupling.score < TEMPORAL_RISK_THRESHOLD {
                continue;
            }
            let a = coupling.file_a.to_string_lossy();
            let b = coupling.file_b.to_string_lossy();
            if should_demote_pair(&a, &b, path_mode) {
                // Keep full temporal_couplings on packet for audit; skip risk weight/reasons.
                continue;
            }
            reasons.push(format!(
                "High temporal coupling: {} and {} often change together ({:.0}%)",
                coupling.file_a.display(),
                coupling.file_b.display(),
                coupling.score * 100.0
            ));
            total_weight += 10;
        }

        Ok(RiskImpact {
            weight: total_weight,
            reasons,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::impact::packet::{RiskLevel, TemporalCoupling};
    use std::path::PathBuf;

    #[test]
    fn temporal_demotion_pure_governance_zero_weight_under_code_mode() {
        let mut packet = ImpactPacket {
            path_mode: "code".to_string(),
            temporal_couplings: vec![TemporalCoupling {
                file_a: PathBuf::from("conductor/0173/spec.md"),
                file_b: PathBuf::from("deferred.md"),
                score: 0.95,
            }],
            risk_level: RiskLevel::Low,
            ..ImpactPacket::default()
        };

        let impact = TemporalImpactProvider
            .analyze(&packet, &Rules::default(), &Config::default())
            .expect("analyze");
        assert_eq!(impact.weight, 0);
        assert!(impact.reasons.is_empty());
        // Full list retained on packet
        assert_eq!(packet.temporal_couplings.len(), 1);

        // Honesty count helper matches demotion
        let demoted = crate::impact::path_class::count_demoted_temporal(
            &packet.temporal_couplings,
            &packet.path_mode,
            TEMPORAL_RISK_THRESHOLD,
        );
        assert_eq!(demoted, 1);

        // include-governance / pathMode=all restores weight
        packet.path_mode = "all".to_string();
        let impact_all = TemporalImpactProvider
            .analyze(&packet, &Rules::default(), &Config::default())
            .expect("analyze all");
        assert_eq!(impact_all.weight, 10);
        assert_eq!(impact_all.reasons.len(), 1);
        assert_eq!(
            crate::impact::path_class::count_demoted_temporal(
                &packet.temporal_couplings,
                "all",
                TEMPORAL_RISK_THRESHOLD,
            ),
            0
        );
    }

    #[test]
    fn temporal_keeps_code_code_and_contract_pairs() {
        let packet = ImpactPacket {
            path_mode: "code".to_string(),
            temporal_couplings: vec![
                TemporalCoupling {
                    file_a: PathBuf::from("src/a.rs"),
                    file_b: PathBuf::from("src/b.rs"),
                    score: 0.9,
                },
                TemporalCoupling {
                    file_a: PathBuf::from("src/a.rs"),
                    file_b: PathBuf::from("docs/agent-output-contract.md"),
                    score: 0.9,
                },
            ],
            ..ImpactPacket::default()
        };
        let impact = TemporalImpactProvider
            .analyze(&packet, &Rules::default(), &Config::default())
            .expect("analyze");
        assert_eq!(impact.weight, 20);
        assert_eq!(impact.reasons.len(), 2);
    }
}
