use crate::config::model::Config;
use crate::impact::analysis::ImpactProvider;
use crate::impact::packet::{ImpactPacket, RiskImpact};
use crate::policy::rules::Rules;
use miette::Result;

/// Provider that analyzes semantic risk: centrality and reachability.
pub struct SemanticImpactProvider;

impl ImpactProvider for SemanticImpactProvider {
    fn name(&self) -> &'static str {
        "Semantic Impact Provider"
    }

    fn analyze(
        &self,
        packet: &ImpactPacket,
        _rules: &Rules,
        _config: &Config,
    ) -> Result<RiskImpact> {
        let mut total_weight = 0;
        let mut reasons = Vec::new();

        // 1. Centrality Risk
        for risk in &packet.centrality_risks {
            if risk.entrypoints_reachable >= 5 {
                reasons.push(format!(
                    "High centrality: changed symbol '{}' can reach {} entry points",
                    risk.symbol_name, risk.entrypoints_reachable
                ));
                total_weight += 15;
            }
        }

        // 2. Entrypoint and Public Symbol Changes
        for change in &packet.changes {
            if let Some(ref symbols) = change.symbols {
                let weight_mult = _config.impact.get_path_weight(&change.path);
                for sym in symbols {
                    if sym.is_public {
                        reasons.push(format!("Public symbol modified: {}", sym.name));
                        total_weight += (30.0 * weight_mult) as u32;
                    }
                    if let Some(ref kind) = sym.entrypoint_kind {
                        if kind == "ENTRYPOINT" {
                            reasons.push(format!("Entry point changed: {}", sym.name));
                            total_weight += (20.0 * weight_mult) as u32;
                        } else if kind == "HANDLER" {
                            reasons.push(format!("Handler changed: {}", sym.name));
                            total_weight += (15.0 * weight_mult) as u32;
                        }
                    }
                }
            }
        }

        Ok(RiskImpact {
            weight: total_weight,
            reasons,
        })
    }
}
