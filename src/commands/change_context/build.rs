//! Build change-context packets (in-memory impact; no latest-impact rewrite).

use super::packet::*;
use super::storage::{open_storage_for_change_context, storage_unavailable_reason};
use crate::config::model::Config;
use crate::git::RepoSnapshot;
use crate::git::repo::{get_head_info, open_repo};
use crate::impact::packet::{ImpactPacket, RiskLevel};
use crate::state::layout::Layout;
use crate::state::storage::StorageManager;
use chrono::{DateTime, Duration, Utc};
use miette::Result;
use std::collections::BTreeSet;
use std::path::Path;

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
    // Usage error: hard-fail (do not fold into not_ready).
    if !opts.paths.is_empty() && opts.base_ref.is_some() {
        return Err(miette::miette!(
            "--paths and --base-ref are mutually exclusive"
        ));
    }

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

    let path_mode =
        crate::impact::path_class::path_mode_from_include_governance(opts.include_governance);
    let (read_set, read_set_capped, read_set_total_candidates) = build_read_set(
        &impact,
        opts.max_files,
        config.temporal.coupling_threshold,
        path_mode,
    );

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

    let change_hints = if has_file_changes {
        Some(compute_change_hints_for_impact(&impact, project_root))
    } else {
        None
    };

    let summary = compose_summary(
        status,
        has_file_changes,
        &impact,
        &ledger,
        &doctor,
        change_hints.as_ref(),
    );
    let agent_summary = Some(build_agent_summary(
        &impact,
        change_hints.as_ref(),
        risk_level_str(impact.risk_level),
    ));
    let risk_reasons = trim_reasons(&impact.risk_reasons, opts.detail);
    let mut warnings = trim_reasons(&analysis_warnings, opts.detail);
    warnings.sort();
    warnings.dedup();

    let next_actions = compose_next_actions(
        status,
        &doctor,
        &ledger,
        read_set_capped,
        has_file_changes,
        change_hints.as_ref(),
    );

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
        agent_summary,
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
        change_hints,
        doctor,
        ledger,
        analysis_warnings: warnings,
        next_actions,
        impact_schema_version: Some(impact.schema_version.clone()),
    })
}

pub(crate) fn compute_structural_impact(
    opts: &ChangeContextOpts,
    storage: &StorageManager,
    config: &Config,
    project_root: &Path,
) -> Result<ImpactPacket> {
    if !opts.paths.is_empty() && opts.base_ref.is_some() {
        return Err(miette::miette!(
            "--paths and --base-ref are mutually exclusive"
        ));
    }

    if !opts.paths.is_empty() {
        let parsed = crate::commands::impact::parse_prospective_paths(&opts.paths)?;
        let snapshot = crate::commands::impact::build_prospective_snapshot(project_root, &parsed)?;
        return crate::commands::impact::compute_impact_from_snapshot_in_memory_with_mode(
            storage,
            config,
            project_root,
            snapshot,
            opts.include_governance,
            "prospective",
            parsed,
        );
    }

    if let Some(ref base_ref) = opts.base_ref {
        let snapshot = build_repo_snapshot_from_base_ref(project_root, base_ref, config)?;
        return crate::commands::impact::compute_impact_from_snapshot_in_memory_with_mode(
            storage,
            config,
            project_root,
            snapshot,
            opts.include_governance,
            "base_ref",
            Vec::new(),
        );
    }

    // Working tree — always with_mode so pathMode is set before temporal demotion.
    let repo = open_repo(project_root)?;
    let (head_hash, branch_name) = get_head_info(&repo)?;
    let all_changes = crate::git::status::get_repo_status(&repo)?;
    let changes = crate::git::ignore::filter_ignored_changes(
        all_changes,
        &config.watch.ignore_patterns,
        true,
    )?;
    let is_clean = changes.is_empty();
    let snapshot = RepoSnapshot {
        head_hash,
        branch_name,
        is_clean,
        changes,
    };
    crate::commands::impact::compute_impact_from_snapshot_in_memory_with_mode(
        storage,
        config,
        project_root,
        snapshot,
        opts.include_governance,
        "working_tree",
        Vec::new(),
    )
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

pub(crate) fn not_ready_packet(
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
        agent_summary: None,
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
        change_hints: None,
        doctor,
        ledger,
        analysis_warnings: Vec::new(),
        next_actions: next_actions_for_class(class),
        impact_schema_version: None,
    }
}

