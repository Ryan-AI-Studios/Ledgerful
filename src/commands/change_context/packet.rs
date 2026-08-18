//! Agent change-context packet types (track 0114 / 0192).

use miette::Result;
use serde::{Deserialize, Serialize};

/// Packet schema version (doctor/verify style u32, not ImpactPacket's string).
pub const CHANGE_CONTEXT_SCHEMA_VERSION: u32 = 1;

/// Default max `readSet` entries.
pub const DEFAULT_MAX_FILES: usize = 20;

/// Doctor sidecar older than this is marked `stale` (counts still exposed).
pub(crate) const DOCTOR_STALE_AFTER_HOURS: i64 = 24;

/// Error class for `not_ready` nextActions (track 0124 B5).
///
/// Pure-RO / permission failures must not suggest Class C recovery
/// (`doctor` / `init` / `index`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NotReadyErrorClass {
    /// State/DB exists but open/mkdir/write failed under pure RO or OS deny.
    PermissionDenied,
    /// `ledger.db` does not exist / storage not initialized.
    MissingDb,
    /// RO open failed schema currency check.
    SchemaStale,
    /// Layout / git discover failed.
    LayoutUnavailable,
    /// Generic failure — writable-env triad still appropriate.
    Other,
}

/// Detail level for risk reasons / warnings / coupling names in the packet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ChangeContextDetail {
    #[default]
    Minimal,
    Standard,
}

impl ChangeContextDetail {
    pub fn parse(s: &str) -> Result<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "minimal" | "" => Ok(Self::Minimal),
            "standard" => Ok(Self::Standard),
            other => Err(miette::miette!(
                "invalid --detail '{other}' (expected minimal|standard)"
            )),
        }
    }
}

/// Builder options for [`build_change_context`].
#[derive(Debug, Clone)]
pub struct ChangeContextOpts {
    pub detail: ChangeContextDetail,
    pub max_files: usize,
    pub base_ref: Option<String>,
    pub blast_depth: Option<u32>,
    /// Prospective paths (0173). Mutually exclusive with `base_ref`.
    pub paths: Vec<String>,
    /// When true, pathMode=all — restore pre-0173 governance temporal risk/readSet.
    pub include_governance: bool,
}

impl Default for ChangeContextOpts {
    fn default() -> Self {
        Self {
            detail: ChangeContextDetail::Minimal,
            max_files: DEFAULT_MAX_FILES,
            base_ref: None,
            blast_depth: None,
            paths: Vec::new(),
            include_governance: false,
        }
    }
}

/// Canonical agent change-context packet (schemaVersion 1).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ChangeContextPacket {
    pub schema_version: u32,
    pub status: String,
    pub summary: String,
    /// Structured scannable header (0173). Coexists with freeform `summary`.
    /// Present on `ready`/`empty`; omitted on `not_ready`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_summary: Option<AgentSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub head_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub risk_level: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub risk_reasons: Vec<String>,
    #[serde(default)]
    pub read_set: Vec<ReadSetEntry>,
    pub read_set_capped: bool,
    pub read_set_total_candidates: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blast: Option<BlastSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub test_coverage: Option<TestCoverageSummary>,
    /// Nested affected HTTP flows summary (0118). Present for both minimal and
    /// standard detail; sample size is detail-aware (5 / 10).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub affected_flows: Option<AffectedFlowsSummary>,
    /// Greenfield / new-surface hints + budgeted suggested tests (0127).
    /// Present only when `impact.changes` is non-empty; omitted on empty/not_ready.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub change_hints: Option<crate::impact::enrichment::change_hints::ChangeHintsReport>,
    pub doctor: DoctorSection,
    pub ledger: LedgerSection,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub analysis_warnings: Vec<String>,
    #[serde(default)]
    pub next_actions: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub impact_schema_version: Option<String>,
}

/// Structured agent scannable header (0173). Coexists with freeform `summary`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AgentSummary {
    pub risk_one_liner: String,
    pub changed: ChangedClassCounts,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub top_symbols: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub must_touch_sample: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub suggested_tests_sample: Vec<String>,
    pub demoted_temporal_count: u32,
    pub path_mode: String,
    pub analysis_mode: String,
}

/// Per-class counts of changed files for agentSummary.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct ChangedClassCounts {
    pub total: usize,
    pub code: usize,
    pub governance: usize,
    pub contract: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ReadSetEntry {
    pub path: String,
    pub reason: String,
    pub priority: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BlastSummary {
    pub depth: u32,
    pub must_touch_file_count: usize,
    pub must_touch_symbol_count: usize,
    /// Class counts only (0117). Same shape as `blastRadius.confidenceSummary`.
    /// Present at both `minimal` and `standard` detail. **No** edges array.
    pub confidence_summary: crate::impact::enrichment::edge_confidence::EdgeConfidenceSummary,
}

/// Deepened test-coverage / gap summary (0115). Re-exports the shared library
/// report so change-context, impact, and scan --pr share one schema.
pub type TestCoverageSummary = crate::impact::enrichment::test_gaps::TestGapsReport;

/// Nested affected-flows summary (0118). Same schema as ImpactPacket /
/// PR `affectedFlows`, with a detail-aware sample of `flows` (not full cap-20).
pub type AffectedFlowsSummary = crate::impact::enrichment::affected_flows::AffectedFlowsReport;

/// Sample cap for `affectedFlows.flows` at minimal detail.
pub(crate) const AFFECTED_FLOWS_SAMPLE_MINIMAL: usize = 5;
/// Sample cap for `affectedFlows.flows` at standard detail.
pub(crate) const AFFECTED_FLOWS_SAMPLE_STANDARD: usize = 10;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DoctorSection {
    pub status: String,
    pub ready_for_publish: bool,
    pub block: u64,
    pub warn: u64,
    pub info: u64,
    #[serde(default)]
    pub top_findings: Vec<DoctorTopFinding>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DoctorTopFinding {
    pub code: String,
    pub severity: String,
    pub message: String,
    /// Optional copy-paste remediation from doctor (0125/0129). Never invent;
    /// omit from JSON when `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remediation: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct LedgerSection {
    pub pending_count: usize,
    #[serde(default)]
    pub active_tx: Vec<ActiveTxEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ActiveTxEntry {
    pub tx_id: String,
    pub entity: String,
    pub category: String,
}

impl ChangeContextOpts {
    /// CLI construction path: parse `--detail`, clamp `--max-files` to at least 1,
    /// and refuse `--paths` with `--base-ref`. MCP keeps its own checks.
    pub fn from_cli(
        detail: Option<String>,
        max_files: Option<usize>,
        base_ref: Option<String>,
        blast_depth: Option<u32>,
        paths: Vec<String>,
        include_governance: bool,
    ) -> Result<Self> {
        if !paths.is_empty() && base_ref.is_some() {
            return Err(miette::miette!(
                "--paths and --base-ref are mutually exclusive"
            ));
        }
        let detail = match detail {
            Some(s) => ChangeContextDetail::parse(&s)?,
            None => ChangeContextDetail::Minimal,
        };
        let max_files = max_files.unwrap_or(DEFAULT_MAX_FILES).max(1);
        Ok(Self {
            detail,
            max_files,
            base_ref,
            blast_depth,
            paths,
            include_governance,
        })
    }
}

/// Greppable next-action when greenfield suggestions are present (0127 B4).
pub const GREENFIELD_SUGGESTED_TESTS_ACTION: &str =
    "review changeHints.suggestedTests and add covering tests for new surfaces";
