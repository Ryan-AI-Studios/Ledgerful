use crate::impact::enrichment::edge_confidence::{ConfidenceClass, EdgeConfidenceSummary};
use serde::{Deserialize, Serialize};

/// One evidence-tagged call-graph edge in the structural blast radius.
///
/// Direction `caller` means `from_*` calls `to_*` (reverse callers of seeds at hop 1).
/// Direction `callee` means `from_*` calls into a seed's callee (optional forward).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct BlastEdge {
    pub hop: u32,
    pub direction: String,
    pub from_symbol: String,
    pub from_file: String,
    pub to_symbol: String,
    pub to_file: String,
    pub resolution_status: String,
    pub evidence: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f64>,
    /// Whether this edge's far node may seed hop N+1 (high-confidence discovery).
    pub expandable: bool,
    /// Product confidence class (pure function of `resolution_status` + `evidence`).
    /// Always present when edge is serialized. JSON key: `confidenceClass`.
    pub confidence_class: ConfidenceClass,
}

impl Eq for BlastEdge {}

impl PartialOrd for BlastEdge {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for BlastEdge {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // confidence_class is intentionally omitted: it is a pure function of
        // resolution_status + evidence, which are already ordered below.
        self.hop
            .cmp(&other.hop)
            .then_with(|| self.direction.cmp(&other.direction))
            .then_with(|| self.from_file.cmp(&other.from_file))
            .then_with(|| self.from_symbol.cmp(&other.from_symbol))
            .then_with(|| self.to_file.cmp(&other.to_file))
            .then_with(|| self.to_symbol.cmp(&other.to_symbol))
            .then_with(|| self.resolution_status.cmp(&other.resolution_status))
            .then_with(|| self.evidence.cmp(&other.evidence))
    }
}

/// Bounded structural blast radius (call-graph punchlist).
///
/// JSON key: `blastRadius`. This is **not** deploy `highBlastResources`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct BlastRadius {
    pub depth_requested: u32,
    pub depth_applied: u32,
    #[serde(default)]
    pub edges: Vec<BlastEdge>,
    #[serde(default)]
    pub must_touch_files: Vec<String>,
    #[serde(default)]
    pub must_touch_symbols: Vec<String>,
    #[serde(default)]
    pub test_hints: Vec<String>,
    #[serde(default)]
    pub honesty_notes: Vec<String>,
    /// Aggregate edge confidence class counts. Always present when blast is
    /// serialized (may be all zeros). JSON key: `confidenceSummary`.
    #[serde(default)]
    pub confidence_summary: EdgeConfidenceSummary,
}

impl BlastRadius {
    /// True when the section has nothing useful to emit (quiet packets stay small).
    /// Zero-only `confidence_summary` does not keep an otherwise-empty blast alive.
    pub fn is_empty_for_serde(&self) -> bool {
        self.edges.is_empty()
            && self.must_touch_files.is_empty()
            && self.must_touch_symbols.is_empty()
            && self.test_hints.is_empty()
            && self.honesty_notes.is_empty()
    }
}

impl Eq for BlastRadius {}

/// Serde helper: omit `blastRadius` when None or empty.
pub fn blast_radius_skip(value: &Option<BlastRadius>) -> bool {
    match value {
        None => true,
        Some(br) => br.is_empty_for_serde(),
    }
}
