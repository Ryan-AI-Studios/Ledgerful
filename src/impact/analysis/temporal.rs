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

    fn coupling(a: &str, b: &str, score: f32) -> TemporalCoupling {
        TemporalCoupling {
            file_a: PathBuf::from(a),
            file_b: PathBuf::from(b),
            score,
        }
    }

    #[test]
    fn src_src_pair_still_scores() {
        // T5: src↔src 0.9 still weight 10 under pathMode=code.
        let packet = ImpactPacket {
            path_mode: "code".to_string(),
            temporal_couplings: vec![coupling("src/a.rs", "src/b.rs", 0.9)],
            ..ImpactPacket::default()
        };
        let impact = TemporalImpactProvider
            .analyze(&packet, &Rules::default(), &Config::default())
            .expect("analyze");
        assert_eq!(impact.weight, 10);
        assert_eq!(impact.reasons.len(), 1);
        assert_eq!(packet.temporal_couplings.len(), 1);
    }

    #[test]
    fn include_governance_restores_changelog_weight() {
        // T14: CHANGELOG ↔ docs/Call-Resolution.md score 0.9.
        let mut packet = ImpactPacket {
            path_mode: "code".to_string(),
            temporal_couplings: vec![coupling("CHANGELOG.md", "docs/Call-Resolution.md", 0.9)],
            ..ImpactPacket::default()
        };
        let impact = TemporalImpactProvider
            .analyze(&packet, &Rules::default(), &Config::default())
            .expect("analyze");
        assert_eq!(impact.weight, 0);
        assert_eq!(
            crate::impact::path_class::count_demoted_temporal(
                &packet.temporal_couplings,
                "code",
                TEMPORAL_RISK_THRESHOLD,
            ),
            1
        );

        packet.path_mode = "all".to_string();
        let impact_all = TemporalImpactProvider
            .analyze(&packet, &Rules::default(), &Config::default())
            .expect("analyze all");
        assert_eq!(impact_all.weight, 10);
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
    fn f1_only_five_packaging_pairs_are_not_dod1() {
        // T15: these five leftover KEEP-after-F1 packaging pairs have no
        // CHANGELOG, so F1 does not touch them. F1-only would still score
        // them (w=50 Medium, exactly the High boundary) — NOT DoD-1.
        // After F1+F2+F3 they all demote: weight 0, count_demoted_temporal == 5.
        // Packet retains all 5 couplings (F4).
        let leftover = vec![
            coupling("packaging/homebrew/ledgerful.rb", "packaging", 0.9),
            coupling("packaging/homebrew/ledgerful.rb", "packaging/homebrew", 0.9),
            coupling("packaging/scoop/ledgerful.json", "packaging", 0.9),
            coupling("packaging/scoop/ledgerful.json", "packaging/scoop", 0.9),
            coupling("packaging/homebrew/ledgerful.rb", "docs", 0.9),
        ];
        let packet = ImpactPacket {
            path_mode: "code".to_string(),
            temporal_couplings: leftover,
            ..ImpactPacket::default()
        };
        let impact = TemporalImpactProvider
            .analyze(&packet, &Rules::default(), &Config::default())
            .expect("analyze leftover");
        assert_eq!(impact.weight, 0);
        assert_eq!(
            crate::impact::path_class::count_demoted_temporal(
                &packet.temporal_couplings,
                "code",
                TEMPORAL_RISK_THRESHOLD,
            ),
            5
        );
        assert_eq!(packet.temporal_couplings.len(), 5);

        // T2-style: CHANGELOG pairs + those 5 → all demote, list kept.
        let t2 = vec![
            coupling("CHANGELOG.md", "docs/Call-Resolution.md", 0.9),
            coupling("CHANGELOG.md", "docs/Ledgerful/skill.md", 0.9),
            coupling("CHANGELOG.md", "src/commands/doctor.rs", 0.9),
            coupling("packaging/homebrew/ledgerful.rb", "packaging", 0.9),
            coupling("packaging/homebrew/ledgerful.rb", "packaging/homebrew", 0.9),
            coupling("packaging/scoop/ledgerful.json", "packaging", 0.9),
            coupling("packaging/scoop/ledgerful.json", "packaging/scoop", 0.9),
            coupling("packaging/homebrew/ledgerful.rb", "docs", 0.9),
        ];
        let t2_len = t2.len();
        let packet_t2 = ImpactPacket {
            path_mode: "code".to_string(),
            temporal_couplings: t2,
            ..ImpactPacket::default()
        };
        let impact_t2 = TemporalImpactProvider
            .analyze(&packet_t2, &Rules::default(), &Config::default())
            .expect("analyze t2");
        assert_eq!(impact_t2.weight, 0);
        assert_eq!(
            crate::impact::path_class::count_demoted_temporal(
                &packet_t2.temporal_couplings,
                "code",
                TEMPORAL_RISK_THRESHOLD,
            ),
            t2_len as u32
        );
        assert_eq!(packet_t2.temporal_couplings.len(), t2_len);
    }
}
