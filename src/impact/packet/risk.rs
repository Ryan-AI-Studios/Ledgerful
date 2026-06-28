use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "camelCase")]
pub enum RiskLevel {
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TemporalCoupling {
    pub file_a: PathBuf,
    pub file_b: PathBuf,
    pub score: f32,
}

impl Eq for TemporalCoupling {}

impl PartialOrd for TemporalCoupling {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for TemporalCoupling {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.file_a
            .cmp(&other.file_a)
            .then_with(|| self.file_b.cmp(&other.file_b))
            .then_with(|| {
                self.score
                    .partial_cmp(&other.score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct StructuralCoupling {
    pub caller_symbol_name: String,
    pub callee_symbol_name: String,
    pub caller_file_path: PathBuf,
}

impl Eq for StructuralCoupling {}

impl PartialOrd for StructuralCoupling {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for StructuralCoupling {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.caller_symbol_name
            .cmp(&other.caller_symbol_name)
            .then_with(|| self.callee_symbol_name.cmp(&other.callee_symbol_name))
            .then_with(|| self.caller_file_path.cmp(&other.caller_file_path))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CentralityRisk {
    pub symbol_name: String,
    pub entrypoints_reachable: usize,
}

impl Eq for CentralityRisk {}

impl PartialOrd for CentralityRisk {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for CentralityRisk {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.symbol_name
            .cmp(&other.symbol_name)
            .then_with(|| self.entrypoints_reachable.cmp(&other.entrypoints_reachable))
    }
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RiskImpact {
    pub weight: u32,
    pub reasons: Vec<String>,
}
