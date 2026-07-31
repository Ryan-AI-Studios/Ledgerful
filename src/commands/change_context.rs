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
}

/// Deepened test-coverage / gap summary (0115). Re-exports the shared library
/// report so change-context, impact, and scan --pr share one schema.
pub type TestCoverageSummary = crate::impact::enrichment::test_gaps::TestGapsReport;

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
    });

    let test_coverage = Some(summarize_test_coverage(storage, &impact));

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
            );
            return emit_packet(&packet, json);
        }
    };

    let config = crate::config::load::load_config(&layout).unwrap_or_default();
    let storage = match StorageManager::init_with_layout(&layout) {
        Ok(s) => s,
        Err(e) => {
            let packet = not_ready_packet(
                format!("storage unavailable: {e}"),
                opts.base_ref.clone(),
                read_doctor_section(&layout),
                LedgerSection {
                    pending_count: 0,
                    active_tx: Vec::new(),
                },
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
        doctor,
        ledger,
        analysis_warnings: Vec::new(),
        next_actions: vec![
            "ledgerful doctor --json".to_string(),
            "ledgerful init".to_string(),
            "ledgerful index --incremental".to_string(),
        ],
        impact_schema_version: None,
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
pub fn build_change_context_from_cwd(opts: &ChangeContextOpts) -> Result<ChangeContextPacket> {
    let layout = crate::commands::helpers::get_layout()?;
    let config = crate::config::load::load_config(&layout).unwrap_or_default();
    let storage = StorageManager::init_with_layout(&layout)?;
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
        );
        assert_eq!(p.schema_version, 1);
        assert_eq!(p.status, "not_ready");
        assert!(p.reason.is_some());
        assert!(!p.next_actions.is_empty());
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
}
