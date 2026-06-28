use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "camelCase")]
pub enum StalenessTier {
    Warning,
    Critical,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RelevantDecision {
    pub file_path: PathBuf,
    pub heading: Option<String>,
    pub excerpt: String,
    pub similarity: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rerank_score: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub staleness_days: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub staleness_tier: Option<StalenessTier>,
}

impl PartialEq for RelevantDecision {
    fn eq(&self, other: &Self) -> bool {
        self.similarity == other.similarity && self.file_path == other.file_path
    }
}

impl Eq for RelevantDecision {}

impl PartialOrd for RelevantDecision {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for RelevantDecision {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        other
            .similarity
            .partial_cmp(&self.similarity)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| self.file_path.cmp(&other.file_path))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Hotspot {
    pub path: PathBuf,
    #[serde(deserialize_with = "super::serialization::deserialize_score")]
    pub score: f32,
    #[serde(default)]
    pub display_score: f32,
    pub complexity: i32,
    pub frequency: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub centrality: Option<usize>,
}

impl Eq for Hotspot {}

impl PartialOrd for Hotspot {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Hotspot {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.path.cmp(&other.path).then_with(|| {
            self.score
                .partial_cmp(&other.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AiInsight {
    pub memory_id: String,
    pub relevance: f64,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "camelCase")]
pub struct KGImpact {
    pub source_node: String,
    pub source_category: String,
    pub impacted_node: String,
    pub impacted_category: String,
    pub relation: String,
    pub path_length: usize,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "camelCase")]
pub enum ConfidenceFactor {
    UnreachableFromEntrypoints,
    GitInactive { days_since_last_commit: u32 },
    NoTestCoverage,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DeadCodeFinding {
    pub symbol_name: String,
    pub file_path: PathBuf,
    pub confidence: f64,
    pub factors: Vec<ConfidenceFactor>,
    pub recommendation: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_start: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_end: Option<usize>,
}

impl DeadCodeFinding {
    /// 1-based inclusive start line, when available in the finding.
    pub fn line_start(&self) -> Option<usize> {
        self.line_start
    }

    /// 1-based inclusive end line, when available in the finding.
    pub fn line_end(&self) -> Option<usize> {
        self.line_end
    }
}

impl Eq for DeadCodeFinding {}

impl PartialOrd for DeadCodeFinding {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for DeadCodeFinding {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        other
            .confidence
            .partial_cmp(&self.confidence)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| self.file_path.cmp(&other.file_path))
            .then_with(|| self.symbol_name.cmp(&other.symbol_name))
    }
}
