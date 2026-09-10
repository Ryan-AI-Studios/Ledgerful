//! Agent `review` command (0304). Range-correct git + impact join.
//!
//! Does **not** call `build_change_context` / `--base-ref` (those diff
//! `base…HEAD`). Does **not** rewrite `latest-impact.json`.

use crate::commands::scan::{
    compute_pr_scan_affected_flows, compute_pr_scan_test_gaps, files_changed_between,
    parse_pr_range, resolve_commit_oid,
};
use crate::config::model::{Config, ReviewConfig};
use crate::git::repo::{get_head_info, open_repo};
use crate::git::{ChangeType, FileChange, RepoSnapshot};
use crate::impact::enrichment::affected_flows::AffectedFlowsReport;
use crate::impact::enrichment::test_gaps::TestGapsReport;
use crate::ledger::db::LedgerDb;
use crate::state::layout::Layout;
use crate::state::storage::StorageManager;
use crate::verify::results::{VERIFY_HISTORY, parse_verify_history};
use globset::{Glob, GlobSet, GlobSetBuilder};
use miette::Result;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

pub const REVIEW_SCHEMA_VERSION: u32 = 1;
pub const REVIEW_KIND: &str = "review";
pub const REVIEW_ANALYSIS_MODE: &str = "range";

const MAX_REQUIREMENTS_PATHS: usize = 8;
const MAX_REQUIREMENT_ITEMS: usize = 20;
const MAX_REQUIREMENT_TEXT: usize = 2048;
const MAX_FILES_CHANGED: usize = 200;
const MAX_FILES_CLASSIFIED: usize = 100;
const MAX_AFFECTED_SYMBOLS: usize = 5;
const MAX_PRIOR_DECISIONS: usize = 10;
const MAX_CLAIMS: usize = 10;
const MAX_FINDINGS: usize = 20;
const MAX_CI: usize = 10;
const MAX_CONTRACTS: usize = 20;
const MAX_SEARCH_STEMS: usize = 3;

