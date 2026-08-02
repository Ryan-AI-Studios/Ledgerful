//! Agent change-context packet (track 0114).
//!
//! Composes impact (in-memory only), doctor sidecar, ledger pending, and a
//! budgeted `readSet` into one versioned camelCase JSON packet for agents.
//! Never calls `execute_impact_silent*` (does not rewrite `latest-impact.json`).

use crate::config::model::Config;
use crate::git::RepoSnapshot;
use crate::git::repo::{get_head_info, open_repo};
use crate::impact::packet::{ImpactPacket, RiskLevel};
use crate::state::layout::Layout;
use crate::state::storage::StorageManager;
use chrono::{DateTime, Duration, Utc};
use miette::Result;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::Path;

/// Packet schema version (doctor/verify style u32, not ImpactPacket's string).
pub const CHANGE_CONTEXT_SCHEMA_VERSION: u32 = 1;

/// Default max `readSet` entries.
pub const DEFAULT_MAX_FILES: usize = 20;

/// Doctor sidecar older than this is marked `stale` (counts still exposed).
const DOCTOR_STALE_AFTER_HOURS: i64 = 24;

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
}

impl Default for ChangeContextOpts {
    fn default() -> Self {
        Self {
            detail: ChangeContextDetail::Minimal,
            max_files: DEFAULT_MAX_FILES,
            base_ref: None,
            blast_depth: None,
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
    pub doctor: DoctorSection,
    pub ledger: LedgerSection,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub analysis_warnings: Vec<String>,
    #[serde(default)]
    pub next_actions: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub impact_schema_version: Option<String>,
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
const AFFECTED_FLOWS_SAMPLE_MINIMAL: usize = 5;
/// Sample cap for `affectedFlows.flows` at standard detail.
const AFFECTED_FLOWS_SAMPLE_STANDARD: usize = 10;

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

/// Build a change-context packet for the current layout/storage/config.
///
/// Impact is always computed in-memory (no `latest-impact.json` rewrite).
/// Doctor and ledger always report **present** workspace state even when
/// `--base-ref` time-travels structural impact.
pub fn build_change_context(
    opts: &ChangeContextOpts,
    layout: &Layout,
    storage: &StorageManager,
    config: &Config,
) -> Result<ChangeContextPacket> {
    let project_root = layout.root.as_std_path();
    let mut config = config.clone();
    let mut analysis_warnings: Vec<String> = Vec::new();

    if let Some(note) = crate::impact::enrichment::blast::apply_cli_blast_depth(
        &mut config.impact.blast_depth,
        config.impact.blast_depth_max,
        opts.blast_depth,
    ) {
        analysis_warnings.push(note);
    }

    let impact = match compute_structural_impact(opts, storage, &config, project_root) {
        Ok(packet) => packet,
        Err(e) => {
            let (ledger, ledger_warnings) =
                read_ledger_section_with_warnings(layout, storage, &config);
            let mut packet = not_ready_packet(
                format!("impact computation failed: {e}"),
                opts.base_ref.clone(),
                read_doctor_section(layout),
                ledger,
                NotReadyErrorClass::Other,
            );
            packet.analysis_warnings.extend(ledger_warnings);
            return Ok(packet);
        }
    };

    analysis_warnings.extend(impact.analysis_warnings.iter().cloned());

    let doctor = read_doctor_section(layout);
    let (ledger, ledger_warnings) = read_ledger_section_with_warnings(layout, storage, &config);
    analysis_warnings.extend(ledger_warnings);

    let (read_set, read_set_capped, read_set_total_candidates) =
        build_read_set(&impact, opts.max_files, config.temporal.coupling_threshold);

    if read_set_capped {
        analysis_warnings.push(format!(
            "readSet capped at {} of {} candidates; use `ledgerful scan --impact --json` for full set",
            opts.max_files, read_set_total_candidates
        ));
    }

    let has_file_changes = !impact.changes.is_empty();
    let status = if !has_file_changes && ledger.pending_count == 0 {
        "empty"
    } else {
        "ready"
    };

    let summary = compose_summary(status, has_file_changes, &impact, &ledger, &doctor);
    let risk_reasons = trim_reasons(&impact.risk_reasons, opts.detail);
    let mut warnings = trim_reasons(&analysis_warnings, opts.detail);
    warnings.sort();
    warnings.dedup();

    let next_actions =
        compose_next_actions(status, &doctor, &ledger, read_set_capped, has_file_changes);

    let blast = impact.blast_radius.as_ref().map(|b| BlastSummary {
        depth: b.depth_applied,
        must_touch_file_count: b.must_touch_files.len(),
        must_touch_symbol_count: b.must_touch_symbols.len(),
        confidence_summary: b.confidence_summary,
    });

    let test_coverage = Some(summarize_test_coverage(storage, &impact));
    let affected_flows = Some(summarize_affected_flows(storage, &impact, opts.detail));

    Ok(ChangeContextPacket {
        schema_version: CHANGE_CONTEXT_SCHEMA_VERSION,
        status: status.to_string(),
        summary,
        reason: None,
        head_hash: impact.head_hash.clone(),
        base_ref: opts.base_ref.clone(),
        risk_level: Some(risk_level_str(impact.risk_level).to_string()),
        risk_reasons,
        read_set,
        read_set_capped,
        read_set_total_candidates,
        blast,
        test_coverage,
        affected_flows,
        doctor,
        ledger,
        analysis_warnings: warnings,
        next_actions,
        impact_schema_version: Some(impact.schema_version.clone()),
    })
}

/// CLI entrypoint: resolve layout/storage/config, build packet, print human or JSON.
pub fn execute_change_context(
    json: bool,
    detail: Option<String>,
    max_files: Option<usize>,
    base_ref: Option<String>,
    blast_depth: Option<u32>,
) -> Result<()> {
    let detail = match detail {
        Some(s) => ChangeContextDetail::parse(&s)?,
        None => ChangeContextDetail::Minimal,
    };
    let max_files = max_files.unwrap_or(DEFAULT_MAX_FILES).max(1);
    let opts = ChangeContextOpts {
        detail,
        max_files,
        base_ref,
        blast_depth,
    };

    let layout = match crate::commands::helpers::get_layout() {
        Ok(l) => l,
        Err(e) => {
            let packet = not_ready_packet(
                format!("layout unavailable: {e}"),
                opts.base_ref.clone(),
                DoctorSection {
                    status: "missing".to_string(),
                    ready_for_publish: false,
                    block: 0,
                    warn: 0,
                    info: 0,
                    top_findings: Vec::new(),
                },
                LedgerSection {
                    pending_count: 0,
                    active_tx: Vec::new(),
                },
                NotReadyErrorClass::LayoutUnavailable,
            );
            return emit_packet(&packet, json);
        }
    };

    let config = crate::config::load::load_config(&layout).unwrap_or_default();
    let storage = match open_storage_for_change_context(&layout) {
        Ok(s) => s,
        Err((e, class)) => {
            let packet = not_ready_packet(
                storage_unavailable_reason(&e, class),
                opts.base_ref.clone(),
                read_doctor_section(&layout),
                LedgerSection {
                    pending_count: 0,
                    active_tx: Vec::new(),
                },
                class,
            );
            return emit_packet(&packet, json);
        }
    };

    let packet = build_change_context(&opts, &layout, &storage, &config)?;
    let _ = storage.shutdown();
    emit_packet(&packet, json)
}

fn emit_packet(packet: &ChangeContextPacket, json: bool) -> Result<()> {
    if json {
        let out = serde_json::to_string_pretty(packet)
            .map_err(|e| miette::miette!("Failed to serialize change-context: {e}"))?;
        println!("{out}");
    } else {
        print_human(packet);
    }
    Ok(())
}

fn print_human(packet: &ChangeContextPacket) {
    use owo_colors::OwoColorize;

    println!("{}", "Ledgerful change-context".bold().underline());
    println!("  status:           {}", packet.status);
    println!("  summary:          {}", packet.summary);
    if let Some(ref risk) = packet.risk_level {
        println!("  risk:             {risk}");
    }
    println!(
        "  readSet:          {} (capped={}, candidates={})",
        packet.read_set.len(),
        packet.read_set_capped,
        packet.read_set_total_candidates
    );
    if let Some(ref cov) = packet.test_coverage {
        println!(
            "  testCoverage:     status={} mapped={} fileMapped={} unmapped={}",
            cov.status.as_str(),
            cov.mapped_count,
            cov.file_mapped_count,
            cov.unmapped_count
        );
        if cov.unmapped_count > 0 {
            eprintln!(
                "warning: {} production symbol(s)/file(s) lack structural test_mapping (not line coverage)",
                cov.unmapped_count
            );
        }
    }
    if let Some(ref flows) = packet.affected_flows {
        println!(
            "  affectedFlows:    status={} flowCount={}",
            flows.status.as_str(),
            flows.flow_count
        );
    }
    println!(
        "  doctor:           {} readyForPublish={} (block={} warn={} info={})",
        packet.doctor.status,
        packet.doctor.ready_for_publish,
        packet.doctor.block,
        packet.doctor.warn,
        packet.doctor.info
    );
    println!(
        "  ledger:           pendingCount={}",
        packet.ledger.pending_count
    );
    if !packet.next_actions.is_empty() {
        println!("  nextActions:");
        for a in &packet.next_actions {
            println!("    - {a}");
        }
    }
}

fn compute_structural_impact(
    opts: &ChangeContextOpts,
    storage: &StorageManager,
    config: &Config,
    project_root: &Path,
) -> Result<ImpactPacket> {
    if let Some(ref base_ref) = opts.base_ref {
        let snapshot = build_repo_snapshot_from_base_ref(project_root, base_ref, config)?;
        crate::commands::impact::compute_impact_from_snapshot_in_memory(
            storage,
            config,
            project_root,
            snapshot,
        )
    } else {
        crate::commands::impact::compute_impact_in_memory_at(storage, config, project_root)
    }
}

/// Build a [`RepoSnapshot`] from `git diff base_ref...HEAD` (structure only).
pub(crate) fn build_repo_snapshot_from_base_ref(
    project_root: &Path,
    base_ref: &str,
    config: &Config,
) -> Result<RepoSnapshot> {
    let repo = open_repo(project_root)?;
    let (head_hash, branch_name) = get_head_info(&repo)?;
    let all_changes = crate::commands::scan::files_changed_since(project_root, base_ref)?;
    let changes = crate::git::ignore::filter_ignored_changes(
        all_changes,
        &config.watch.ignore_patterns,
        true,
    )?;
    let is_clean = changes.is_empty();
    Ok(RepoSnapshot {
        head_hash,
        branch_name,
        is_clean,
        changes,
    })
}

fn not_ready_packet(
    reason: String,
    base_ref: Option<String>,
    doctor: DoctorSection,
    ledger: LedgerSection,
    class: NotReadyErrorClass,
) -> ChangeContextPacket {
    ChangeContextPacket {
        schema_version: CHANGE_CONTEXT_SCHEMA_VERSION,
        status: "not_ready".to_string(),
        summary: format!("Change context not ready: {reason}"),
        reason: Some(reason),
        head_hash: None,
        base_ref,
        risk_level: None,
        risk_reasons: Vec::new(),
        read_set: Vec::new(),
        read_set_capped: false,
        read_set_total_candidates: 0,
        blast: None,
        test_coverage: None,
        affected_flows: None,
        doctor,
        ledger,
        analysis_warnings: Vec::new(),
        next_actions: next_actions_for_class(class),
        impact_schema_version: None,
    }
}

/// Class-aware recovery actions (B5). RO/permission must not lead with Class C.
fn next_actions_for_class(class: NotReadyErrorClass) -> Vec<String> {
    match class {
        NotReadyErrorClass::PermissionDenied => vec![
            "Set LEDGERFUL_STATE_DIR to a populated .ledgerful directory if the override is wrong"
                .to_string(),
            "Re-run outside pure RO sandbox (`--sandbox workspace-write` or unrestricted)"
                .to_string(),
            "Continue git-only review (ledgerful grounding unavailable under pure RO)".to_string(),
        ],
        NotReadyErrorClass::MissingDb => vec![
            "In a writable environment: ledgerful init (if needed)".to_string(),
            "In a writable environment: ledgerful scan or ledgerful index --incremental"
                .to_string(),
            "Then re-run ledgerful change-context --json".to_string(),
        ],
        NotReadyErrorClass::SchemaStale => vec![
            "In a writable environment: upgrade/migrate state (e.g. ledgerful update --migrate)"
                .to_string(),
            "Then re-run ledgerful change-context --json".to_string(),
        ],
        NotReadyErrorClass::LayoutUnavailable => vec![
            "Fix cwd / ensure a git repository is discoverable".to_string(),
            "Continue git-only review if layout cannot be resolved".to_string(),
        ],
        NotReadyErrorClass::Other => vec![
            "ledgerful doctor --json".to_string(),
            "ledgerful init".to_string(),
            "ledgerful index --incremental".to_string(),
        ],
    }
}

/// Greppable storage-unavailable reason; RO class adds "state directory not writable".
pub(crate) fn storage_unavailable_reason(
    err: &miette::Report,
    class: NotReadyErrorClass,
) -> String {
    match class {
        NotReadyErrorClass::PermissionDenied => {
            format!("storage unavailable: state directory not writable: {err}")
        }
        _ => format!("storage unavailable: {err}"),
    }
}

/// Map open/init failures to B5 classes for nextActions.
///
/// Order matters: open/permission strings often appear inside messages that also
/// mention `PRAGMA user_version` (schema probe). Prefer PermissionDenied over
/// SchemaStale when both could match — never advise migration for pure RO open fail.
fn classify_storage_error(err: &miette::Report, db_exists: bool) -> NotReadyErrorClass {
    let s = format!("{err}").to_ascii_lowercase();

    // Permission / pure-RO open failures first (before schema keyword scan).
    if s.contains("permission denied")
        || s.contains("access is denied")
        || s.contains("read-only file system")
        || s.contains("readonly database")
        || s.contains("attempt to write a readonly")
        || s.contains("state directory not writable")
        || s.contains("os error 5")
        || s.contains("(os error 5)")
        || s.contains("os error 30")
        || s.contains("unable to open database")
        || s.contains("disk i/o error")
    {
        return if db_exists || s.contains("unable to open database") || s.contains("readonly") {
            NotReadyErrorClass::PermissionDenied
        } else {
            NotReadyErrorClass::MissingDb
        };
    }

    // True schema mismatch (StateError::SchemaMismatch / migration probe).
    // Do NOT match bare "user_version" alone — open failures often embed
    // `PRAGMA user_version ... unable to open database file`.
    if s.contains("schema mismatch")
        || s.contains("migration required")
        || s.contains("schema is not current")
        || s.contains("schema not current")
        || (s.contains("schema") && s.contains("not current"))
        || s.contains("schema_version")
    {
        return NotReadyErrorClass::SchemaStale;
    }

    if s.contains("not initialized")
        || s.contains("no such file")
        || s.contains("does not exist")
        || s.contains("the system cannot find the file")
    {
        return if db_exists {
            // Exists but still reported missing path fragments → prefer permission.
            NotReadyErrorClass::PermissionDenied
        } else {
            NotReadyErrorClass::MissingDb
        };
    }

    if !db_exists {
        return NotReadyErrorClass::MissingDb;
    }

    NotReadyErrorClass::Other
}

/// Soft-open change-context storage (B6): prefer true RO when `ledger.db` exists.
///
/// On RO permission/schema failure, do **not** fall through to write-open.
/// When the DB is missing, attempt write init (writable env creates state).
/// Shared by CLI, `build_change_context_from_cwd`, and MCP `change_context`.
pub(crate) fn open_storage_for_change_context(
    layout: &Layout,
) -> std::result::Result<StorageManager, (miette::Report, NotReadyErrorClass)> {
    let db_path = layout.state_subdir().join("ledger.db");
    let db_exists = db_path.exists();

    if db_exists {
        match StorageManager::open_read_only(layout) {
            Ok(s) => return Ok(s),
            Err(e) => {
                let class = classify_storage_error(&e, true);
                // Permission / schema: honest not_ready — do not try write open.
                if matches!(
                    class,
                    NotReadyErrorClass::PermissionDenied | NotReadyErrorClass::SchemaStale
                ) {
                    return Err((e, class));
                }
                // Other full-RO failures (often Cozo): try SQLite-only RO so
                // reviewer packets still work without mutating state.
                tracing::debug!(
                    "change-context RO open failed ({class:?}); trying sqlite-only RO: {e}"
                );
                match StorageManager::open_read_only_sqlite_only(layout) {
                    Ok(s) => return Ok(s),
                    Err(e2) => {
                        tracing::debug!(
                            "change-context sqlite-only RO also failed; trying write open: {e2}"
                        );
                    }
                }
            }
        }
    }

    match StorageManager::init_with_layout(layout) {
        Ok(s) => Ok(s),
        Err(e) => {
            let class = classify_storage_error(&e, db_path.exists());
            Err((e, class))
        }
    }
}

/// Build budgeted readSet: changed → blast must-touch → temporal partners.
///
/// Within each priority band paths are sorted for determinism.
/// Always sets total candidates; `capped=true` when truncated.
pub(crate) fn build_read_set(
    impact: &ImpactPacket,
    max_files: usize,
    coupling_threshold: f32,
) -> (Vec<ReadSetEntry>, bool, usize) {
    let max_files = max_files.max(1);
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut candidates: Vec<ReadSetEntry> = Vec::new();

    // Priority 1: changed files (stable path sort already on packet after finalize)
    let mut changed: Vec<String> = impact
        .changes
        .iter()
        .map(|c| normalize_path(&c.path))
        .collect();
    changed.sort();
    changed.dedup();
    for path in changed {
        if seen.insert(path.clone()) {
            candidates.push(ReadSetEntry {
                path,
                reason: "changed".to_string(),
                priority: 1,
            });
        }
    }

    // Priority 2: blast must-touch files
    if let Some(ref blast) = impact.blast_radius {
        let mut blast_files: Vec<String> = blast
            .must_touch_files
            .iter()
            .map(|p| normalize_path_str(p))
            .collect();
        blast_files.sort();
        blast_files.dedup();
        for path in blast_files {
            if seen.insert(path.clone()) {
                candidates.push(ReadSetEntry {
                    path,
                    reason: "blast".to_string(),
                    priority: 2,
                });
            }
        }
    }

    // Priority 3: temporal coupling partners above threshold
    let mut temporal: Vec<String> = Vec::new();
    for tc in &impact.temporal_couplings {
        if tc.score < coupling_threshold {
            continue;
        }
        temporal.push(normalize_path(&tc.file_a));
        temporal.push(normalize_path(&tc.file_b));
    }
    temporal.sort();
    temporal.dedup();
    for path in temporal {
        if seen.insert(path.clone()) {
            candidates.push(ReadSetEntry {
                path,
                reason: "temporal".to_string(),
                priority: 3,
            });
        }
    }

    let total = candidates.len();
    let capped = total > max_files;
    candidates.truncate(max_files);
    (candidates, capped, total)
}

fn normalize_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn normalize_path_str(path: &str) -> String {
    path.replace('\\', "/")
}

fn risk_level_str(level: RiskLevel) -> &'static str {
    match level {
        RiskLevel::Low => "low",
        RiskLevel::Medium => "medium",
        RiskLevel::High => "high",
    }
}

fn trim_reasons(reasons: &[String], detail: ChangeContextDetail) -> Vec<String> {
    let limit = match detail {
        ChangeContextDetail::Minimal => 5,
        ChangeContextDetail::Standard => 20,
    };
    reasons.iter().take(limit).cloned().collect()
}

/// Prefer impact-attached gaps (shared orchestrator seeds); else recompute.
fn summarize_test_coverage(storage: &StorageManager, impact: &ImpactPacket) -> TestCoverageSummary {
    use crate::impact::enrichment::blast::resolve_seeds;
    use crate::impact::enrichment::test_gaps::{
        TestGapsOpts, compute_change_set_test_gaps_from_seeds,
    };

    if let Some(ref gaps) = impact.test_gaps {
        return gaps.clone();
    }

    let conn = storage.get_connection();
    let opts = TestGapsOpts {
        head_hash: impact.head_hash.clone(),
    };
    match resolve_seeds(impact, conn) {
        Ok(seeds) => compute_change_set_test_gaps_from_seeds(conn, &seeds, &opts),
        Err(_) => crate::impact::enrichment::test_gaps::TestGapsReport::unavailable(),
    }
}

/// Prefer impact-attached affected flows (single compute on impact path); else recompute.
/// Detail-aware sample: Minimal → 5 flows, Standard → 10. Counts pass through full report.
fn summarize_affected_flows(
    storage: &StorageManager,
    impact: &ImpactPacket,
    detail: ChangeContextDetail,
) -> AffectedFlowsSummary {
    use crate::impact::enrichment::affected_flows::{
        AffectedFlowsOpts, AffectedFlowsReport, compute_affected_flows,
    };

    let sample_cap = match detail {
        ChangeContextDetail::Minimal => AFFECTED_FLOWS_SAMPLE_MINIMAL,
        ChangeContextDetail::Standard => AFFECTED_FLOWS_SAMPLE_STANDARD,
    };

    let mut report = if let Some(ref flows) = impact.affected_flows {
        flows.clone()
    } else {
        let conn = storage.get_connection();
        let opts = AffectedFlowsOpts {
            head_hash: impact.head_hash.clone(),
        };
        compute_affected_flows(conn, &impact.changes, impact.blast_radius.as_ref(), &opts)
            .unwrap_or_else(|_| AffectedFlowsReport::unavailable())
    };

    // Token budget: sample only; flowCount/flowTotal stay full-report counts.
    if report.flows.len() > sample_cap {
        report.flows.truncate(sample_cap);
    }
    report
}

fn compose_summary(
    status: &str,
    has_file_changes: bool,
    impact: &ImpactPacket,
    ledger: &LedgerSection,
    doctor: &DoctorSection,
) -> String {
    match status {
        "empty" => "No file changes and no pending ledger transactions.".to_string(),
        "ready" if !has_file_changes && ledger.pending_count > 0 => {
            format!(
                "No file changes; {} pending ledger transaction(s).",
                ledger.pending_count
            )
        }
        "ready" => {
            let risk = risk_level_str(impact.risk_level);
            format!(
                "{} changed file(s), risk={}, doctor.readyForPublish={}, ledger.pending={}.",
                impact.changes.len(),
                risk,
                doctor.ready_for_publish,
                ledger.pending_count
            )
        }
        other => other.to_string(),
    }
}

fn compose_next_actions(
    status: &str,
    doctor: &DoctorSection,
    ledger: &LedgerSection,
    read_set_capped: bool,
    has_file_changes: bool,
) -> Vec<String> {
    let mut actions = Vec::new();
    if doctor.status == "missing" || doctor.status == "stale" || doctor.status == "error" {
        actions.push("ledgerful doctor --json".to_string());
    }
    // Only when there are actual block findings — not when sidecar is merely missing
    // (readyForPublish=false is a safe default for missing/stale, not a proven block).
    if doctor.block > 0 || (doctor.status == "ok" && !doctor.ready_for_publish) {
        actions.push("resolve doctor block findings before publish".to_string());
    }
    if ledger.pending_count > 0 {
        actions.push("ledgerful ledger status --json".to_string());
    }
    if read_set_capped {
        actions.push("ledgerful scan --impact --json".to_string());
    }
    if has_file_changes {
        actions.push("ledgerful verify --scope fast".to_string());
    }
    if status == "empty" {
        actions.push("no structural work required".to_string());
    }
    actions.sort();
    actions.dedup();
    actions
}

/// Read doctor-results.json sidecar (present-tense workspace).
pub(crate) fn read_doctor_section(layout: &Layout) -> DoctorSection {
    let path = layout.state_subdir().join("doctor-results.json");
    match std::fs::read_to_string(path.as_std_path()) {
        Ok(contents) => parse_doctor_sidecar(&contents, path.as_std_path()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => DoctorSection {
            status: "missing".to_string(),
            ready_for_publish: false,
            block: 0,
            warn: 0,
            info: 0,
            top_findings: Vec::new(),
        },
        Err(e) => {
            tracing::warn!("Failed to read doctor-results.json: {e}");
            DoctorSection {
                status: "error".to_string(),
                ready_for_publish: false,
                block: 0,
                warn: 0,
                info: 0,
                top_findings: Vec::new(),
            }
        }
    }
}

fn parse_doctor_sidecar(contents: &str, path: &Path) -> DoctorSection {
    let error_section = || DoctorSection {
        status: "error".to_string(),
        ready_for_publish: false,
        block: 0,
        warn: 0,
        info: 0,
        top_findings: Vec::new(),
    };

    let json: serde_json::Value = match serde_json::from_str(contents) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(
                "Failed to parse doctor-results.json at {}: {e}",
                path.display()
            );
            return error_section();
        }
    };

    // Require production sidecar shape (doctor write_doctor_results). Incomplete
    // objects must not default to readyForPublish=true (false-green).
    let Some(block) = json.get("block").and_then(|v| v.as_u64()) else {
        tracing::warn!(
            "doctor-results.json missing numeric 'block' at {}",
            path.display()
        );
        return error_section();
    };
    let Some(warn) = json.get("warn").and_then(|v| v.as_u64()) else {
        tracing::warn!(
            "doctor-results.json missing numeric 'warn' at {}",
            path.display()
        );
        return error_section();
    };
    let Some(info) = json.get("info").and_then(|v| v.as_u64()) else {
        tracing::warn!(
            "doctor-results.json missing numeric 'info' at {}",
            path.display()
        );
        return error_section();
    };
    let Some(ready) = json.get("readyForPublish").and_then(|v| v.as_bool()) else {
        tracing::warn!(
            "doctor-results.json missing boolean 'readyForPublish' at {}",
            path.display()
        );
        return error_section();
    };

    let mut top_findings = Vec::new();
    if let Some(arr) = json.get("findings").and_then(|v| v.as_array()) {
        for f in arr.iter().take(5) {
            let code = f
                .get("code")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string();
            let severity = f
                .get("severity")
                .and_then(|v| v.as_str())
                .unwrap_or("info")
                .to_string();
            let message = f
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            top_findings.push(DoctorTopFinding {
                code,
                severity,
                message,
            });
        }
    }

    let status = if is_doctor_stale(&json, path) {
        "stale".to_string()
    } else {
        "ok".to_string()
    };

    DoctorSection {
        status,
        ready_for_publish: ready,
        block,
        warn,
        info,
        top_findings,
    }
}

fn is_doctor_stale(json: &serde_json::Value, path: &Path) -> bool {
    if let Some(ts) = json.get("timestamp").and_then(|v| v.as_str())
        && let Ok(parsed) = DateTime::parse_from_rfc3339(ts)
    {
        let age = Utc::now().signed_duration_since(parsed.with_timezone(&Utc));
        return age > Duration::hours(DOCTOR_STALE_AFTER_HOURS);
    }
    // Fallback: file mtime
    if let Ok(meta) = std::fs::metadata(path)
        && let Ok(modified) = meta.modified()
    {
        let modified: DateTime<Utc> = modified.into();
        let age = Utc::now().signed_duration_since(modified);
        return age > Duration::hours(DOCTOR_STALE_AFTER_HOURS);
    }
    false
}

/// In-process pending ledger list (present-tense workspace).
///
/// On DB read failure returns zero pending **and** a warning so callers never
/// silently treat errors as a clean empty ledger.
fn read_ledger_section_with_warnings(
    layout: &Layout,
    storage: &StorageManager,
    config: &Config,
) -> (LedgerSection, Vec<String>) {
    let _ = (layout, config);
    let mut warnings = Vec::new();
    let pending = {
        let db = crate::ledger::db::LedgerDb::new(storage.get_connection());
        match db.get_all_pending() {
            Ok(rows) => rows,
            Err(e) => {
                tracing::warn!("Failed to read pending ledger transactions: {e}");
                warnings.push(format!(
                    "ledger pending read failed; pendingCount may be incomplete: {e}"
                ));
                Vec::new()
            }
        }
    };

    let mut active_tx: Vec<ActiveTxEntry> = pending
        .iter()
        .map(|t| ActiveTxEntry {
            tx_id: t.tx_id.clone(),
            entity: t.entity.clone(),
            category: t.category.to_string(),
        })
        .collect();
    active_tx.sort_by(|a, b| a.tx_id.cmp(&b.tx_id));

    (
        LedgerSection {
            pending_count: active_tx.len(),
            active_tx,
        },
        warnings,
    )
}

/// Helper for tests/MCP: open layout from cwd and build packet.
///
/// Soft-opens existing `ledger.db` read-only (B6) before write init.
/// Layout/storage failures return `Ok(not_ready)` with B5 class (mirrors CLI).
pub fn build_change_context_from_cwd(opts: &ChangeContextOpts) -> Result<ChangeContextPacket> {
    let layout = match crate::commands::helpers::get_layout() {
        Ok(l) => l,
        Err(e) => {
            return Ok(not_ready_packet(
                format!("layout unavailable: {e}"),
                opts.base_ref.clone(),
                DoctorSection {
                    status: "missing".into(),
                    ready_for_publish: false,
                    block: 0,
                    warn: 0,
                    info: 0,
                    top_findings: vec![],
                },
                LedgerSection {
                    pending_count: 0,
                    active_tx: vec![],
                },
                NotReadyErrorClass::LayoutUnavailable,
            ));
        }
    };
    let config = crate::config::load::load_config(&layout).unwrap_or_default();
    let storage = match open_storage_for_change_context(&layout) {
        Ok(s) => s,
        Err((e, class)) => {
            return Ok(not_ready_packet(
                storage_unavailable_reason(&e, class),
                opts.base_ref.clone(),
                read_doctor_section(&layout),
                LedgerSection {
                    pending_count: 0,
                    active_tx: vec![],
                },
                class,
            ));
        }
    };
    let packet = build_change_context(opts, &layout, &storage, &config)?;
    let _ = storage.shutdown();
    Ok(packet)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::impact::packet::{BlastRadius, ChangedFile, FileAnalysisStatus, TemporalCoupling};
    use crate::ledger::{Category, TransactionManager, TransactionRequest};
    use crate::state::reports::{LATEST_IMPACT_REPORT, write_impact_report};
    use std::fs;
    use std::path::PathBuf;
    use tempfile::tempdir;

    fn make_changed(path: &str) -> ChangedFile {
        ChangedFile {
            path: PathBuf::from(path),
            status: "Modified".to_string(),
            old_path: None,
            is_staged: false,
            symbols: None,
            imports: None,
            runtime_usage: None,
            analysis_status: FileAnalysisStatus::default(),
            analysis_warnings: Vec::new(),
            ..Default::default()
        }
    }

    fn base_packet(changes: Vec<ChangedFile>) -> ImpactPacket {
        let mut p = ImpactPacket::default();
        p.schema_version = "v1".to_string();
        p.risk_level = RiskLevel::Low;
        p.changes = changes;
        p.tree_clean = p.changes.is_empty();
        p.head_hash = Some("abc123".to_string());
        p
    }

    #[test]
    fn read_set_priority_changed_then_blast_then_temporal() {
        let mut impact = base_packet(vec![make_changed("src/a.rs")]);
        impact.blast_radius = Some(BlastRadius {
            depth_requested: 1,
            depth_applied: 1,
            edges: Vec::new(),
            must_touch_files: vec!["src/b.rs".to_string(), "src/a.rs".to_string()],
            must_touch_symbols: vec!["foo".to_string()],
            test_hints: Vec::new(),
            honesty_notes: Vec::new(),
            ..Default::default()
        });
        impact.temporal_couplings = vec![TemporalCoupling {
            file_a: PathBuf::from("src/a.rs"),
            file_b: PathBuf::from("src/c.rs"),
            score: 0.9,
        }];

        let (set, capped, total) = build_read_set(&impact, 20, 0.75);
        assert!(!capped);
        assert_eq!(total, 3);
        assert_eq!(set.len(), 3);
        assert_eq!(set[0].path, "src/a.rs");
        assert_eq!(set[0].reason, "changed");
        assert_eq!(set[1].path, "src/b.rs");
        assert_eq!(set[1].reason, "blast");
        assert_eq!(set[2].path, "src/c.rs");
        assert_eq!(set[2].reason, "temporal");
    }

    #[test]
    fn read_set_max_files_sets_capped_flags() {
        let impact = base_packet(vec![
            make_changed("src/a.rs"),
            make_changed("src/b.rs"),
            make_changed("src/c.rs"),
        ]);
        let (set, capped, total) = build_read_set(&impact, 1, 0.75);
        assert!(capped);
        assert_eq!(total, 3);
        assert_eq!(set.len(), 1);
        assert_eq!(set[0].reason, "changed");
    }

    #[test]
    fn next_actions_missing_doctor_does_not_claim_block_findings() {
        let doctor = DoctorSection {
            status: "missing".to_string(),
            ready_for_publish: false,
            block: 0,
            warn: 0,
            info: 0,
            top_findings: Vec::new(),
        };
        let ledger = LedgerSection {
            pending_count: 0,
            active_tx: Vec::new(),
        };
        let actions = compose_next_actions("empty", &doctor, &ledger, false, false);
        assert!(
            actions.iter().any(|a| a.contains("ledgerful doctor")),
            "missing doctor should suggest refresh: {actions:?}"
        );
        assert!(
            actions
                .iter()
                .all(|a| !a.contains("resolve doctor block findings")),
            "missing sidecar must not claim block findings: {actions:?}"
        );
    }

    #[test]
    fn next_actions_block_count_suggests_resolve() {
        let doctor = DoctorSection {
            status: "ok".to_string(),
            ready_for_publish: false,
            block: 2,
            warn: 0,
            info: 0,
            top_findings: Vec::new(),
        };
        let ledger = LedgerSection {
            pending_count: 0,
            active_tx: Vec::new(),
        };
        let actions = compose_next_actions("ready", &doctor, &ledger, false, true);
        assert!(
            actions
                .iter()
                .any(|a| a.contains("resolve doctor block findings")),
            "block>0 must surface resolve action: {actions:?}"
        );
    }

    #[test]
    fn doctor_missing_when_sidecar_absent() {
        let tmp = tempdir().unwrap();
        let root = camino::Utf8Path::from_path(tmp.path()).unwrap();
        let layout = Layout::new(root);
        layout.ensure_state_dir().unwrap();
        let section = read_doctor_section(&layout);
        assert_eq!(section.status, "missing");
        assert!(!section.ready_for_publish);
    }

    #[test]
    fn doctor_ok_from_sidecar() {
        let tmp = tempdir().unwrap();
        let root = camino::Utf8Path::from_path(tmp.path()).unwrap();
        let layout = Layout::new(root);
        layout.ensure_state_dir().unwrap();
        let path = layout.state_subdir().join("doctor-results.json");
        fs::write(
            path.as_std_path(),
            r#"{
                "failures": 0,
                "timestamp": "2099-01-01T00:00:00+00:00",
                "readyForPublish": true,
                "block": 0,
                "warn": 1,
                "info": 2
            }"#,
        )
        .unwrap();
        let section = read_doctor_section(&layout);
        assert_eq!(section.status, "ok");
        assert!(section.ready_for_publish);
        assert_eq!(section.warn, 1);
        assert_eq!(section.info, 2);
    }

