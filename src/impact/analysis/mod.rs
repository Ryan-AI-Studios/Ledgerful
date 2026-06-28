use crate::config::model::Config;
use crate::impact::packet::{ImpactPacket, RiskImpact};
use crate::policy::rules::Rules;
use miette::Result;

pub mod dead_code;
pub mod dependencies;
pub mod environment;
pub mod git;
pub mod semantic;
pub mod temporal;

/// Trait for modular impact analysis and risk scoring components.
pub trait ImpactProvider: Send + Sync {
    /// The unique name of this provider (for debugging and tracing).
    fn name(&self) -> &'static str;

    /// Analyzes the impact packet and returns the calculated risk impact.
    fn analyze(&self, packet: &ImpactPacket, rules: &Rules, config: &Config) -> Result<RiskImpact>;
}

/// Registry that orchestrates multiple impact analysis providers.
pub struct AnalysisRegistry {
    pub providers: Vec<Box<dyn ImpactProvider>>,
}

impl Default for AnalysisRegistry {
    fn default() -> Self {
        let mut registry = Self::new();
        registry.register(Box::new(git::GitImpactProvider));
        registry.register(Box::new(dependencies::DependencyImpactProvider));
        registry.register(Box::new(semantic::SemanticImpactProvider));
        registry.register(Box::new(temporal::TemporalImpactProvider));
        registry.register(Box::new(environment::EnvironmentImpactProvider));
        registry.register(Box::new(dead_code::DeadCodeImpactProvider));
        registry
    }
}

impl AnalysisRegistry {
    pub fn new() -> Self {
        Self {
            providers: Vec::new(),
        }
    }

    pub fn register(&mut self, provider: Box<dyn ImpactProvider>) {
        self.providers.push(provider);
    }

    pub fn run(&self, packet: &mut ImpactPacket, rules: &Rules, config: &Config) -> Result<()> {
        let mut total_weight = 0;

        let has_prior_risk_signal = packet.risk_level == crate::impact::packet::RiskLevel::High
            || !packet.risk_reasons.is_empty();

        for provider in &self.providers {
            let impact = provider.analyze(packet, rules, config)?;
            packet.apply_risk_impact(impact, &mut total_weight);
        }

        packet.finalize_risk_level(total_weight, has_prior_risk_signal);

        Ok(())
    }
}

pub fn analyze_risk(packet: &mut ImpactPacket, rules: &Rules, config: &Config) -> Result<()> {
    let registry = AnalysisRegistry::default();
    registry.run(packet, rules, config)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::impact::packet::{ChangedFile, FileAnalysisStatus, RiskLevel};
    use std::path::PathBuf;

    #[test]
    fn test_analyze_risk_low() {
        let mut packet = ImpactPacket::default();
        packet.changes.push(ChangedFile {
            path: PathBuf::from("README.md"),
            status: "Modified".to_string(),
            analysis_status: FileAnalysisStatus::default(),
            ..ChangedFile::default()
        });

        let rules = Rules::default();
        analyze_risk(&mut packet, &rules, &Config::default()).unwrap();

        assert_eq!(packet.risk_level, RiskLevel::Low);
    }

    #[test]
    fn test_analyze_risk_protected_path() {
        let mut packet = ImpactPacket::default();
        packet.changes.push(ChangedFile {
            path: PathBuf::from("Cargo.toml"),
            status: "Modified".to_string(),
            ..ChangedFile::default()
        });

        let rules = Rules {
            protected_paths: vec!["Cargo.toml".to_string()],
            ..Rules::default()
        };

        analyze_risk(&mut packet, &rules, &Config::default()).unwrap();

        assert_eq!(packet.risk_level, RiskLevel::High);
        assert!(
            packet
                .risk_reasons
                .iter()
                .any(|r| r.contains("Protected path hit"))
        );
    }

    #[test]
    fn test_registry_aggregates_weights() {
        let mut packet = ImpactPacket::default();
        // 10 changes -> 20 weight (Git)
        for i in 0..10 {
            packet.changes.push(ChangedFile {
                path: PathBuf::from(format!("src/file_{}.rs", i)),
                status: "Added".to_string(),
                ..ChangedFile::default()
            });
        }

        // 1 structural coupling -> 15 weight (Dependency)
        packet
            .structural_couplings
            .push(crate::impact::packet::StructuralCoupling {
                caller_symbol_name: "A".to_string(),
                callee_symbol_name: "B".to_string(),
                caller_file_path: PathBuf::from("src/a.rs"),
            });

        let rules = Rules::default();
        analyze_risk(&mut packet, &rules, &Config::default()).unwrap();

        // Total weight = 20 + 15 = 35 (> 20 -> Medium)
        assert_eq!(packet.risk_level, RiskLevel::Medium);
        assert!(
            packet
                .risk_reasons
                .iter()
                .any(|r| r.contains("High volume"))
        );
        assert!(
            packet
                .risk_reasons
                .iter()
                .any(|r| r.contains("Structurally coupled"))
        );
    }
}