#[derive(Debug, Clone)]
pub struct ReviewOpts {
    pub range: String,
    pub json: bool,
    pub requirements: Vec<String>,
    pub id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ReviewEnvelope {
    pub schema_version: u32,
    pub kind: String,
    pub analysis_mode: String,
    pub range: String,
    pub base_ref: String,
    pub head_ref: String,
    pub files_changed: Vec<ReviewFileChange>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub files_changed_truncated: bool,
    pub blast: ReviewBlast,
    pub affected_symbols: Vec<String>,
    pub tests: ReviewTests,
    pub contracts: ReviewContracts,
    pub promised_requirements: ReviewRequirements,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub requirements_files_truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub files_intended: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub files_unexpected: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub files_intended_truncated: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub files_unexpected_truncated: bool,
    pub prior_decisions: Vec<ReviewDecision>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub prior_decisions_truncated: bool,
    pub implementation_claims: Vec<ReviewClaim>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub implementation_claims_truncated: bool,
    pub unresolved_findings: ReviewFindings,
    pub ci_evidence: ReviewCiEvidence,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub coordinated: Option<ReviewCoordinated>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ReviewFileChange {
    pub path: String,
    pub change_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub old_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ReviewBlast {
    pub status: String,
    pub files_scanned: usize,
    pub files_total: usize,
    pub files_capped: bool,
    pub nodes: usize,
    pub edges: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ReviewTests {
    pub status: String,
    pub exercising: Vec<String>,
    pub untested: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ReviewContracts {
    pub status: String,
    pub items: Vec<serde_json::Value>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ReviewRequirements {
    pub status: String,
    pub items: Vec<ReviewRequirementItem>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ReviewRequirementItem {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub source: String,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ReviewDecision {
    pub tx_id: String,
    pub summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ReviewClaim {
    pub tx_id: String,
    pub summary: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ReviewFindings {
    pub status: String,
    pub items: Vec<ReviewFindingItem>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ReviewFindingItem {
    pub source: String,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub severity: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ReviewCiEvidence {
    pub status: String,
    pub items: Vec<ReviewCiItem>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ReviewCiItem {
    #[serde(rename_all = "camelCase")]
    VerifyHistory {
        timestamp: String,
        passed: bool,
        duration_secs: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        tx_id: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    GithubCheck {
        name: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        conclusion: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        html_url: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ReviewCoordinated {
    pub track_id: String,
    pub spec_path: String,
}

/// CLI entry.
pub fn execute_review(
    range: String,
    json: bool,
    requirements: Vec<String>,
    id: Option<String>,
) -> Result<()> {
    let layout = crate::commands::helpers::get_layout()
        .map_err(|e| miette::miette!("review: layout unavailable: {e}"))?;
    let config = crate::config::load::load_config(&layout).unwrap_or_default();
    let work_dir = layout.root.as_std_path().to_path_buf();
    let opts = ReviewOpts {
        range,
        json,
        requirements,
        id,
    };
    let mut out = std::io::stdout();
    execute_review_in(&layout, &work_dir, &config, &opts, &mut out)
}

/// Testable seam. Never writes `latest-impact.json`.
pub fn execute_review_in(
    layout: &Layout,
    work_dir: &Path,
    config: &Config,
    opts: &ReviewOpts,
    out: &mut impl Write,
) -> Result<()> {
    let review_cfg = &config.review;
    let backend_configured = github_backend_on(review_cfg) || conductor_backend_on(review_cfg);
    if opts.id.is_some() && !backend_configured {
        return Err(miette::miette!("--id requires a configured review backend"));
    }

    let (base_ref, head_ref, git_range) = parse_review_range(&opts.range)?;

    let raw_changes = files_changed_between(work_dir, &git_range, &base_ref)?;
    let files_total = raw_changes.len();
    let filtered = crate::git::ignore::filter_ignored_changes(
        raw_changes,
        &config.watch.ignore_patterns,
        true,
    )?;

    let mut files_changed = file_changes_to_review(&filtered);
    files_changed.sort_by(|a, b| a.path.cmp(&b.path));
    files_changed.dedup_by(|a, b| a.path == b.path && a.change_type == b.change_type);
    let files_changed_truncated = files_changed.len() > MAX_FILES_CHANGED;
    if files_changed_truncated {
        files_changed.truncate(MAX_FILES_CHANGED);
    }

    let (requirements_files_truncated, promised) =
        load_promised_requirements(layout, work_dir, review_cfg, opts)?;

    let classify = !review_cfg.expected_path_globs.is_empty()
        || promised.items.iter().any(|i| i.source != "conductor");
    let (files_intended, files_unexpected, files_intended_truncated, files_unexpected_truncated) =
        if classify {
            classify_paths(&files_changed, review_cfg, &promised.items)
        } else {
            (None, None, false, false)
        };

    let storage = open_review_storage(layout);
    let snapshot = build_range_snapshot(work_dir, filtered, &head_ref);

    let (blast, affected_symbols, tests, contracts) = match &storage {
        Some(storage) => compose_impact(
            layout,
            work_dir,
            config,
            storage,
            &snapshot,
            files_total,
            files_changed_truncated,
        ),
        None => (
            ReviewBlast {
                status: "unavailable".to_string(),
                files_scanned: snapshot.changes.len(),
                files_total,
                files_capped: files_changed_truncated,
                nodes: 0,
                edges: 0,
                reason: Some("ledger.db unavailable".to_string()),
            },
            Vec::new(),
            ReviewTests {
                status: "unavailable".to_string(),
                exercising: Vec::new(),
                untested: Vec::new(),
                notes: Some("ledger.db unavailable".to_string()),
            },
            ReviewContracts {
                status: "unavailable".to_string(),
                items: Vec::new(),
                truncated: false,
            },
        ),
    };

    let (prior_decisions, prior_decisions_truncated, claims, claims_truncated) = match &storage {
        Some(storage) if !files_changed.is_empty() => {
            search_ledger_overlap(storage, &files_changed)
        }
        _ => (Vec::new(), false, Vec::new(), false),
    };

    let mut promised_out = promised;
    let (mut findings, coordinated) = load_conductor(review_cfg, opts.id.as_deref());
    if let Some(items) = conductor_requirement_items(review_cfg, opts.id.as_deref()) {
        merge_promised(&mut promised_out, items);
    }

    let suppress_github = suppress_github_http(coordinated.is_some(), &findings.status);
    let github_bits = if suppress_github {
        GithubBits {
            attempted: false,
            requirements: None,
            findings: Vec::new(),
        }
    } else {
        load_github(work_dir, review_cfg, opts.id.as_deref())
    };
    if let Some(items) = github_bits.requirements {
        merge_promised(&mut promised_out, items);
    }
    if github_bits.attempted {
        let conductor_produced = findings.status == "ok";
        let github_failed = github_bits
            .findings
            .iter()
            .any(|i| i.severity.as_deref() == Some("unavailable"));
        merge_findings(&mut findings, github_bits.findings);
        if conductor_produced || !github_failed {
            findings.status = "ok".to_string();
        } else {
            findings.status = "unavailable".to_string();
        }
    }

    let ci_evidence = load_ci_evidence(
        layout,
        review_cfg,
        work_dir,
        opts.id.as_deref(),
        suppress_github,
    );

    let envelope = ReviewEnvelope {
        schema_version: REVIEW_SCHEMA_VERSION,
        kind: REVIEW_KIND.to_string(),
        analysis_mode: REVIEW_ANALYSIS_MODE.to_string(),
        range: git_range,
        base_ref,
        head_ref,
        files_changed,
        files_changed_truncated,
        blast,
        affected_symbols,
        tests,
        contracts,
        promised_requirements: promised_out,
        requirements_files_truncated,
        files_intended,
        files_unexpected,
        files_intended_truncated,
        files_unexpected_truncated,
        prior_decisions,
        prior_decisions_truncated,
        implementation_claims: claims,
        implementation_claims_truncated: claims_truncated,
        unresolved_findings: findings,
        ci_evidence,
        coordinated,
    };

    emit_review(&envelope, opts.json, out)
}

fn parse_review_range(range: &str) -> Result<(String, String, String)> {
    parse_pr_range(range).map_err(|e| {
        let msg = format!("{e}").replace("--pr range", "<RANGE>");
        miette::miette!("{msg}")
    })
}

fn github_backend_on(cfg: &ReviewConfig) -> bool {
    cfg.github.enabled
}

fn conductor_backend_on(cfg: &ReviewConfig) -> bool {
    !cfg.conductor.root.trim().is_empty()
}

fn file_changes_to_review(changes: &[FileChange]) -> Vec<ReviewFileChange> {
    changes
        .iter()
        .map(|c| {
            let path = normalize_path(&c.path);
            match &c.change_type {
                ChangeType::Added => ReviewFileChange {
                    path,
                    change_type: "added".to_string(),
                    old_path: None,
                },
                ChangeType::Modified => ReviewFileChange {
                    path,
                    change_type: "modified".to_string(),
                    old_path: None,
                },
                ChangeType::Deleted => ReviewFileChange {
                    path,
                    change_type: "deleted".to_string(),
                    old_path: None,
                },
                ChangeType::Renamed { old_path } => ReviewFileChange {
                    path,
                    change_type: "renamed".to_string(),
                    old_path: Some(normalize_path(old_path)),
                },
            }
        })
        .collect()
}

fn normalize_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn build_range_snapshot(work_dir: &Path, changes: Vec<FileChange>, head_ref: &str) -> RepoSnapshot {
    let (head_hash, branch_name) = match open_repo(work_dir) {
        Ok(repo) => get_head_info(&repo).unwrap_or_default(),
        Err(_) => (None, None),
    };
    let head_hash = resolve_commit_oid(work_dir, head_ref).ok().or(head_hash);
    RepoSnapshot {
        head_hash,
        branch_name,
        is_clean: changes.is_empty(),
        changes,
    }
}

fn open_review_storage(layout: &Layout) -> Option<StorageManager> {
    let db_path = layout.state_subdir().join("ledger.db");
    if !db_path.exists() {
        return None;
    }
    StorageManager::open_read_only_sqlite_only(layout).ok()
}

fn compose_impact(
    layout: &Layout,
    work_dir: &Path,
    config: &Config,
    storage: &StorageManager,
    snapshot: &RepoSnapshot,
    files_total: usize,
    files_changed_truncated: bool,
) -> (ReviewBlast, Vec<String>, ReviewTests, ReviewContracts) {
    let files_scanned = snapshot.changes.len();
    let impact = crate::commands::impact::compute_impact_from_snapshot_in_memory_with_mode(
        storage,
        config,
        work_dir,
        snapshot.clone(),
        false,
        REVIEW_ANALYSIS_MODE,
        Vec::new(),
    );
    let (blast, symbols) = match impact {
        Ok(packet) => {
            let edges = packet
                .blast_radius
                .as_ref()
                .map(|b| b.edges.len())
                .unwrap_or(0);
            let nodes = packet
                .blast_radius
                .as_ref()
                .map(|b| b.must_touch_files.len() + b.must_touch_symbols.len())
                .unwrap_or(0);
            let mut symbols = top_symbols_from_packet(&packet);
            symbols.truncate(MAX_AFFECTED_SYMBOLS);
            (
                ReviewBlast {
                    status: "ok".to_string(),
                    files_scanned,
                    files_total,
                    files_capped: files_changed_truncated,
                    nodes,
                    edges,
                    reason: None,
                },
                symbols,
            )
        }
        Err(e) => (
            ReviewBlast {
                status: "unavailable".to_string(),
                files_scanned,
                files_total,
                files_capped: files_changed_truncated,
                nodes: 0,
                edges: 0,
                reason: Some(format!("impact compose failed: {e}")),
            },
            Vec::new(),
        ),
    };

    let gaps = compute_pr_scan_test_gaps(layout, snapshot);
    let tests = tests_from_gaps(&gaps);
    let flows = compute_pr_scan_affected_flows(layout, snapshot);
    let contracts = contracts_from_flows(&flows);
    (blast, symbols, tests, contracts)
}

fn top_symbols_from_packet(packet: &crate::impact::packet::ImpactPacket) -> Vec<String> {
    let mut symbols = Vec::new();
    for c in &packet.changes {
        if let Some(ref syms) = c.symbols {
            for s in syms {
                let name = s.qualified_name.clone().unwrap_or_else(|| s.name.clone());
                if !name.is_empty() {
                    symbols.push(name);
                }
            }
        }
    }
    symbols.sort();
    symbols.dedup();
    symbols.truncate(MAX_AFFECTED_SYMBOLS);
    symbols
}

fn tests_from_gaps(gaps: &TestGapsReport) -> ReviewTests {
    let mut exercising: Vec<String> = gaps
        .mapped_sample
        .iter()
        .map(|m| {
            if m.file.is_empty() {
                m.symbol.clone()
            } else {
                m.file.replace('\\', "/")
            }
        })
        .collect();
    exercising.sort();
    exercising.dedup();
    let mut untested: Vec<String> = gaps
        .unmapped
        .iter()
        .map(|u| {
            if u.file.is_empty() {
                u.symbol.clone()
            } else {
                u.file.replace('\\', "/")
            }
        })
        .collect();
    untested.sort();
    untested.dedup();
    let notes = if gaps.notes.is_empty() {
        None
    } else {
        Some(gaps.notes.join("; "))
    };
    ReviewTests {
        status: gaps.status.as_str().to_string(),
        exercising,
        untested,
        notes,
    }
}

fn contracts_from_flows(flows: &AffectedFlowsReport) -> ReviewContracts {
    let mut items: Vec<serde_json::Value> = flows
        .flows
        .iter()
        .filter_map(|f| serde_json::to_value(f).ok())
        .collect();
    items.sort_by_key(|a| a.to_string());
    let truncated = items.len() > MAX_CONTRACTS;
    items.truncate(MAX_CONTRACTS);
    ReviewContracts {
        status: flows.status.as_str().to_string(),
        items,
        truncated,
    }
}

fn path_stem(path: &str) -> Option<String> {
    let name = Path::new(path).file_stem()?.to_string_lossy();
    let stem = name.trim();
    if stem.is_empty() {
        None
    } else {
        Some(stem.to_string())
    }
}

fn search_ledger_overlap(
    storage: &StorageManager,
    files: &[ReviewFileChange],
) -> (Vec<ReviewDecision>, bool, Vec<ReviewClaim>, bool) {
    let mut stems: Vec<String> = files.iter().filter_map(|f| path_stem(&f.path)).collect();
    stems.sort();
    stems.dedup();
    stems.truncate(MAX_SEARCH_STEMS);

    let db = LedgerDb::new(storage.get_connection());
    let mut decisions: BTreeMap<String, ReviewDecision> = BTreeMap::new();
    let mut claims: BTreeMap<String, ReviewClaim> = BTreeMap::new();
    for stem in &stems {
        match db.search_ledger(stem, None, None, false, Some(20), 0, false) {
            Ok(entries) => {
                for e in entries {
                    decisions.entry(e.tx_id.clone()).or_insert(ReviewDecision {
                        tx_id: e.tx_id.clone(),
                        summary: e.summary.clone(),
                    });
                    claims.entry(e.tx_id.clone()).or_insert(ReviewClaim {
                        tx_id: e.tx_id,
                        summary: e.summary,
                        reason: e.reason,
                    });
                }
            }
            Err(_) => continue,
        }
    }
    let mut prior: Vec<ReviewDecision> = decisions.into_values().collect();
    prior.sort_by(|a, b| a.tx_id.cmp(&b.tx_id));
    let prior_truncated = prior.len() > MAX_PRIOR_DECISIONS;
    prior.truncate(MAX_PRIOR_DECISIONS);
    let mut claim_items: Vec<ReviewClaim> = claims.into_values().collect();
    claim_items.sort_by(|a, b| a.tx_id.cmp(&b.tx_id));
    let claims_truncated = claim_items.len() > MAX_CLAIMS;
    claim_items.truncate(MAX_CLAIMS);
    (prior, prior_truncated, claim_items, claims_truncated)
}

fn load_promised_requirements(
    layout: &Layout,
    work_dir: &Path,
    cfg: &ReviewConfig,
    opts: &ReviewOpts,
) -> Result<(bool, ReviewRequirements)> {
    let mut paths: Vec<String> = Vec::new();
    paths.extend(cfg.requirements_files.iter().cloned());
    let mut cli = opts.requirements.clone();
    let requirements_files_truncated = cli.len() > MAX_REQUIREMENTS_PATHS;
    if requirements_files_truncated {
        cli.truncate(MAX_REQUIREMENTS_PATHS);
    }
    paths.extend(cli);
    paths.sort();
    paths.dedup();

    let mut items = Vec::new();
    for raw in paths {
        let raw = raw.trim();
        if raw.is_empty() {
            continue;
        }
        let path = resolve_maybe_relative(work_dir, layout, raw);
        if !path.is_file() {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let clipped = truncate_bytes(text.trim(), MAX_REQUIREMENT_TEXT);
        if clipped.is_empty() {
            continue;
        }
        items.push(ReviewRequirementItem {
            id: None,
            source: normalize_path(&path),
            text: clipped,
        });
    }
    items.sort_by(|a, b| a.source.cmp(&b.source));
    let truncated = items.len() > MAX_REQUIREMENT_ITEMS;
    items.truncate(MAX_REQUIREMENT_ITEMS);
    let status = if items.is_empty() { "none" } else { "ok" };
    Ok((
        requirements_files_truncated,
        ReviewRequirements {
            status: status.to_string(),
            items,
            truncated,
        },
    ))
}

fn resolve_maybe_relative(work_dir: &Path, layout: &Layout, raw: &str) -> PathBuf {
    let p = PathBuf::from(raw);
    if p.is_absolute() {
        p
    } else if work_dir.join(&p).exists() {
        work_dir.join(p)
    } else {
        layout.root.as_std_path().join(p)
    }
}

fn truncate_bytes(text: &str, max: usize) -> String {
    let mut bytes = text.as_bytes();
    if bytes.len() <= max {
        return text.to_string();
    }
    bytes = &bytes[..max];
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_string(),
        Err(e) => {
            let valid = e.valid_up_to();
            String::from_utf8_lossy(&bytes[..valid]).into_owned()
        }
    }
}

fn classify_paths(
    files: &[ReviewFileChange],
    cfg: &ReviewConfig,
    reqs: &[ReviewRequirementItem],
) -> (Option<Vec<String>>, Option<Vec<String>>, bool, bool) {
    let set = compile_globs(&cfg.expected_path_globs);
    let req_blob: String = reqs
        .iter()
        .map(|r| r.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let mut intended = Vec::new();
    let mut unexpected = Vec::new();
    for f in files {
        let glob_hit = set.as_ref().map(|g| g.is_match(&f.path)).unwrap_or(false);
        let req_hit = !req_blob.is_empty() && req_blob.contains(&f.path);
        if glob_hit || req_hit {
            intended.push(f.path.clone());
        } else {
            unexpected.push(f.path.clone());
        }
    }
    intended.sort();
    intended.dedup();
    unexpected.sort();
    unexpected.dedup();
    let intended_truncated = intended.len() > MAX_FILES_CLASSIFIED;
    let unexpected_truncated = unexpected.len() > MAX_FILES_CLASSIFIED;
    intended.truncate(MAX_FILES_CLASSIFIED);
    unexpected.truncate(MAX_FILES_CLASSIFIED);
    (
        Some(intended),
        Some(unexpected),
        intended_truncated,
        unexpected_truncated,
    )
}

fn compile_globs(patterns: &[String]) -> Option<GlobSet> {
    if patterns.is_empty() {
        return None;
    }
    let mut builder = GlobSetBuilder::new();
    let mut any = false;
    for p in patterns {
        if let Ok(g) = Glob::new(p) {
            builder.add(g);
            any = true;
        }
    }
    if !any {
        return None;
    }
    builder.build().ok()
}

fn merge_promised(dest: &mut ReviewRequirements, extra: Vec<ReviewRequirementItem>) {
    dest.items.extend(extra);
    dest.items
        .sort_by(|a, b| a.source.cmp(&b.source).then(a.text.cmp(&b.text)));
    dest.items.dedup();
    dest.truncated = dest.items.len() > MAX_REQUIREMENT_ITEMS;
    dest.items.truncate(MAX_REQUIREMENT_ITEMS);
    if !dest.items.is_empty() {
        dest.status = "ok".to_string();
    }
}

fn merge_findings(dest: &mut ReviewFindings, extra: Vec<ReviewFindingItem>) {
    dest.items.extend(extra);
    dest.items
        .sort_by(|a, b| a.source.cmp(&b.source).then(a.title.cmp(&b.title)));
    dest.truncated = dest.items.len() > MAX_FINDINGS;
    dest.items.truncate(MAX_FINDINGS);
    if !dest.items.is_empty() {
        dest.status = "ok".to_string();
    }
}

fn load_conductor(
    cfg: &ReviewConfig,
    id: Option<&str>,
) -> (ReviewFindings, Option<ReviewCoordinated>) {
    if !conductor_backend_on(cfg) {
        return (
            ReviewFindings {
                status: "none".to_string(),
                items: Vec::new(),
                truncated: false,
            },
            None,
        );
    }
    let Some(id) = id else {
        return (
            ReviewFindings {
                status: "none".to_string(),
                items: Vec::new(),
                truncated: false,
            },
            None,
        );
    };
    match resolve_conductor_track(&cfg.conductor.root, id) {
        Ok((track_id, spec_path)) => {
            let findings = conductor_findings(spec_path.parent().unwrap_or(Path::new("")));
            (
                findings,
                Some(ReviewCoordinated {
                    track_id,
                    spec_path: normalize_path(&spec_path),
                }),
            )
        }
        Err(reason) => {
            let genuine_miss = reason.contains("no conductor track matching");
            if id.parse::<u64>().is_ok() && cfg.github.enabled && genuine_miss {
                (
                    ReviewFindings {
                        status: "none".to_string(),
                        items: Vec::new(),
                        truncated: false,
                    },
                    None,
                )
            } else {
                (
                    ReviewFindings {
                        status: "unavailable".to_string(),
                        items: vec![ReviewFindingItem {
                            source: "conductor".to_string(),
                            title: reason,
                            severity: Some("unavailable".to_string()),
                        }],
                        truncated: false,
                    },
                    None,
                )
            }
        }
    }
}

fn conductor_requirement_items(
    cfg: &ReviewConfig,
    id: Option<&str>,
) -> Option<Vec<ReviewRequirementItem>> {
    let id = id?;
    if !conductor_backend_on(cfg) {
        return None;
    }
    let (track_id, spec_path) = resolve_conductor_track(&cfg.conductor.root, id).ok()?;
    let text = std::fs::read_to_string(&spec_path).ok()?;
    let objective = extract_heading_section(&text, &["Objective"]);
    let dod = extract_heading_section(&text, &["Definition of Done"]);
    let mut items = Vec::new();
    if let Some(t) = objective {
        items.push(ReviewRequirementItem {
            id: Some(track_id.clone()),
            source: format!("{}#objective", normalize_path(&spec_path)),
            text: truncate_bytes(&t, MAX_REQUIREMENT_TEXT),
        });
    }
    if let Some(t) = dod {
        items.push(ReviewRequirementItem {
            id: Some(track_id),
            source: format!("{}#dod", normalize_path(&spec_path)),
            text: truncate_bytes(&t, MAX_REQUIREMENT_TEXT),
        });
    }
    if items.is_empty() { None } else { Some(items) }
}

fn resolve_conductor_track(root: &str, id: &str) -> std::result::Result<(String, PathBuf), String> {
    let root = PathBuf::from(root.trim());
    if !root.is_dir() {
        return Err("conductor root is not a directory".to_string());
    }
    let exact = root.join(id);
    if exact.is_dir() {
        let spec = exact.join("spec.md");
        if spec.is_file() {
            return Ok((id.to_string(), spec));
        }
    }
    let prefix = format!("{id}-");
    let mut matches = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&root) {
        for ent in rd.flatten() {
            let name = ent.file_name().to_string_lossy().into_owned();
            if name == id || name.starts_with(&prefix) {
                let spec = ent.path().join("spec.md");
                if spec.is_file() {
                    matches.push((name, spec));
                }
            }
        }
    }
    matches.sort_by(|a, b| a.0.cmp(&b.0));
    match matches.as_slice() {
        [(name, spec)] => Ok((name.clone(), spec.clone())),
        [] => Err(format!("no conductor track matching `{id}`")),
        _ => Err(format!("ambiguous conductor track prefix `{id}`")),
    }
}

fn is_named_heading(line: &str, names: &[&str]) -> bool {
    let t = line.trim();
    if !t.starts_with("##") {
        return false;
    }
    let rest = t.trim_start_matches('#').trim();
    let rest = if let Some((n, tail)) = rest.split_once('.') {
        if n.chars().all(|c| c.is_ascii_digit()) {
            tail.trim()
        } else {
            rest
        }
    } else {
        rest
    };
    names.iter().any(|n| rest.eq_ignore_ascii_case(n))
}

fn extract_heading_section(markdown: &str, names: &[&str]) -> Option<String> {
    let mut lines = markdown.lines();
    let mut collecting = false;
    let mut buf = Vec::new();
    for line in lines.by_ref() {
        if is_named_heading(line, names) {
            collecting = true;
            continue;
        }
        if collecting && line.trim_start().starts_with("## ") {
            break;
        }
        if collecting {
            buf.push(line);
        }
    }
    let text = buf.join("\n").trim().to_string();
    if text.is_empty() { None } else { Some(text) }
}

fn conductor_findings(track_dir: &Path) -> ReviewFindings {
    let mut items = Vec::new();
    let Ok(rd) = std::fs::read_dir(track_dir) else {
        return ReviewFindings {
            status: "unavailable".to_string(),
            items: Vec::new(),
            truncated: false,
        };
    };
    let mut names: Vec<PathBuf> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            let n = p.file_name().and_then(|s| s.to_str()).unwrap_or("");
            n == "review.md"
                || (n.ends_with("-review.md") && !n.eq_ignore_ascii_case("AI-review.md"))
        })
        .collect();
    names.sort();
    for path in names {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let source = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("review")
            .to_string();
        for line in text.lines() {
            let t = line.trim();
            if !t.starts_with('|') || !conductor_finding_row_is_open(t) {
                continue;
            }
            items.push(ReviewFindingItem {
                source: source.clone(),
                title: truncate_bytes(t, 256),
                severity: None,
            });
        }
    }
    items.sort_by(|a, b| a.title.cmp(&b.title));
    items.dedup();
    let truncated = items.len() > MAX_FINDINGS;
    items.truncate(MAX_FINDINGS);
    ReviewFindings {
        status: "ok".to_string(),
        items,
        truncated,
    }
}

const CONDUCTOR_CLOSED_CELLS: &[&str] = &[
    "closed",
    "folded",
    "declined",
    "verified_fixed",
    "agree — fold",
    "agree - fold",
    "resolved",
];

fn normalize_conductor_cell(c: &str) -> String {
    c.trim()
        .trim_matches(|ch| ch == '*' || ch == '`' || ch == '_')
        .to_ascii_lowercase()
}

fn conductor_finding_row_is_open(t: &str) -> bool {
    let cells: Vec<String> = t
        .split('|')
        .map(normalize_conductor_cell)
        .filter(|c| !c.is_empty())
        .collect();
    if cells
        .iter()
        .any(|c| CONDUCTOR_CLOSED_CELLS.contains(&c.as_str()))
    {
        return false;
    }
    t.contains("- [ ]") || t.contains("UNRESOLVED") || cells.iter().any(|c| c == "open")
}

fn should_fetch_github_checks(cfg: &ReviewConfig) -> bool {
    cfg.github.enabled && cfg.ci.github_checks
}

/// Skip GitHub findings *and* check-run HTTP when conductor already owns
/// the `--id` (resolved track) or reported a real failure (ambiguous /
/// slug miss). Genuine numeric miss stays `status: none` so GitHub may run.
fn suppress_github_http(coordinated: bool, findings_status: &str) -> bool {
    coordinated || findings_status == "unavailable"
}

struct GithubBits {
    attempted: bool,
    requirements: Option<Vec<ReviewRequirementItem>>,
    findings: Vec<ReviewFindingItem>,
}

fn load_github(work_dir: &Path, cfg: &ReviewConfig, id: Option<&str>) -> GithubBits {
    if !cfg.github.enabled {
        return GithubBits {
            attempted: false,
            requirements: None,
            findings: Vec::new(),
        };
    }
    let Some(id) = id else {
        return GithubBits {
            attempted: false,
            requirements: None,
            findings: Vec::new(),
        };
    };
    if id.parse::<u64>().is_err() {
        return GithubBits {
            attempted: false,
            requirements: None,
            findings: Vec::new(),
        };
    }
    let token = std::env::var("GITHUB_TOKEN")
        .or_else(|_| std::env::var("GH_TOKEN"))
        .ok();
    let Some(token) = token else {
        return GithubBits {
            attempted: true,
            requirements: None,
            findings: vec![ReviewFindingItem {
                source: "github".to_string(),
                title: "GITHUB_TOKEN / GH_TOKEN missing".to_string(),
                severity: Some("unavailable".to_string()),
            }],
        };
    };
    let repo = resolve_github_repo(work_dir, &cfg.github.repo);
    let Some(repo) = repo else {
        return GithubBits {
            attempted: true,
            requirements: None,
            findings: vec![ReviewFindingItem {
                source: "github".to_string(),
                title: "github repo unparseable (set [review.github].repo or origin)".to_string(),
                severity: Some("unavailable".to_string()),
            }],
        };
    };
    match fetch_github_pr(&token, &repo, id) {
        Ok(bits) => bits,
        Err(reason) => GithubBits {
            attempted: true,
            requirements: None,
            findings: vec![ReviewFindingItem {
                source: "github".to_string(),
                title: reason,
                severity: Some("unavailable".to_string()),
            }],
        },
    }
}

fn resolve_github_repo(work_dir: &Path, configured: &str) -> Option<String> {
    let trimmed = configured.trim();
    if !trimmed.is_empty() {
        return Some(trimmed.trim_matches('/').to_string());
    }
    let output = crate::git::git_command()
        .ok()?
        .args(["remote", "get-url", "origin"])
        .current_dir(work_dir)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let url = String::from_utf8_lossy(&output.stdout);
    parse_origin_owner_repo(url.trim())
}

fn parse_origin_owner_repo(url: &str) -> Option<String> {
    let url = url.trim().trim_end_matches(".git");
    if let Some(rest) = url.strip_prefix("git@") {
        let (_, path) = rest.split_once(':')?;
        return owner_repo(path);
    }
    if let Some(idx) = url.find("github.com/") {
        return owner_repo(&url[idx + "github.com/".len()..]);
    }
    if let Some(idx) = url.find("github.com:") {
        return owner_repo(&url[idx + "github.com:".len()..]);
    }
    None
}

fn owner_repo(path: &str) -> Option<String> {
    let path = path.trim().trim_start_matches('/');
    let mut parts = path.split('/');
    let owner = parts.next()?;
    let repo = parts.next()?;
    if owner.is_empty() || repo.is_empty() {
        return None;
    }
    Some(format!("{owner}/{repo}"))
}

fn fetch_github_pr(token: &str, repo: &str, pr: &str) -> std::result::Result<GithubBits, String> {
    let agent = ureq::AgentBuilder::new()
        .timeout(std::time::Duration::from_secs(8))
        .build();
    let url = format!("https://api.github.com/repos/{repo}/pulls/{pr}");
    let body = github_get_json(&agent, token, &url)?;
    let mut reqs = Vec::new();
    if let Some(text) = body.get("body").and_then(|v| v.as_str()) {
        let clipped = truncate_bytes(text.trim(), MAX_REQUIREMENT_TEXT);
        if !clipped.is_empty() {
            reqs.push(ReviewRequirementItem {
                id: Some(pr.to_string()),
                source: format!("github:{repo}#{pr}"),
                text: clipped,
            });
        }
    }
    let comments_url = format!("https://api.github.com/repos/{repo}/pulls/{pr}/comments");
    let mut findings = Vec::new();
    if let Ok(comments) = github_get_json(&agent, token, &comments_url)
        && let Some(arr) = comments.as_array()
    {
        for c in arr {
            let title = c
                .get("body")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .lines()
                .next()
                .unwrap_or("")
                .trim()
                .to_string();
            if title.is_empty() {
                continue;
            }
            findings.push(ReviewFindingItem {
                source: "github".to_string(),
                title: truncate_bytes(&title, 256),
                severity: None,
            });
        }
    }
    findings.sort_by(|a, b| a.title.cmp(&b.title));
    findings.truncate(MAX_FINDINGS);
    Ok(GithubBits {
        attempted: true,
        requirements: if reqs.is_empty() { None } else { Some(reqs) },
        findings,
    })
}

fn github_get_json(
    agent: &ureq::Agent,
    token: &str,
    url: &str,
) -> std::result::Result<serde_json::Value, String> {
    let resp = agent
        .get(url)
        .set("Authorization", &format!("Bearer {token}"))
        .set("User-Agent", "ledgerful-review")
        .set("Accept", "application/vnd.github+json")
        .call()
        .map_err(|e| format!("github http: {e}"))?;
    resp.into_json().map_err(|e| format!("github json: {e}"))
}

fn load_ci_evidence(
    layout: &Layout,
    cfg: &ReviewConfig,
    work_dir: &Path,
    id: Option<&str>,
    suppress_github: bool,
) -> ReviewCiEvidence {
    let mut items = Vec::new();
    let history_path = layout.reports_dir().join(VERIFY_HISTORY);
    if history_path.exists() {
        match std::fs::read_to_string(&history_path) {
            Ok(content) => match parse_verify_history(&content) {
                Ok(mut recs) => {
                    recs.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));
                    for r in recs.into_iter().rev() {
                        items.push(ReviewCiItem::VerifyHistory {
                            timestamp: r.timestamp,
                            passed: r.passed,
                            duration_secs: r.duration_secs,
                            tx_id: r.tx_id,
                        });
                    }
                }
                Err(e) => {
                    return ReviewCiEvidence {
                        status: "unavailable".to_string(),
                        items: Vec::new(),
                        truncated: false,
                    }
                    .with_note_unused(e);
                }
            },
            Err(_) => {
                return ReviewCiEvidence {
                    status: "unavailable".to_string(),
                    items: Vec::new(),
                    truncated: false,
                };
            }
        }
    }

    if !suppress_github
        && should_fetch_github_checks(cfg)
        && let Some(extra) = fetch_github_checks(work_dir, cfg, id)
    {
        items.extend(extra);
    }

    items.sort_by(|a, b| format!("{a:?}").cmp(&format!("{b:?}")));
    let truncated = items.len() > MAX_CI;
    items.truncate(MAX_CI);
    let status = if items.is_empty() {
        "unavailable"
    } else {
        "ok"
    };
    ReviewCiEvidence {
        status: status.to_string(),
        items,
        truncated,
    }
}

impl ReviewCiEvidence {
    fn with_note_unused(mut self, _e: serde_json::Error) -> Self {
        self.status = "unavailable".to_string();
        self
    }
}

fn fetch_github_checks(
    work_dir: &Path,
    cfg: &ReviewConfig,
    id: Option<&str>,
) -> Option<Vec<ReviewCiItem>> {
    let token = std::env::var("GITHUB_TOKEN")
        .or_else(|_| std::env::var("GH_TOKEN"))
        .ok()?;
    let repo = resolve_github_repo(work_dir, &cfg.github.repo)?;
    let pr = id.filter(|s| s.parse::<u64>().is_ok())?;
    let agent = ureq::AgentBuilder::new()
        .timeout(std::time::Duration::from_secs(8))
        .build();
    let url = format!("https://api.github.com/repos/{repo}/pulls/{pr}");
    let body = github_get_json(&agent, &token, &url).ok()?;
    let sha = body.get("head")?.get("sha")?.as_str()?;
    let checks_url = format!("https://api.github.com/repos/{repo}/commits/{sha}/check-runs");
    let json = github_get_json(&agent, &token, &checks_url).ok()?;
    let arr = json.get("check_runs")?.as_array()?;
    let mut items = Vec::new();
    for c in arr {
        let name = c.get("name")?.as_str()?.to_string();
        items.push(ReviewCiItem::GithubCheck {
            name,
            conclusion: c
                .get("conclusion")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            html_url: c
                .get("html_url")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
        });
    }
    Some(items)
}

fn emit_review(envelope: &ReviewEnvelope, json: bool, out: &mut impl Write) -> Result<()> {
    if json {
        let body = crate::output::json::format_json(envelope)?;
        write!(out, "{body}").map_err(|e| miette::miette!("review json write: {e}"))?;
        return Ok(());
    }
    writeln!(
        out,
        "review {} — {} file(s) changed.",
        envelope.range,
        envelope.files_changed.len()
    )
    .map_err(|e| miette::miette!("review write: {e}"))?;
    writeln!(out, "Use --json for the agent envelope.")
        .map_err(|e| miette::miette!("review write: {e}"))?;
    Ok(())
}

#[allow(dead_code)]
#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::args::Cli;
    use camino::Utf8Path;
    use clap::{CommandFactory, Parser};
    use std::fs;
    use std::process::Command;
    use tempfile::tempdir;

    mod env_guard {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/integration/common/env_guard.rs"
        ));
    }
    use env_guard::TempEnv;

    fn parse_cli(args: &[&str]) -> std::result::Result<Cli, clap::Error> {
        let mut full = vec!["ledgerful"];
        full.extend_from_slice(args);
        Cli::try_parse_from(full)
    }

    fn git(dir: &Path, args: &[&str]) -> std::process::Output {
        Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.com")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.com")
            .output()
            .expect("git")
    }

    fn init_git_repo(dir: &Path) {
        assert!(git(dir, &["init", "-b", "main"]).status.success());
        git(dir, &["config", "user.email", "t@example.com"]);
        git(dir, &["config", "user.name", "t"]);
        fs::write(dir.join("README.md"), "one\n").unwrap();
        assert!(git(dir, &["add", "README.md"]).status.success());
        assert!(git(dir, &["commit", "-m", "first"]).status.success());
        fs::write(dir.join("README.md"), "two\n").unwrap();
        assert!(git(dir, &["add", "README.md"]).status.success());
        assert!(git(dir, &["commit", "-m", "second"]).status.success());
    }

    fn harness() -> (tempfile::TempDir, Layout, PathBuf, Config) {
        let tmp = tempdir().unwrap();
        init_git_repo(tmp.path());
        let work = tmp.path().to_path_buf();
        let root = Utf8Path::from_path(&work).expect("utf8");
        let state = root.join(".ledgerful");
        let layout = Layout::from_roots(root, &state);
        layout.ensure_state_dir().unwrap();
        (tmp, layout, work, Config::default())
    }

    fn run(
        layout: &Layout,
        work: &Path,
        config: &Config,
        opts: ReviewOpts,
    ) -> (Result<()>, String) {
        let mut buf = Vec::new();
        let result = execute_review_in(layout, work, config, &opts, &mut buf);
        (result, String::from_utf8_lossy(&buf).into_owned())
    }

    fn parse_env(stdout: &str) -> ReviewEnvelope {
        serde_json::from_str(stdout.trim()).unwrap_or_else(|e| panic!("json: {e}\n{stdout}"))
    }

    #[test]
    fn review_help_lists_range_and_json() {
        let cmd = Cli::command();
        let review = cmd
            .get_subcommands()
            .find(|c| c.get_name() == "review")
            .expect("review subcommand");
        let help = review.clone().render_help().to_string();
        assert!(help.contains("RANGE"), "{help}");
        assert!(help.contains("--json"), "{help}");
        assert!(help.contains("--requirements"), "{help}");
        assert!(help.contains("--id"), "{help}");
        assert!(
            !review
                .get_arguments()
                .any(|a| a.get_long() == Some("track")),
            "review must not grow a --track flag:\n{help}"
        );
    }

    #[test]
    fn review_missing_range_is_clap_usage() {
        assert!(parse_cli(&["review"]).is_err());
    }

    #[test]
    fn review_json_is_machine_output() {
        let cli = parse_cli(&["review", "HEAD~1..HEAD", "--json"]).expect("parse");
        assert!(cli.command.is_machine_output());
        assert_eq!(cli.command.command_name(), "review");
    }

    #[test]
    fn review_two_dot_range_normalizes_like_scan_pr() {
        let (base, head, range) = parse_review_range("HEAD~1..HEAD").unwrap();
        assert_eq!(base, "HEAD~1");
        assert_eq!(head, "HEAD");
        assert_eq!(range, "HEAD~1...HEAD");
        let err = parse_review_range("").unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("<RANGE>"), "{msg}");
        assert!(!msg.contains("--pr range"), "{msg}");
    }

    #[test]
    fn review_id_without_backend_is_error() {
        let (_tmp, layout, work, config) = harness();
        let (result, stdout) = run(
            &layout,
            &work,
            &config,
            ReviewOpts {
                range: "HEAD~1..HEAD".to_string(),
                json: true,
                requirements: Vec::new(),
                id: Some("0304".to_string()),
            },
        );
        assert!(result.is_err(), "{result:?}");
        assert!(stdout.is_empty());
        let msg = format!("{}", result.unwrap_err());
        assert!(msg.contains("configured review backend"), "{msg}");
    }

    #[test]
    fn review_json_kind_review_schema_version_1() {
        let (_tmp, layout, work, config) = harness();
        let (result, stdout) = run(
            &layout,
            &work,
            &config,
            ReviewOpts {
                range: "HEAD~1..HEAD".to_string(),
                json: true,
                requirements: Vec::new(),
                id: None,
            },
        );
        assert!(result.is_ok(), "{result:?}\n{stdout}");
        let env = parse_env(&stdout);
        assert_eq!(env.schema_version, 1);
        assert_eq!(env.kind, "review");
        assert_eq!(env.analysis_mode, "range");
        assert_eq!(env.range, "HEAD~1...HEAD");
    }

    #[test]
    fn review_empty_config_promised_none_omits_unexpected() {
        let (_tmp, layout, work, config) = harness();
        let (result, stdout) = run(
            &layout,
            &work,
            &config,
            ReviewOpts {
                range: "HEAD~1..HEAD".to_string(),
                json: true,
                requirements: Vec::new(),
                id: None,
            },
        );
        assert!(result.is_ok(), "{result:?}\n{stdout}");
        let env = parse_env(&stdout);
        assert_eq!(env.promised_requirements.status, "none");
        assert!(env.files_intended.is_none());
        assert!(env.files_unexpected.is_none());
        assert!(!stdout.contains("filesIntended"));
        assert!(!stdout.contains("filesUnexpected"));
        assert!(!stdout.to_ascii_lowercase().contains("coordinated"));
        assert_eq!(env.analysis_mode, "range");
    }

    #[test]
    fn review_does_not_rewrite_latest_impact_json() {
        let (_tmp, layout, work, config) = harness();
        let impact = layout.reports_dir().join("latest-impact.json");
        assert!(!impact.exists());
        let (result, _) = run(
            &layout,
            &work,
            &config,
            ReviewOpts {
                range: "HEAD~1..HEAD".to_string(),
                json: true,
                requirements: Vec::new(),
                id: None,
            },
        );
        assert!(result.is_ok(), "{result:?}");
        assert!(!impact.exists());
    }

    #[test]
    fn review_missing_db_fields_unavailable() {
        let (_tmp, layout, work, config) = harness();
        assert!(!layout.state_subdir().join("ledger.db").exists());
        let (result, stdout) = run(
            &layout,
            &work,
            &config,
            ReviewOpts {
                range: "HEAD~1..HEAD".to_string(),
                json: true,
                requirements: Vec::new(),
                id: None,
            },
        );
        assert!(result.is_ok(), "{result:?}\n{stdout}");
        let env = parse_env(&stdout);
        assert_eq!(env.blast.status, "unavailable");
        assert!(env.blast.reason.as_deref().unwrap().contains("ledger.db"));
        assert_eq!(env.tests.status, "unavailable");
        assert_eq!(env.contracts.status, "unavailable");
    }

    #[test]
    fn review_expected_path_globs_classifies_intended_and_unexpected() {
        let (tmp, layout, work, mut config) = harness();
        fs::write(tmp.path().join("keep.rs"), "fn k() {}\n").unwrap();
        fs::write(tmp.path().join("skip.md"), "x\n").unwrap();
        assert!(
            git(tmp.path(), &["add", "keep.rs", "skip.md"])
                .status
                .success()
        );
        assert!(git(tmp.path(), &["commit", "-m", "two"]).status.success());
        config.review.expected_path_globs = vec!["*.rs".to_string()];
        let (result, stdout) = run(
            &layout,
            &work,
            &config,
            ReviewOpts {
                range: "HEAD~1..HEAD".to_string(),
                json: true,
                requirements: Vec::new(),
                id: None,
            },
        );
        assert!(result.is_ok(), "{result:?}\n{stdout}");
        let env = parse_env(&stdout);
        let intended = env.files_intended.expect("intended");
        let unexpected = env.files_unexpected.expect("unexpected");
        assert!(
            intended.iter().any(|p| p.ends_with("keep.rs")),
            "{intended:?}"
        );
        assert!(
            unexpected.iter().any(|p| p.ends_with("skip.md")),
            "{unexpected:?}"
        );
    }

    #[test]
    fn review_requirements_files_loads_and_skips_missing() {
        let (tmp, layout, work, mut config) = harness();
        let req = tmp.path().join("reqs.md");
        fs::write(&req, "Must touch src/review.rs\n").unwrap();
        config.review.requirements_files = vec![
            req.to_string_lossy().into_owned(),
            tmp.path()
                .join("missing-req.md")
                .to_string_lossy()
                .into_owned(),
        ];
        let (result, stdout) = run(
            &layout,
            &work,
            &config,
            ReviewOpts {
                range: "HEAD~1..HEAD".to_string(),
                json: true,
                requirements: Vec::new(),
                id: None,
            },
        );
        assert!(result.is_ok(), "{result:?}\n{stdout}");
        let env = parse_env(&stdout);
        assert_eq!(env.promised_requirements.status, "ok");
        assert_eq!(env.promised_requirements.items.len(), 1);
        assert!(
            env.promised_requirements.items[0]
                .text
                .contains("src/review.rs")
        );
    }

    #[test]
    fn review_conductor_resolves_track_spec() {
        let (tmp, layout, work, mut config) = harness();
        let root = tmp.path().join("conductor");
        let track = root.join("0304-AgentReviewPacket");
        fs::create_dir_all(&track).unwrap();
        fs::write(
            track.join("spec.md"),
            "## 1. Objective\n\nShip review.\n\n## 7. Definition of Done\n\n- [ ] DoD-1\n",
        )
        .unwrap();
        config.review.conductor.root = root.to_string_lossy().into_owned();
        let (result, stdout) = run(
            &layout,
            &work,
            &config,
            ReviewOpts {
                range: "HEAD~1..HEAD".to_string(),
                json: true,
                requirements: Vec::new(),
                id: Some("0304".to_string()),
            },
        );
        assert!(result.is_ok(), "{result:?}\n{stdout}");
        let env = parse_env(&stdout);
        let coord = env.coordinated.expect("coordinated");
        assert_eq!(coord.track_id, "0304-AgentReviewPacket");
        assert!(
            env.promised_requirements
                .items
                .iter()
                .any(|i| i.text.contains("Ship review"))
        );
        assert!(
            env.promised_requirements
                .items
                .iter()
                .any(|i| i.text.contains("DoD-1"))
        );
        assert_eq!(env.unresolved_findings.status, "ok");
        assert!(env.unresolved_findings.items.is_empty());
    }

    #[test]
    fn review_verify_history_populates_ci_evidence() {
        let (_tmp, layout, work, config) = harness();
        fs::create_dir_all(layout.reports_dir().as_std_path()).unwrap();
        fs::write(
            layout.reports_dir().join(VERIFY_HISTORY).as_std_path(),
            r#"[{"timestamp":"2026-09-09T00:00:00Z","passed":true,"duration_secs":3,"tx_id":"abc"}]"#,
        )
        .unwrap();
        let (result, stdout) = run(
            &layout,
            &work,
            &config,
            ReviewOpts {
                range: "HEAD~1..HEAD".to_string(),
                json: true,
                requirements: Vec::new(),
                id: None,
            },
        );
        assert!(result.is_ok(), "{result:?}\n{stdout}");
        let env = parse_env(&stdout);
        assert_eq!(env.ci_evidence.status, "ok");
        assert!(
            env.ci_evidence
                .items
                .iter()
                .any(|i| matches!(i, ReviewCiItem::VerifyHistory { passed: true, .. }))
        );
    }

    #[test]
    fn review_range_head_not_head_lists_range_files() {
        let (tmp, layout, work, config) = harness();
        fs::write(tmp.path().join("mid.rs"), "fn mid() {}\n").unwrap();
        assert!(git(tmp.path(), &["add", "mid.rs"]).status.success());
        assert!(git(tmp.path(), &["commit", "-m", "mid"]).status.success());
        let mid = String::from_utf8_lossy(&git(tmp.path(), &["rev-parse", "HEAD"]).stdout)
            .trim()
            .to_string();
        fs::write(tmp.path().join("later.rs"), "fn later() {}\n").unwrap();
        assert!(git(tmp.path(), &["add", "later.rs"]).status.success());
        assert!(git(tmp.path(), &["commit", "-m", "later"]).status.success());
        let first = String::from_utf8_lossy(&git(tmp.path(), &["rev-parse", "HEAD~2"]).stdout)
            .trim()
            .to_string();
        let (result, stdout) = run(
            &layout,
            &work,
            &config,
            ReviewOpts {
                range: format!("{first}..{mid}"),
                json: true,
                requirements: Vec::new(),
                id: None,
            },
        );
        assert!(result.is_ok(), "{result:?}\n{stdout}");
        let env = parse_env(&stdout);
        let paths: Vec<&str> = env.files_changed.iter().map(|f| f.path.as_str()).collect();
        assert!(
            paths.iter().any(|p| p.ends_with("mid.rs")),
            "expected mid.rs in {paths:?}"
        );
        assert!(
            !paths.iter().any(|p| p.ends_with("later.rs")),
            "later.rs is after range head: {paths:?}"
        );
    }

    #[test]
    fn review_parse_origin_owner_repo_https_and_ssh() {
        assert_eq!(
            parse_origin_owner_repo("https://github.com/acme/widgets.git").as_deref(),
            Some("acme/widgets")
        );
        assert_eq!(
            parse_origin_owner_repo("git@github.com:acme/widgets.git").as_deref(),
            Some("acme/widgets")
        );
        assert!(parse_origin_owner_repo("not-a-remote").is_none());
    }

    #[test]
    fn review_stem_is_basename_without_extension() {
        assert_eq!(
            path_stem("src/commands/review.rs").as_deref(),
            Some("review")
        );
        assert_eq!(path_stem("README").as_deref(), Some("README"));
    }

    #[test]
    fn review_requirements_trims_comma_whitespace() {
        let (tmp, layout, work, config) = harness();
        let req = tmp.path().join("spaced.md");
        fs::write(&req, "Need keep.rs\n").unwrap();
        let padded = format!("  {}  ", req.display());
        let (result, stdout) = run(
            &layout,
            &work,
            &config,
            ReviewOpts {
                range: "HEAD~1..HEAD".to_string(),
                json: true,
                requirements: vec![padded],
                id: None,
            },
        );
        assert!(result.is_ok(), "{result:?}\n{stdout}");
        let env = parse_env(&stdout);
        assert_eq!(env.promised_requirements.status, "ok");
        assert_eq!(env.promised_requirements.items.len(), 1);
        assert!(
            env.promised_requirements.items[0]
                .text
                .contains("Need keep.rs")
        );
    }

    #[test]
    fn review_conductor_clean_status_ok() {
        let (tmp, layout, work, mut config) = harness();
        let root = tmp.path().join("conductor");
        let track = root.join("0304-AgentReviewPacket");
        fs::create_dir_all(&track).unwrap();
        fs::write(track.join("spec.md"), "## Objective\n\nClean.\n").unwrap();
        config.review.conductor.root = root.to_string_lossy().into_owned();
        let (result, stdout) = run(
            &layout,
            &work,
            &config,
            ReviewOpts {
                range: "HEAD~1..HEAD".to_string(),
                json: true,
                requirements: Vec::new(),
                id: Some("0304".to_string()),
            },
        );
        assert!(result.is_ok(), "{result:?}\n{stdout}");
        let env = parse_env(&stdout);
        assert_eq!(env.unresolved_findings.status, "ok");
        assert!(env.unresolved_findings.items.is_empty());
        assert!(
            !stdout.contains("\"status\":\"none\"") || env.promised_requirements.status == "ok"
        );
    }

    #[test]
    fn review_conductor_skips_folded_and_ai_review_bundle() {
        let (tmp, layout, work, mut config) = harness();
        let root = tmp.path().join("conductor");
        let track = root.join("0304-AgentReviewPacket");
        fs::create_dir_all(&track).unwrap();
        fs::write(track.join("spec.md"), "## Objective\n\nShip.\n").unwrap();
        fs::write(
            track.join("AI-review.md"),
            "| agy-B-01 | Blocker | bundle dupe | open |\n",
        )
        .unwrap();
        fs::write(
            track.join("agy-review.md"),
            "| agy-M-01 | Major | historical | Agree — fold |\n",
        )
        .unwrap();
        fs::write(track.join("review.md"), "| P1 | open | real leftover |\n").unwrap();
        config.review.conductor.root = root.to_string_lossy().into_owned();
        let (result, stdout) = run(
            &layout,
            &work,
            &config,
            ReviewOpts {
                range: "HEAD~1..HEAD".to_string(),
                json: true,
                requirements: Vec::new(),
                id: Some("0304".to_string()),
            },
        );
        assert!(result.is_ok(), "{result:?}\n{stdout}");
        let env = parse_env(&stdout);
        assert_eq!(env.unresolved_findings.status, "ok");
        assert_eq!(env.unresolved_findings.items.len(), 1);
        assert!(
            env.unresolved_findings.items[0]
                .title
                .contains("real leftover"),
            "{:?}",
            env.unresolved_findings.items
        );
        assert!(
            !env.unresolved_findings
                .items
                .iter()
                .any(|i| i.source == "AI-review.md" || i.title.contains("Agree — fold")),
            "{:?}",
            env.unresolved_findings.items
        );
    }

    #[test]
    fn review_conductor_row_disclosed_stays_open() {
        assert!(conductor_finding_row_is_open(
            "| M-01 | Medium | open | disclosed later |"
        ));
    }

    #[test]
    fn review_conductor_row_unfolded_stays_open() {
        assert!(conductor_finding_row_is_open(
            "| M-01 | Medium | open | unfolded |"
        ));
    }

    #[test]
    fn review_conductor_row_closed_cell_is_closed() {
        assert!(!conductor_finding_row_is_open(
            "| M-01 | Medium | closed | done |"
        ));
        assert!(!conductor_finding_row_is_open(
            "| agy-M-01 | Major | historical | Agree — fold |"
        ));
        assert!(!conductor_finding_row_is_open(
            "| P1 | verified_fixed | leftover |"
        ));
        assert!(!conductor_finding_row_is_open(
            "| M-01 | Medium | open | Agree — fold |"
        ));
        assert!(!conductor_finding_row_is_open(
            "| - [ ] | Blocker | closed | done |"
        ));
        assert!(!conductor_finding_row_is_open(
            "| M-02 | Medium | declined | leftover |"
        ));
        assert!(!conductor_finding_row_is_open(
            "| M-03 | Medium | resolved | leftover |"
        ));
        assert!(!conductor_finding_row_is_open(
            "| M-04 | Medium | open | agree - fold |"
        ));
    }

    #[test]
    fn review_conductor_row_markdown_formatted_closed_is_closed() {
        assert!(!conductor_finding_row_is_open(
            "| R1-01 | medium | open | **verified_fixed** |"
        ));
        assert!(conductor_finding_row_is_open(
            "| R1-02 | medium | **open** | leftover |"
        ));
    }

    #[test]
    fn review_conductor_row_separator_and_unknown_status_dropped() {
        assert!(!conductor_finding_row_is_open("|---|---|"));
        assert!(!conductor_finding_row_is_open(
            "| M-01 | Medium | wontfix |"
        ));
        assert!(!conductor_finding_row_is_open("| ID | Sev | Status |"));
    }

    #[test]
    fn review_conductor_row_description_closed_stays_open() {
        assert!(conductor_finding_row_is_open(
            "| M-01 | Medium | open | Handle socket that was closed abruptly |"
        ));
    }

    #[test]
    fn review_github_omitted_id_not_attempted() {
        let (_tmp, layout, work, mut config) = harness();
        config.review.github.enabled = true;
        let (result, stdout) = run(
            &layout,
            &work,
            &config,
            ReviewOpts {
                range: "HEAD~1..HEAD".to_string(),
                json: true,
                requirements: Vec::new(),
                id: None,
            },
        );
        assert!(result.is_ok(), "{result:?}\n{stdout}");
        let env = parse_env(&stdout);
        assert!(
            !env.unresolved_findings
                .items
                .iter()
                .any(|i| i.source == "github"),
            "{:?}",
            env.unresolved_findings.items
        );
        assert!(
            !stdout.contains("must be a numeric pull number"),
            "{stdout}"
        );
    }

    #[test]
    fn review_github_slug_id_not_attempted_when_conductor_ok() {
        let (tmp, layout, work, mut config) = harness();
        let root = tmp.path().join("conductor");
        let track = root.join("0304-AgentReviewPacket");
        fs::create_dir_all(&track).unwrap();
        fs::write(track.join("spec.md"), "## Objective\n\nShip.\n").unwrap();
        config.review.conductor.root = root.to_string_lossy().into_owned();
        config.review.github.enabled = true;
        let (result, stdout) = run(
            &layout,
            &work,
            &config,
            ReviewOpts {
                range: "HEAD~1..HEAD".to_string(),
                json: true,
                requirements: Vec::new(),
                id: Some("0304-AgentReviewPacket".to_string()),
            },
        );
        assert!(result.is_ok(), "{result:?}\n{stdout}");
        let env = parse_env(&stdout);
        assert_eq!(
            env.coordinated.as_ref().map(|c| c.track_id.as_str()),
            Some("0304-AgentReviewPacket")
        );
        assert!(
            !env.unresolved_findings
                .items
                .iter()
                .any(|i| i.source == "github" || i.title.contains("must be a numeric")),
            "{:?}",
            env.unresolved_findings.items
        );
    }

    #[test]
    #[serial_test::serial(env)]
    fn review_github_leading_zero_id_skips_github_when_conductor_resolves() {
        let _token = TempEnv::set("GITHUB_TOKEN", "dummy-not-used");
        let _gh = TempEnv::remove("GH_TOKEN");
        let (tmp, layout, work, mut config) = harness();
        let root = tmp.path().join("conductor");
        let track = root.join("0304-AgentReviewPacket");
        fs::create_dir_all(&track).unwrap();
        fs::write(track.join("spec.md"), "## Objective\n\nShip.\n").unwrap();
        config.review.conductor.root = root.to_string_lossy().into_owned();
        config.review.github.enabled = true;
        config.review.github.repo.clear();
        config.review.ci.github_checks = true;
        assert_eq!("0304".parse::<u64>().ok(), Some(304));
        assert!(should_fetch_github_checks(&config.review));
        assert!(suppress_github_http(true, "ok"));
        let (result, stdout) = run(
            &layout,
            &work,
            &config,
            ReviewOpts {
                range: "HEAD~1..HEAD".to_string(),
                json: true,
                requirements: Vec::new(),
                id: Some("0304".to_string()),
            },
        );
        assert!(result.is_ok(), "{result:?}\n{stdout}");
        let env = parse_env(&stdout);
        assert_eq!(
            env.coordinated.as_ref().map(|c| c.track_id.as_str()),
            Some("0304-AgentReviewPacket")
        );
        assert!(
            !env.unresolved_findings
                .items
                .iter()
                .any(|i| { i.source == "github" || i.title.contains("github repo unparseable") }),
            "{:?}",
            env.unresolved_findings.items
        );
        assert!(
            !env.ci_evidence
                .items
                .iter()
                .any(|i| matches!(i, ReviewCiItem::GithubCheck { .. })),
            "{:?}",
            env.ci_evidence.items
        );
    }

    #[test]
    fn review_conductor_numeric_pr_miss_is_none_when_github_on() {
        let (tmp, layout, work, mut config) = harness();
        let root = tmp.path().join("conductor");
        fs::create_dir_all(&root).unwrap();
        config.review.conductor.root = root.to_string_lossy().into_owned();
        config.review.github.enabled = true;
        let (result, stdout) = run(
            &layout,
            &work,
            &config,
            ReviewOpts {
                range: "HEAD~1..HEAD".to_string(),
                json: true,
                requirements: Vec::new(),
                id: Some("323".to_string()),
            },
        );
        assert!(result.is_ok(), "{result:?}\n{stdout}");
        let env = parse_env(&stdout);
        assert!(env.coordinated.is_none());
        assert!(
            !env.unresolved_findings
                .items
                .iter()
                .any(|i| i.source == "conductor"),
            "{:?}",
            env.unresolved_findings.items
        );
    }

    #[test]
    fn review_conductor_numeric_miss_unavailable_when_github_off() {
        let (tmp, layout, work, mut config) = harness();
        let root = tmp.path().join("conductor");
        fs::create_dir_all(&root).unwrap();
        config.review.conductor.root = root.to_string_lossy().into_owned();
        config.review.github.enabled = false;
        let (result, stdout) = run(
            &layout,
            &work,
            &config,
            ReviewOpts {
                range: "HEAD~1..HEAD".to_string(),
                json: true,
                requirements: Vec::new(),
                id: Some("323".to_string()),
            },
        );
        assert!(result.is_ok(), "{result:?}\n{stdout}");
        let env = parse_env(&stdout);
        assert_eq!(env.unresolved_findings.status, "unavailable");
        assert!(
            env.unresolved_findings
                .items
                .iter()
                .any(|i| i.source == "conductor" && i.severity.as_deref() == Some("unavailable")),
            "{:?}",
            env.unresolved_findings.items
        );
    }

    #[test]
    fn review_github_slug_id_not_attempted_when_conductor_misses() {
        let (tmp, layout, work, mut config) = harness();
        let root = tmp.path().join("conductor");
        fs::create_dir_all(&root).unwrap();
        config.review.conductor.root = root.to_string_lossy().into_owned();
        config.review.github.enabled = true;
        let (result, stdout) = run(
            &layout,
            &work,
            &config,
            ReviewOpts {
                range: "HEAD~1..HEAD".to_string(),
                json: true,
                requirements: Vec::new(),
                id: Some("0999-NoSuch".to_string()),
            },
        );
        assert!(result.is_ok(), "{result:?}\n{stdout}");
        let env = parse_env(&stdout);
        assert!(env.coordinated.is_none());
        assert_eq!(env.unresolved_findings.status, "unavailable");
        assert!(
            env.unresolved_findings
                .items
                .iter()
                .any(|i| i.source == "conductor" && i.title.contains("no conductor track matching")),
            "{:?}",
            env.unresolved_findings.items
        );
        assert!(
            !env.unresolved_findings
                .items
                .iter()
                .any(|i| i.source == "github" || i.title.contains("must be a numeric")),
            "{:?}",
            env.unresolved_findings.items
        );
        assert!(
            !stdout.contains("must be a numeric pull number"),
            "{stdout}"
        );
    }

    #[test]
    #[serial_test::serial(env)]
    fn review_conductor_ambiguous_prefix_is_unavailable_even_when_github_on() {
        let _token = TempEnv::set("GITHUB_TOKEN", "dummy-not-used");
        let _gh = TempEnv::remove("GH_TOKEN");
        let (tmp, layout, work, mut config) = harness();
        let root = tmp.path().join("conductor");
        let first = root.join("0304-AgentReviewPacket");
        let second = root.join("0304-OtherTrack");
        fs::create_dir_all(&first).unwrap();
        fs::create_dir_all(&second).unwrap();
        fs::write(first.join("spec.md"), "## Objective\n\nShip.\n").unwrap();
        fs::write(second.join("spec.md"), "## Objective\n\nOther.\n").unwrap();
        config.review.conductor.root = root.to_string_lossy().into_owned();
        config.review.github.enabled = true;
        config.review.github.repo.clear();
        config.review.ci.github_checks = true;
        assert!(should_fetch_github_checks(&config.review));
        assert!(suppress_github_http(false, "unavailable"));
        let (result, stdout) = run(
            &layout,
            &work,
            &config,
            ReviewOpts {
                range: "HEAD~1..HEAD".to_string(),
                json: true,
                requirements: Vec::new(),
                id: Some("0304".to_string()),
            },
        );
        assert!(result.is_ok(), "{result:?}\n{stdout}");
        let env = parse_env(&stdout);
        assert!(env.coordinated.is_none());
        assert_eq!(env.unresolved_findings.status, "unavailable");
        assert!(
            env.unresolved_findings.items.iter().any(|i| {
                i.source == "conductor"
                    && i.severity.as_deref() == Some("unavailable")
                    && i.title.contains("ambiguous conductor track prefix")
            }),
            "{:?}",
            env.unresolved_findings.items
        );
        assert!(
            !env.unresolved_findings
                .items
                .iter()
                .any(|i| { i.source == "github" || i.title.contains("github repo unparseable") }),
            "{:?}",
            env.unresolved_findings.items
        );
        assert!(
            !env.ci_evidence
                .items
                .iter()
                .any(|i| matches!(i, ReviewCiItem::GithubCheck { .. })),
            "{:?}",
            env.ci_evidence.items
        );
    }

    #[test]
    fn review_github_http_suppressed_for_resolved_or_unavailable() {
        assert!(suppress_github_http(true, "ok"));
        assert!(suppress_github_http(true, "none"));
        assert!(suppress_github_http(false, "unavailable"));
        assert!(!suppress_github_http(false, "none"));
        assert!(!suppress_github_http(false, "ok"));
    }

    #[test]
    fn review_ci_github_checks_ignored_when_github_disabled() {
        let (tmp, layout, work, mut config) = harness();
        let root = tmp.path().join("conductor");
        fs::create_dir_all(&root).unwrap();
        config.review.conductor.root = root.to_string_lossy().into_owned();
        config.review.github.enabled = false;
        config.review.github.repo = "owner/repo".to_string();
        config.review.ci.github_checks = true;
        assert!(!should_fetch_github_checks(&config.review));
        fs::create_dir_all(layout.reports_dir().as_std_path()).unwrap();
        fs::write(
            layout.reports_dir().join(VERIFY_HISTORY).as_std_path(),
            r#"[{"timestamp":"2026-09-09T00:00:00Z","passed":true,"duration_secs":3,"tx_id":"abc"}]"#,
        )
        .unwrap();
        let (result, stdout) = run(
            &layout,
            &work,
            &config,
            ReviewOpts {
                range: "HEAD~1..HEAD".to_string(),
                json: true,
                requirements: Vec::new(),
                id: Some("1".to_string()),
            },
        );
        assert!(result.is_ok(), "{result:?}\n{stdout}");
        let env = parse_env(&stdout);
        assert_eq!(env.ci_evidence.status, "ok");
        assert!(
            env.ci_evidence
                .items
                .iter()
                .any(|i| matches!(i, ReviewCiItem::VerifyHistory { passed: true, .. }))
        );
        assert!(
            !env.ci_evidence
                .items
                .iter()
                .any(|i| matches!(i, ReviewCiItem::GithubCheck { .. })),
            "{:?}",
            env.ci_evidence.items
        );
    }

    #[test]
    #[serial_test::serial(env)]
    fn review_ci_github_checks_skipped_when_no_token() {
        let _token = TempEnv::remove("GITHUB_TOKEN");
        let _gh = TempEnv::remove("GH_TOKEN");
        let (_tmp, layout, work, mut config) = harness();
        config.review.github.enabled = true;
        config.review.github.repo = "owner/repo".to_string();
        config.review.ci.github_checks = true;
        assert!(should_fetch_github_checks(&config.review));
        fs::create_dir_all(layout.reports_dir().as_std_path()).unwrap();
        fs::write(
            layout.reports_dir().join(VERIFY_HISTORY).as_std_path(),
            r#"[{"timestamp":"2026-09-09T00:00:00Z","passed":true,"duration_secs":3,"tx_id":"abc"}]"#,
        )
        .unwrap();
        let (result, stdout) = run(
            &layout,
            &work,
            &config,
            ReviewOpts {
                range: "HEAD~1..HEAD".to_string(),
                json: true,
                requirements: Vec::new(),
                id: Some("1".to_string()),
            },
        );
        assert!(result.is_ok(), "{result:?}\n{stdout}");
        let env = parse_env(&stdout);
        assert_eq!(env.ci_evidence.status, "ok");
        assert!(
            env.ci_evidence
                .items
                .iter()
                .any(|i| matches!(i, ReviewCiItem::VerifyHistory { passed: true, .. }))
        );
        assert!(
            !env.ci_evidence
                .items
                .iter()
                .any(|i| matches!(i, ReviewCiItem::GithubCheck { .. })),
            "{:?}",
            env.ci_evidence.items
        );
    }

    #[test]
    fn review_blast_ok_with_seeded_ledger() {
        let (_tmp, layout, work, config) = harness();
        {
            StorageManager::init(layout.state_subdir().join("ledger.db").as_std_path()).unwrap();
        }
        let (result, stdout) = run(
            &layout,
            &work,
            &config,
            ReviewOpts {
                range: "HEAD~1..HEAD".to_string(),
                json: true,
                requirements: Vec::new(),
                id: None,
            },
        );
        assert!(result.is_ok(), "{result:?}\n{stdout}");
        let env = parse_env(&stdout);
        assert_eq!(env.blast.status, "ok", "{stdout}");
        assert_eq!(env.blast.files_total, env.files_changed.len());
        assert!(!env.blast.files_capped);
        assert_eq!(env.ci_evidence.status, "unavailable");
        assert!(!stdout.contains("\"filesChangedTruncated\""));
    }

    #[test]
    fn review_range_head_not_head_blast_follows_range() {
        let (tmp, layout, work, config) = harness();
        {
            StorageManager::init(layout.state_subdir().join("ledger.db").as_std_path()).unwrap();
        }
        fs::write(tmp.path().join("mid.rs"), "pub fn mid_only_fn() {}\n").unwrap();
        assert!(git(tmp.path(), &["add", "mid.rs"]).status.success());
        assert!(git(tmp.path(), &["commit", "-m", "mid"]).status.success());
        let mid = String::from_utf8_lossy(&git(tmp.path(), &["rev-parse", "HEAD"]).stdout)
            .trim()
            .to_string();
        fs::write(tmp.path().join("later.rs"), "pub fn later_only_fn() {}\n").unwrap();
        assert!(git(tmp.path(), &["add", "later.rs"]).status.success());
        assert!(git(tmp.path(), &["commit", "-m", "later"]).status.success());
        let first = String::from_utf8_lossy(&git(tmp.path(), &["rev-parse", "HEAD~2"]).stdout)
            .trim()
            .to_string();
        let (result, stdout) = run(
            &layout,
            &work,
            &config,
            ReviewOpts {
                range: format!("{first}..{mid}"),
                json: true,
                requirements: Vec::new(),
                id: None,
            },
        );
        assert!(result.is_ok(), "{result:?}\n{stdout}");
        let env = parse_env(&stdout);
        let paths: Vec<&str> = env.files_changed.iter().map(|f| f.path.as_str()).collect();
        assert!(paths.iter().any(|p| p.ends_with("mid.rs")), "{paths:?}");
        assert!(!paths.iter().any(|p| p.ends_with("later.rs")), "{paths:?}");
        assert_eq!(env.blast.status, "ok", "{stdout}");
        assert!(
            env.affected_symbols
                .iter()
                .any(|s| s.contains("mid_only_fn")),
            "expected mid_only_fn in {:?}",
            env.affected_symbols
        );
        assert!(
            !env.affected_symbols
                .iter()
                .any(|s| s.contains("later_only_fn")),
            "later.rs must not contribute symbols: {:?}",
            env.affected_symbols
        );
    }

    #[test]
    fn review_truncation_flags_emit_when_capped() {
        let (tmp, layout, work, config) = harness();
        let mut reqs = Vec::new();
        for i in 0..9 {
            let p = tmp.path().join(format!("req{i}.md"));
            fs::write(&p, format!("requirement {i}\n")).unwrap();
            reqs.push(p.to_string_lossy().into_owned());
        }
        let (result, stdout) = run(
            &layout,
            &work,
            &config,
            ReviewOpts {
                range: "HEAD~1..HEAD".to_string(),
                json: true,
                requirements: reqs,
                id: None,
            },
        );
        assert!(result.is_ok(), "{result:?}\n{stdout}");
        assert!(stdout.contains("requirementsFilesTruncated"), "{stdout}");
        let env = parse_env(&stdout);
        assert!(env.requirements_files_truncated);
        assert!(env.promised_requirements.items.len() <= 8);

        let quiet = ReviewEnvelope {
            schema_version: 1,
            kind: "review".into(),
            analysis_mode: "range".into(),
            range: "HEAD~1...HEAD".into(),
            base_ref: "HEAD~1".into(),
            head_ref: "HEAD".into(),
            files_changed: vec![],
            files_changed_truncated: false,
            blast: ReviewBlast {
                status: "unavailable".into(),
                files_scanned: 0,
                files_total: 0,
                files_capped: false,
                nodes: 0,
                edges: 0,
                reason: None,
            },
            affected_symbols: vec![],
            tests: ReviewTests {
                status: "unavailable".into(),
                exercising: vec![],
                untested: vec![],
                notes: None,
            },
            contracts: ReviewContracts {
                status: "unavailable".into(),
                items: vec![],
                truncated: false,
            },
            promised_requirements: ReviewRequirements {
                status: "none".into(),
                items: vec![],
                truncated: false,
            },
            requirements_files_truncated: false,
            files_intended: None,
            files_unexpected: None,
            files_intended_truncated: false,
            files_unexpected_truncated: false,
            prior_decisions: vec![],
            prior_decisions_truncated: false,
            implementation_claims: vec![],
            implementation_claims_truncated: false,
            unresolved_findings: ReviewFindings {
                status: "none".into(),
                items: vec![],
                truncated: false,
            },
            ci_evidence: ReviewCiEvidence {
                status: "unavailable".into(),
                items: vec![],
                truncated: false,
            },
            coordinated: None,
        };
        let omitted = serde_json::to_string(&quiet).expect("json");
        assert!(
            !omitted.contains("Truncated"),
            "false *Truncated must omit: {omitted}"
        );
    }
}