/// Build structured agentSummary from impact + optional changeHints (0173).
pub(crate) fn build_agent_summary(
    impact: &ImpactPacket,
    change_hints: Option<&crate::impact::enrichment::change_hints::ChangeHintsReport>,
    risk: &str,
) -> AgentSummary {
    use crate::impact::path_class::{PathClass, classify_path_buf};

    let mut counts = ChangedClassCounts::default();
    for c in &impact.changes {
        counts.total += 1;
        match classify_path_buf(&c.path) {
            PathClass::Code => counts.code += 1,
            PathClass::Governance => counts.governance += 1,
            PathClass::Contract => counts.contract += 1,
        }
    }

    let mut top_symbols: Vec<String> = Vec::new();
    for c in &impact.changes {
        if let Some(ref syms) = c.symbols {
            for s in syms {
                let name = s.qualified_name.clone().unwrap_or_else(|| s.name.clone());
                if !name.is_empty() {
                    top_symbols.push(name);
                }
            }
        }
    }
    top_symbols.sort();
    top_symbols.dedup();
    top_symbols.truncate(5);

    let mut must_touch_sample: Vec<String> = impact
        .blast_radius
        .as_ref()
        .map(|b| {
            b.must_touch_files
                .iter()
                .map(|p| p.replace('\\', "/"))
                .collect()
        })
        .unwrap_or_default();
    must_touch_sample.sort();
    must_touch_sample.dedup();
    must_touch_sample.truncate(5);

    let mut suggested_tests_sample: Vec<String> = Vec::new();
    if let Some(hints) = change_hints {
        for t in &hints.suggested_tests {
            suggested_tests_sample.push(t.path.replace('\\', "/"));
        }
    }
    suggested_tests_sample.sort();
    suggested_tests_sample.dedup();
    suggested_tests_sample.truncate(3);

    let demoted = impact.demoted_temporal_count;
    let risk_one_liner = if demoted > 0 {
        format!(
            "{risk} — {} code file(s); {demoted} process temporal demoted",
            counts.code
        )
    } else {
        format!("{risk} — {} file(s) changed", counts.total)
    };

    AgentSummary {
        risk_one_liner,
        changed: counts,
        top_symbols,
        must_touch_sample,
        suggested_tests_sample,
        demoted_temporal_count: demoted,
        path_mode: impact.path_mode.clone(),
        analysis_mode: impact.analysis_mode.clone(),
    }
}