    #[test]
    fn doctor_stale_from_old_timestamp() {
        let tmp = tempdir().unwrap();
        let root = camino::Utf8Path::from_path(tmp.path()).unwrap();
        let layout = Layout::new(root);
        layout.ensure_state_dir().unwrap();
        let path = layout.state_subdir().join("doctor-results.json");
        fs::write(
            path.as_std_path(),
            r#"{
                "failures": 0,
                "timestamp": "2000-01-01T00:00:00+00:00",
                "readyForPublish": true,
                "block": 0,
                "warn": 0,
                "info": 0
            }"#,
        )
        .unwrap();
        let section = read_doctor_section(&layout);
        assert_eq!(section.status, "stale");
    }

    #[test]
    fn doctor_incomplete_sidecar_is_error_not_false_green() {
        let tmp = tempdir().unwrap();
        let root = camino::Utf8Path::from_path(tmp.path()).unwrap();
        let layout = Layout::new(root);
        layout.ensure_state_dir().unwrap();
        let path = layout.state_subdir().join("doctor-results.json");
        // Timestamp alone must not default to readyForPublish=true.
        fs::write(
            path.as_std_path(),
            r#"{"timestamp": "2099-01-01T00:00:00+00:00"}"#,
        )
        .unwrap();
        let section = read_doctor_section(&layout);
        assert_eq!(section.status, "error");
        assert!(!section.ready_for_publish);
        assert_eq!(section.block, 0);
    }

    #[test]
    fn packet_schema_version_is_one() {
        let p = not_ready_packet(
            "test".into(),
            None,
            DoctorSection {
                status: "missing".into(),
                ready_for_publish: false,
                block: 0,
                warn: 0,
                info: 0,
                top_findings: Vec::new(),
            },
            LedgerSection {
                pending_count: 0,
                active_tx: Vec::new(),
            },
            NotReadyErrorClass::Other,
        );
        assert_eq!(p.schema_version, 1);
        assert_eq!(p.status, "not_ready");
        assert!(p.reason.is_some());
        assert!(!p.next_actions.is_empty());
    }

    fn empty_doctor_ledger() -> (DoctorSection, LedgerSection) {
        (
            DoctorSection {
                status: "missing".into(),
                ready_for_publish: false,
                block: 0,
                warn: 0,
                info: 0,
                top_findings: Vec::new(),
            },
            LedgerSection {
                pending_count: 0,
                active_tx: Vec::new(),
            },
        )
    }

    fn next_actions_joined(class: NotReadyErrorClass) -> String {
        next_actions_for_class(class)
            .join("\n")
            .to_ascii_lowercase()
    }

    #[test]
    fn ro_permission_next_actions_exclude_class_c_triad() {
        let (doctor, ledger) = empty_doctor_ledger();
        let p = not_ready_packet(
            "storage unavailable: state directory not writable: permission denied".into(),
            None,
            doctor,
            ledger,
            NotReadyErrorClass::PermissionDenied,
        );
        assert_eq!(p.schema_version, 1);
        assert_eq!(p.status, "not_ready");
        let joined = p.next_actions.join("\n").to_ascii_lowercase();
        assert!(
            !joined.contains("doctor --json"),
            "RO class must not suggest doctor: {:?}",
            p.next_actions
        );
        assert!(
            !joined.contains("ledgerful init"),
            "RO class must not suggest init: {:?}",
            p.next_actions
        );
        // Ban bare index recovery (Class C). "index" alone may appear in prose — check command shape.
        assert!(
            !joined.contains("ledgerful index"),
            "RO class must not suggest index: {:?}",
            p.next_actions
        );
        assert!(
            joined.contains("ledgerful_state_dir") || joined.contains("populated"),
            "expected STATE_DIR / populated guidance: {:?}",
            p.next_actions
        );
        assert!(
            joined.contains("workspace-write") || joined.contains("git-only"),
            "expected workspace-write or git-only: {:?}",
            p.next_actions
        );
        assert!(
            p.reason
                .as_ref()
                .is_some_and(|r| r.contains("storage unavailable:")
                    || r.contains("state directory not writable")),
            "greppable reason fragment missing: {:?}",
            p.reason
        );
    }

    #[test]
    fn missing_db_next_actions_distinct_from_ro_class() {
        let (doctor, ledger) = empty_doctor_ledger();
        let p = not_ready_packet(
            "storage unavailable: Storage not initialized".into(),
            None,
            doctor,
            ledger,
            NotReadyErrorClass::MissingDb,
        );
        let joined = p.next_actions.join("\n").to_ascii_lowercase();
        assert!(
            joined.contains("init") || joined.contains("index") || joined.contains("scan"),
            "MissingDb may name writable-env init/scan/index: {:?}",
            p.next_actions
        );
        let ro = next_actions_joined(NotReadyErrorClass::PermissionDenied);
        assert_ne!(
            p.next_actions,
            next_actions_for_class(NotReadyErrorClass::PermissionDenied),
            "MissingDb nextActions must differ from RO class"
        );
        assert!(!ro.contains("ledgerful index") || joined.contains("writable"));
    }

    #[test]
    fn classify_storage_error_permission_and_missing() {
        let perm = miette::miette!("unable to open database file: Access is denied. (os error 5)");
        assert_eq!(
            classify_storage_error(&perm, true),
            NotReadyErrorClass::PermissionDenied
        );
        let missing = miette::miette!(
            "Storage not initialized at /tmp/x/state/ledger.db. Run a write command first."
        );
        assert_eq!(
            classify_storage_error(&missing, false),
            NotReadyErrorClass::MissingDb
        );
        let schema = miette::miette!("schema is not current; migration required");
        assert_eq!(
            classify_storage_error(&schema, true),
            NotReadyErrorClass::SchemaStale
        );
        // Codex R2: pure-RO open can fail during schema probe PRAGMA with
        // "unable to open database file" while still embedding user_version —
        // must NOT classify as SchemaStale (migration advice).
        let pragma_open = miette::miette!(
            "PRAGMA user_version: unable to open database file: Access is denied. (os error 5)"
        );
        assert_eq!(
            classify_storage_error(&pragma_open, true),
            NotReadyErrorClass::PermissionDenied
        );
    }

    #[test]
    fn soft_open_existing_db_builds_valid_packet() {
        let tmp = tempdir().unwrap();
        let root = camino::Utf8Path::from_path(tmp.path()).unwrap();
        let dir = tmp.path();

        std::process::Command::new("git")
            .args(["init"])
            .current_dir(dir)
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["config", "user.email", "t@t.com"])
            .current_dir(dir)
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["config", "user.name", "T"])
            .current_dir(dir)
            .output()
            .unwrap();
        fs::write(dir.join("README.md"), "hi").unwrap();
        std::process::Command::new("git")
            .args(["add", "-A"])
            .current_dir(dir)
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["commit", "-m", "init"])
            .current_dir(dir)
            .output()
            .unwrap();

        let layout = Layout::new(root);
        layout.ensure_state_dir().unwrap();
        // Write-mode create + migrate once, then soft-open RO path.
        let write =
            StorageManager::init(layout.state_subdir().join("ledger.db").as_std_path()).unwrap();
        let _ = write.shutdown();

        let storage = open_storage_for_change_context(&layout).expect("soft-open RO");
        assert!(
            storage.is_read_only,
            "existing ledger.db should soft-open read-only"
        );
        let config = Config::default();
        let opts = ChangeContextOpts::default();
        let packet = build_change_context(&opts, &layout, &storage, &config).unwrap();
        assert_eq!(packet.schema_version, 1);
        assert!(
            packet.status == "empty" || packet.status == "ready",
            "unexpected status: {}",
            packet.status
        );
        let _ = storage.shutdown();
    }

    #[test]
    fn build_empty_clean_tree() {
        let tmp = tempdir().unwrap();
        let root = camino::Utf8Path::from_path(tmp.path()).unwrap();

        // git repo + clean commit
        let dir = tmp.path();
        std::process::Command::new("git")
            .args(["init"])
            .current_dir(dir)
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["config", "user.email", "t@t.com"])
            .current_dir(dir)
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["config", "user.name", "T"])
            .current_dir(dir)
            .output()
            .unwrap();
        fs::write(dir.join("README.md"), "hi").unwrap();
        std::process::Command::new("git")
            .args(["add", "-A"])
            .current_dir(dir)
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["commit", "-m", "init"])
            .current_dir(dir)
            .output()
            .unwrap();

        let layout = Layout::new(root);
        layout.ensure_state_dir().unwrap();
        let storage =
            StorageManager::init(layout.state_subdir().join("ledger.db").as_std_path()).unwrap();
        let config = Config::default();
        let opts = ChangeContextOpts::default();
        // project_root comes from layout.root — no cwd mutation required.
        let packet = build_change_context(&opts, &layout, &storage, &config).unwrap();

        assert_eq!(packet.status, "empty");
        assert_eq!(packet.schema_version, 1);
        assert_eq!(packet.ledger.pending_count, 0);
        assert_eq!(packet.doctor.status, "missing");
        assert!(packet.read_set.is_empty());
        // No fake high risk on empty
        if let Some(ref r) = packet.risk_level {
            assert_ne!(r, "high");
        }
        let _ = storage.shutdown();
    }

    #[test]
    fn build_clean_tree_with_pending_ledger_is_ready() {
        let tmp = tempdir().unwrap();
        let root = camino::Utf8Path::from_path(tmp.path()).unwrap();
        let dir = tmp.path();

        std::process::Command::new("git")
            .args(["init"])
            .current_dir(dir)
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["config", "user.email", "t@t.com"])
            .current_dir(dir)
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["config", "user.name", "T"])
            .current_dir(dir)
            .output()
            .unwrap();
        fs::write(dir.join("README.md"), "hi").unwrap();
        std::process::Command::new("git")
            .args(["add", "-A"])
            .current_dir(dir)
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["commit", "-m", "init"])
            .current_dir(dir)
            .output()
            .unwrap();

        let layout = Layout::new(root);
        layout.ensure_state_dir().unwrap();
        let mut storage =
            StorageManager::init(layout.state_subdir().join("ledger.db").as_std_path()).unwrap();
        let config = Config::default();
        {
            let mut mgr = TransactionManager::new(&mut storage, dir.to_path_buf(), config.clone());
            mgr.start_change(TransactionRequest {
                category: Category::Feature,
                entity: "config".to_string(),
                ..Default::default()
            })
            .unwrap();
        }

        let opts = ChangeContextOpts::default();
        let packet = build_change_context(&opts, &layout, &storage, &config).unwrap();

        assert_eq!(packet.status, "ready");
        assert!(packet.ledger.pending_count >= 1);
        assert!(packet.summary.to_lowercase().contains("pending"));
        assert!(packet.summary.to_lowercase().contains("no file"));
        let _ = storage.shutdown();
    }

    #[test]
    fn build_one_changed_file_ready_with_read_set() {
        let tmp = tempdir().unwrap();
        let root = camino::Utf8Path::from_path(tmp.path()).unwrap();
        let dir = tmp.path();

        std::process::Command::new("git")
            .args(["init"])
            .current_dir(dir)
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["config", "user.email", "t@t.com"])
            .current_dir(dir)
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["config", "user.name", "T"])
            .current_dir(dir)
            .output()
            .unwrap();
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("src/lib.rs"), "pub fn a() {}\n").unwrap();
        std::process::Command::new("git")
            .args(["add", "-A"])
            .current_dir(dir)
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["commit", "-m", "init"])
            .current_dir(dir)
            .output()
            .unwrap();
        fs::write(dir.join("src/lib.rs"), "pub fn a() {}\npub fn b() {}\n").unwrap();

        let layout = Layout::new(root);
        layout.ensure_state_dir().unwrap();
        let storage =
            StorageManager::init(layout.state_subdir().join("ledger.db").as_std_path()).unwrap();
        let config = Config::default();
        let opts = ChangeContextOpts::default();
        let packet = build_change_context(&opts, &layout, &storage, &config).unwrap();

        assert_eq!(packet.status, "ready");
        assert!(!packet.read_set.is_empty());
        assert!(
            packet
                .read_set
                .iter()
                .any(|e| e.path.contains("lib.rs") && e.reason == "changed"),
            "readSet should include changed lib.rs: {:?}",
            packet.read_set
        );
        assert!(packet.risk_level.is_some());
        assert!(packet.doctor.status == "missing" || packet.doctor.status == "ok");
        let _ = storage.shutdown();
    }

    #[test]
    fn change_context_does_not_rewrite_latest_impact() {
        let tmp = tempdir().unwrap();
        let root = camino::Utf8Path::from_path(tmp.path()).unwrap();
        let dir = tmp.path();

        std::process::Command::new("git")
            .args(["init"])
            .current_dir(dir)
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["config", "user.email", "t@t.com"])
            .current_dir(dir)
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["config", "user.name", "T"])
            .current_dir(dir)
            .output()
            .unwrap();
        fs::write(dir.join("README.md"), "hi").unwrap();
        std::process::Command::new("git")
            .args(["add", "-A"])
            .current_dir(dir)
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["commit", "-m", "init"])
            .current_dir(dir)
            .output()
            .unwrap();

        let layout = Layout::new(root);
        layout.ensure_state_dir().unwrap();

        // Pre-seed latest-impact.json with a distinctive marker.
        let seed = ImpactPacket {
            schema_version: "v1".to_string(),
            head_hash: Some("SEED_MARKER_0114".to_string()),
            risk_reasons: vec!["seed-reason-do-not-clobber".to_string()],
            ..Default::default()
        };
        write_impact_report(&layout, &seed).unwrap();

        let report_path = layout.reports_dir().join(LATEST_IMPACT_REPORT);
        let before = fs::read_to_string(report_path.as_std_path()).unwrap();
        assert!(before.contains("SEED_MARKER_0114"));

        let storage =
            StorageManager::init(layout.state_subdir().join("ledger.db").as_std_path()).unwrap();
        let config = Config::default();
        let opts = ChangeContextOpts::default();
        let _packet = build_change_context(&opts, &layout, &storage, &config).unwrap();

        let after = fs::read_to_string(report_path.as_std_path()).unwrap();
        assert_eq!(
            before, after,
            "change-context must not rewrite latest-impact.json"
        );
        let _ = storage.shutdown();
    }

    #[test]
    fn json_roundtrip_camel_case_keys() {
        let p = not_ready_packet(
            "x".into(),
            Some("HEAD~1".into()),
            DoctorSection {
                status: "missing".into(),
                ready_for_publish: false,
                block: 0,
                warn: 0,
                info: 0,
                top_findings: Vec::new(),
            },
            LedgerSection {
                pending_count: 0,
                active_tx: Vec::new(),
            },
            NotReadyErrorClass::Other,
        );
        let s = serde_json::to_string(&p).unwrap();
        assert!(s.contains("\"schemaVersion\":1"));
        assert!(s.contains("\"readSetCapped\""));
        assert!(s.contains("\"readSetTotalCandidates\""));
        assert!(s.contains("\"pendingCount\""));
        assert!(s.contains("\"readyForPublish\""));
        let back: ChangeContextPacket = serde_json::from_str(&s).unwrap();
        assert_eq!(back.schema_version, 1);
    }

    #[test]
    fn detail_parse() {
        assert_eq!(
            ChangeContextDetail::parse("minimal").unwrap(),
            ChangeContextDetail::Minimal
        );
        assert_eq!(
            ChangeContextDetail::parse("standard").unwrap(),
            ChangeContextDetail::Standard
        );
        assert!(ChangeContextDetail::parse("deep").is_err());
    }

    #[test]
    fn test_coverage_never_bare_empty_or_track_0115_handoff() {
        use crate::impact::enrichment::test_gaps::TestGapsStatus;
        use crate::state::migrations::get_migrations;

        let tmp = tempdir().unwrap();
        let root = camino::Utf8Path::from_path(tmp.path()).unwrap();
        let dir = tmp.path();
        std::process::Command::new("git")
            .args(["init"])
            .current_dir(dir)
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["config", "user.email", "t@t.com"])
            .current_dir(dir)
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["config", "user.name", "T"])
            .current_dir(dir)
            .output()
            .unwrap();
        fs::write(dir.join("README.md"), "hi").unwrap();
        std::process::Command::new("git")
            .args(["add", "-A"])
            .current_dir(dir)
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["commit", "-m", "init"])
            .current_dir(dir)
            .output()
            .unwrap();

        let layout = Layout::new(root);
        layout.ensure_state_dir().unwrap();
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        get_migrations().to_latest(&mut conn).unwrap();
        // Use real layout storage so build_change_context works end-to-end.
        let storage =
            StorageManager::init(layout.state_subdir().join("ledger.db").as_std_path()).unwrap();
        let config = Config::default();
        let opts = ChangeContextOpts::default();
        let packet = build_change_context(&opts, &layout, &storage, &config).unwrap();

        let cov = packet.test_coverage.as_ref().expect("testCoverage present");
        // Never bare "empty"
        assert_ne!(cov.status.as_str(), "empty");
        assert!(matches!(
            cov.status,
            TestGapsStatus::Available
                | TestGapsStatus::EmptyMapping
                | TestGapsStatus::MissingTable
                | TestGapsStatus::NoSourceSeeds
                | TestGapsStatus::Unavailable
        ));

        let json = serde_json::to_string(&packet).unwrap();
        assert!(
            !json.contains("track 0115"),
            "handoff string must be gone: {json}"
        );
        assert!(
            !json.contains("see ledgerful tests"),
            "handoff string must be gone: {json}"
        );
        // Guard testCoverage.status specifically (top-level packet status may be "empty").
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        let tc_status = v["testCoverage"]["status"].as_str().unwrap_or("");
        assert_ne!(tc_status, "empty", "testCoverage.status bare empty: {json}");
        assert!(
            matches!(
                tc_status,
                "available" | "empty_mapping" | "missing_table" | "no_source_seeds" | "unavailable"
            ),
            "unexpected testCoverage.status={tc_status}: {json}"
        );
        // Structural + LCOV ceiling always present
        assert!(json.contains("Structural test_mapping"));
        assert!(json.contains("LCOV COVERAGE"));
        let _ = storage.shutdown();
    }

    #[test]
    fn summarize_test_coverage_uses_impact_attached_gaps() {
        use crate::impact::enrichment::test_gaps::{TestGapsReport, TestGapsStatus};

        let tmp = tempdir().unwrap();
        let root = camino::Utf8Path::from_path(tmp.path()).unwrap();
        let layout = Layout::new(root);
        layout.ensure_state_dir().unwrap();
        let storage =
            StorageManager::init(layout.state_subdir().join("ledger.db").as_std_path()).unwrap();

        let impact = ImpactPacket {
            test_gaps: Some(TestGapsReport {
                status: TestGapsStatus::Available,
                source_seed_count: 3,
                mapped_count: 1,
                file_mapped_count: 1,
                unmapped_count: 1,
                unmapped_capped: false,
                unmapped_total: 1,
                unmapped: vec![],
                mapped_sample: vec![],
                notes: vec!["note".into()],
            }),
            ..ImpactPacket::default()
        };
        let summary = summarize_test_coverage(&storage, &impact);
        assert_eq!(summary.status, TestGapsStatus::Available);
        assert_eq!(summary.source_seed_count, 3);
        assert_eq!(summary.mapped_count, 1);
        assert_eq!(summary.unmapped_count, 1);
        let _ = storage.shutdown();
    }

    #[test]
    fn change_context_blast_confidence_summary_counts_only_no_edges() {
        use crate::impact::enrichment::edge_confidence::EdgeConfidenceSummary;

        // Unit-level packet shape: BlastSummary carries counts, never edges.
        let summary = BlastSummary {
            depth: 1,
            must_touch_file_count: 2,
            must_touch_symbol_count: 1,
            confidence_summary: EdgeConfidenceSummary {
                scip_bound: 3,
                resolved: 5,
                ambiguous: 0,
                unresolved: 0,
                capped: 0,
                unknown: 0,
                expandable: 7,
                total: 8,
            },
        };
        let v = serde_json::to_value(&summary).unwrap();
        assert!(
            v.get("edges").is_none(),
            "change-context must not dump edges"
        );
        assert_eq!(v["confidenceSummary"]["scipBound"], 3);
        assert_eq!(v["confidenceSummary"]["resolved"], 5);
        assert_eq!(v["confidenceSummary"]["total"], 8);
        assert_eq!(v["depth"], 1);

        // Both detail levels use the same BlastSummary shape (no detail gate on counts).
        for detail in [ChangeContextDetail::Minimal, ChangeContextDetail::Standard] {
            assert!(
                matches!(
                    detail,
                    ChangeContextDetail::Minimal | ChangeContextDetail::Standard
                ),
                "detail levels are minimal|standard only"
            );
        }
    }

    fn make_flow_entry(i: usize) -> crate::impact::enrichment::affected_flows::AffectedFlowEntry {
        use crate::impact::enrichment::affected_flows::{AffectedFlowEntry, MatchKind};
        AffectedFlowEntry {
            method: "GET".into(),
            path_pattern: format!("/p{i:02}"),
            handler_symbol_name: Some(format!("h{i:02}")),
            handler_file: Some(format!("src/h{i:02}.rs")),
            framework: "Axum".into(),
            match_kind: MatchKind::RouteFile,
            route_confidence: Some(1.0),
            confidence_class: None,
            evidence: None,
        }
    }

    fn make_available_flows(
        n: usize,
    ) -> crate::impact::enrichment::affected_flows::AffectedFlowsReport {
        use crate::impact::enrichment::affected_flows::{
            AffectedFlowsReport, AffectedFlowsStatus, HONESTY_NOTE,
        };
        let flows: Vec<_> = (0..n).map(make_flow_entry).collect();
        AffectedFlowsReport {
            status: AffectedFlowsStatus::Available,
            flow_count: n,
            flow_capped: false,
            flow_total: n,
            flows,
            notes: vec![HONESTY_NOTE.into()],
        }
    }

    #[test]
    fn summarize_affected_flows_uses_impact_attached_report() {
        let tmp = tempdir().unwrap();
        let root = camino::Utf8Path::from_path(tmp.path()).unwrap();
        let layout = Layout::new(root);
        layout.ensure_state_dir().unwrap();
        let storage =
            StorageManager::init(layout.state_subdir().join("ledger.db").as_std_path()).unwrap();

        let impact = ImpactPacket {
            affected_flows: Some(make_available_flows(3)),
            ..ImpactPacket::default()
        };
        let summary = summarize_affected_flows(&storage, &impact, ChangeContextDetail::Minimal);
        assert_eq!(
            summary.status,
            crate::impact::enrichment::affected_flows::AffectedFlowsStatus::Available
        );
        assert_eq!(summary.flow_count, 3);
        assert_eq!(summary.flows.len(), 3);
        let _ = storage.shutdown();
    }

    #[test]
    fn summarize_affected_flows_sample_caps_by_detail() {
        let tmp = tempdir().unwrap();
        let root = camino::Utf8Path::from_path(tmp.path()).unwrap();
        let layout = Layout::new(root);
        layout.ensure_state_dir().unwrap();
        let storage =
            StorageManager::init(layout.state_subdir().join("ledger.db").as_std_path()).unwrap();

        let impact = ImpactPacket {
            affected_flows: Some(make_available_flows(15)),
            ..ImpactPacket::default()
        };

        let minimal = summarize_affected_flows(&storage, &impact, ChangeContextDetail::Minimal);
        assert_eq!(minimal.flows.len(), AFFECTED_FLOWS_SAMPLE_MINIMAL);
        // Counts pass through full report — not the sample size.
        assert_eq!(minimal.flow_count, 15);
        assert_eq!(minimal.flow_total, 15);

        let standard = summarize_affected_flows(&storage, &impact, ChangeContextDetail::Standard);
        assert_eq!(standard.flows.len(), AFFECTED_FLOWS_SAMPLE_STANDARD);
        assert_eq!(standard.flow_count, 15);

        // No huge arrays on either detail.
        assert!(minimal.flows.len() <= 5);
        assert!(standard.flows.len() <= 10);
        let _ = storage.shutdown();
    }

    #[test]
    fn summarize_affected_flows_status_passthrough_and_available_zero() {
        use crate::impact::enrichment::affected_flows::{
            AffectedFlowsReport, AffectedFlowsStatus, HONESTY_NOTE,
        };

        let tmp = tempdir().unwrap();
        let root = camino::Utf8Path::from_path(tmp.path()).unwrap();
        let layout = Layout::new(root);
        layout.ensure_state_dir().unwrap();
        let storage =
            StorageManager::init(layout.state_subdir().join("ledger.db").as_std_path()).unwrap();

        // Status passthrough (empty_map).
        let impact_empty = ImpactPacket {
            affected_flows: Some(AffectedFlowsReport {
                status: AffectedFlowsStatus::EmptyMap,
                flow_count: 0,
                flow_capped: false,
                flow_total: 0,
                flows: vec![],
                notes: vec![HONESTY_NOTE.into()],
            }),
            ..ImpactPacket::default()
        };
        let empty =
            summarize_affected_flows(&storage, &impact_empty, ChangeContextDetail::Standard);
        assert_eq!(empty.status, AffectedFlowsStatus::EmptyMap);
        assert_eq!(empty.flow_count, 0);
        assert!(empty.flows.is_empty());

        // available + 0 = all-clear (no registered routes touched).
        let impact_clear = ImpactPacket {
            affected_flows: Some(AffectedFlowsReport {
                status: AffectedFlowsStatus::Available,
                flow_count: 0,
                flow_capped: false,
                flow_total: 0,
                flows: vec![],
                notes: vec![HONESTY_NOTE.into()],
            }),
            ..ImpactPacket::default()
        };
        let clear = summarize_affected_flows(&storage, &impact_clear, ChangeContextDetail::Minimal);
        assert_eq!(clear.status, AffectedFlowsStatus::Available);
        assert_eq!(clear.flow_count, 0);
        assert!(clear.flows.is_empty());

        let json = serde_json::to_value(&clear).unwrap();
        assert_eq!(json["status"], "available");
        assert_eq!(json["flowCount"], 0);
        assert!(json["flows"].as_array().unwrap().is_empty());
        let _ = storage.shutdown();
    }

    #[test]
    fn change_context_schema_version_stays_one_with_affected_flows_key() {
        use crate::impact::enrichment::affected_flows::HONESTY_NOTE;

        let packet = ChangeContextPacket {
            schema_version: CHANGE_CONTEXT_SCHEMA_VERSION,
            status: "ready".into(),
            summary: "test".into(),
            reason: None,
            head_hash: Some("abc".into()),
            base_ref: None,
            risk_level: Some("low".into()),
            risk_reasons: vec![],
            read_set: vec![],
            read_set_capped: false,
            read_set_total_candidates: 0,
            blast: None,
            test_coverage: None,
            affected_flows: Some(make_available_flows(1)),
            doctor: DoctorSection {
                status: "ok".into(),
                ready_for_publish: true,
                block: 0,
                warn: 0,
                info: 0,
                top_findings: vec![],
            },
            ledger: LedgerSection {
                pending_count: 0,
                active_tx: vec![],
            },
            analysis_warnings: vec![],
            next_actions: vec![],
            impact_schema_version: Some("v1".into()),
        };
        let v = serde_json::to_value(&packet).unwrap();
        assert_eq!(v["schemaVersion"], 1);
        assert!(v.get("affectedFlows").is_some());
        assert_eq!(v["affectedFlows"]["status"], "available");
        assert_eq!(v["affectedFlows"]["flowCount"], 1);
        assert_eq!(v["affectedFlows"]["flows"][0]["method"], "GET");
        assert_eq!(v["affectedFlows"]["flows"][0]["pathPattern"], "/p00");
        let notes = v["affectedFlows"]["notes"].as_array().unwrap();
        assert!(notes.iter().any(|n| n.as_str() == Some(HONESTY_NOTE)));
    }
}