/// Class-aware recovery actions (B5). RO/permission must not lead with Class C.
pub(crate) fn next_actions_for_class(class: NotReadyErrorClass) -> Vec<String> {
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

pub(crate) fn build_read_set(
    impact: &ImpactPacket,
    max_files: usize,
    coupling_threshold: f32,
    path_mode: &str,
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

    // Priority 3: temporal coupling partners above threshold (code-mode demotion)
    let mut temporal: Vec<String> = Vec::new();
    for tc in &impact.temporal_couplings {
        if tc.score < coupling_threshold {
            continue;
        }
        let a = normalize_path(&tc.file_a);
        let b = normalize_path(&tc.file_b);
        if crate::impact::path_class::should_demote_pair(&a, &b, path_mode) {
            continue;
        }
        temporal.push(a);
        temporal.push(b);
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

pub(crate) fn normalize_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

pub(crate) fn normalize_path_str(path: &str) -> String {
    path.replace('\\', "/")
}

pub(crate) fn risk_level_str(level: RiskLevel) -> &'static str {
    match level {
        RiskLevel::Low => "low",
        RiskLevel::Medium => "medium",
        RiskLevel::High => "high",
    }
}

pub(crate) fn trim_reasons(reasons: &[String], detail: ChangeContextDetail) -> Vec<String> {
    let limit = match detail {
        ChangeContextDetail::Minimal => 5,
        ChangeContextDetail::Standard => 20,
    };
    reasons.iter().take(limit).cloned().collect()
}

/// Prefer impact-attached gaps (shared orchestrator seeds); else recompute.
pub(crate) fn summarize_test_coverage(
    storage: &StorageManager,
    impact: &ImpactPacket,
) -> TestCoverageSummary {
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
pub(crate) fn summarize_affected_flows(
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

pub(crate) fn compose_summary(
    status: &str,
    has_file_changes: bool,
    impact: &ImpactPacket,
    ledger: &LedgerSection,
    doctor: &DoctorSection,
    change_hints: Option<&crate::impact::enrichment::change_hints::ChangeHintsReport>,
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
            let mut s = format!(
                "{} changed file(s), risk={}, doctor.readyForPublish={}, ledger.pending={}.",
                impact.changes.len(),
                risk,
                doctor.ready_for_publish,
                ledger.pending_count
            );
            if let Some(hints) = change_hints
                && hints.kind
                    == crate::impact::enrichment::change_hints::ChangeHintsKind::Greenfield
            {
                use crate::impact::enrichment::change_hints::format_summary_prefixes;
                let prefixes = format_summary_prefixes(&hints.new_package_prefixes, 3);
                let clause = if prefixes.is_empty() {
                    format!(
                        " greenfield-ish ({} added / {} total).",
                        hints.added_count, hints.total_changed
                    )
                } else {
                    format!(
                        " greenfield-ish ({} added / {} total; prefixes: {prefixes}).",
                        hints.added_count, hints.total_changed
                    )
                };
                s.push_str(&clause);
            }
            s
        }
        other => other.to_string(),
    }
}

pub(crate) fn compose_next_actions(
    status: &str,
    doctor: &DoctorSection,
    ledger: &LedgerSection,
    read_set_capped: bool,
    has_file_changes: bool,
    change_hints: Option<&crate::impact::enrichment::change_hints::ChangeHintsReport>,
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
    if let Some(hints) = change_hints
        && hints.kind == crate::impact::enrichment::change_hints::ChangeHintsKind::Greenfield
        && !hints.suggested_tests.is_empty()
    {
        actions.push(GREENFIELD_SUGGESTED_TESTS_ACTION.to_string());
    }
    if status == "empty" {
        actions.push("no structural work required".to_string());
    }
    actions.sort();
    actions.dedup();
    actions
}

/// Collect mapped hint paths from impact blast `test_hints` (+ optional mappedSample).
pub(crate) fn mapped_paths_from_impact(impact: &ImpactPacket) -> Vec<String> {
    let mut paths: Vec<String> = Vec::new();
    if let Some(ref blast) = impact.blast_radius {
        for hint in &blast.test_hints {
            let path = hint.split("::").next().unwrap_or(hint).trim();
            if !path.is_empty() {
                paths.push(path.replace('\\', "/"));
            }
        }
    }
    if let Some(ref gaps) = impact.test_gaps {
        for sample in &gaps.mapped_sample {
            // mappedSample is source-side; covering paths aren't listed per entry.
            // Keep source file only when it looks like a test path (rare).
            if crate::index::test_mapping::is_test_path(&sample.file) {
                paths.push(sample.file.replace('\\', "/"));
            }
        }
    }
    paths.sort();
    paths.dedup();
    paths
}

pub(crate) fn compute_change_hints_for_impact(
    impact: &ImpactPacket,
    project_root: &Path,
) -> crate::impact::enrichment::change_hints::ChangeHintsReport {
    use crate::impact::enrichment::change_hints::{ChangeHintsOpts, compute_change_hints};
    let opts = ChangeHintsOpts {
        project_root: Some(project_root.to_path_buf()),
        mapped_hint_paths: mapped_paths_from_impact(impact),
    };
    compute_change_hints(&impact.changes, &opts)
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

pub(crate) fn parse_doctor_sidecar(contents: &str, path: &Path) -> DoctorSection {
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
            // Forward remediation only when present; never invent a value.
            let remediation = f
                .get("remediation")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            top_findings.push(DoctorTopFinding {
                code,
                severity,
                message,
                remediation,
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

pub(crate) fn is_doctor_stale(json: &serde_json::Value, path: &Path) -> bool {
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
pub(crate) fn read_ledger_section_with_warnings(
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
