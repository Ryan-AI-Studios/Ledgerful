//! Compose the session briefing (in-memory impact; no latest-impact rewrite).

use super::packet::*;
use crate::commands::change_context::{
    ChangeContextOpts, ChangeContextPacket, DoctorSection, build_change_context,
};
use crate::config::checklist::build_config_checklist;
use crate::config::model::Config;
use crate::git::repo::{get_head_info, open_repo};
use crate::git::status::get_repo_status;
use crate::impact::budget::{
    AnalysisBudget, CompletenessFilter, HistoryWalkStop, completeness_for_error,
    completeness_for_overall, completeness_for_walk, poll_overall_stop,
    resolve_hotspots_overall_budget_secs,
};
use crate::impact::hotspots::{HotspotQuery, calculate_hotspots_detailed};
use crate::impact::temporal::GixHistoryProvider;
use crate::ledger::Transaction;
use crate::ledger::db::LedgerDb;
use crate::ledger::find_start_collisions;
use crate::state::cli_session::{CliSession, env_session_id};
use crate::state::layout::Layout;
use crate::state::reports::read_latest_impact_report;
use crate::state::storage::StorageManager;
use chrono::Utc;
use miette::Result;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

/// Build a session envelope for the current layout/storage/config.
///
/// Reuses [`build_change_context`] with [`SESSION_MAX_FILES`]. Never writes
/// `latest-impact.json`.
pub fn build_session(
    layout: &Layout,
    storage: &StorageManager,
    config: &Config,
) -> Result<SessionEnvelope> {
    let project_root = layout.root.as_std_path();
    let work_root = layout.root.to_string();

    let (branch, head, dirty_paths_all, git_warnings) = collect_git(project_root, config);
    let (dirty_paths, dirty_count) = cap_dirty_paths(dirty_paths_all.clone());

    let opts = ChangeContextOpts {
        max_files: SESSION_MAX_FILES,
        skip_git_history_enrichment: true,
        ..ChangeContextOpts::default()
    };
    let cc = build_change_context(&opts, layout, storage, config)?;

    let doctor = session_doctor(&cc.doctor);
    let (pending, unaudited_drift, mut extra_warnings) = read_ledger_rows(storage);
    extra_warnings.extend(git_warnings);

    let collisions: Vec<SessionCollision> = find_start_collisions(&pending, "", &dirty_paths_all)
        .iter()
        .map(SessionCollision::from)
        .collect();

    let mut pending_entries: Vec<SessionPendingTx> = pending
        .iter()
        .map(|tx| SessionPendingTx {
            tx_id: tx.tx_id.clone(),
            entity: tx.entity.clone(),
            category: tx.category.to_string(),
        })
        .collect();
    pending_entries.sort_by(|a, b| a.tx_id.cmp(&b.tx_id));

    let live_head = if head.is_empty() {
        None
    } else {
        Some(head.as_str())
    };
    let mut cache_warning = None;
    let impact_cache = match read_latest_impact_report(layout) {
        Ok(report) => classify_impact_cache(report.as_ref(), live_head),
        Err(e) => {
            cache_warning = Some(format!("impactCache unreadable: {e}"));
            SessionImpactCache {
                present: false,
                valid_for_head: false,
                tree_clean: false,
            }
        }
    };

    let (hotspot_files, hotspot_warning, hotspot_completeness, hotspot_provenance) =
        collect_hotspots(storage, config, project_root);

    let change_context = SessionChangeContext {
        status: cc.status.clone(),
        risk_level: cc.risk_level.clone().unwrap_or_default(),
        read_set_capped: cc.read_set_capped,
        read_set_total_candidates: cc.read_set_total_candidates,
        read_set: cc.read_set.clone(),
    };

    let next = compose_session_next(
        &cc,
        &impact_cache,
        collisions.len(),
        hotspot_warning.as_deref(),
        &extra_warnings,
        cache_warning.as_deref(),
    );

    let cli_session = CliSession::load(layout, env_session_id().as_deref(), Utc::now());
    let config_checklist = match build_config_checklist(layout, storage, config, &cli_session) {
        Ok(items) => items,
        Err(e) => {
            tracing::debug!(error = %e, "session: config checklist omitted");
            Vec::new()
        }
    };

    Ok(SessionEnvelope {
        schema_version: SESSION_SCHEMA_VERSION,
        kind: SESSION_KIND.to_string(),
        git: SessionGit {
            branch,
            head,
            dirty_count,
            dirty_paths,
        },
        ledger: SessionLedger {
            work_root,
            pending_count: pending_entries.len(),
            pending: pending_entries,
            unaudited_drift,
            collisions,
        },
        doctor,
        change_context,
        hotspots: SessionHotspots {
            files: hotspot_files,
            excluded_tests: true,
            completeness: hotspot_completeness,
            provenance: hotspot_provenance,
        },
        impact_cache,
        next,
        config_checklist,
    })
}

fn session_doctor(section: &DoctorSection) -> SessionDoctor {
    SessionDoctor {
        ready_for_publish: section.ready_for_publish,
        block: section.block,
        warn: section.warn,
        info: section.info,
    }
}

fn collect_git(project_root: &Path, config: &Config) -> (String, String, Vec<String>, Vec<String>) {
    let mut warnings = Vec::new();
    let repo = match open_repo(project_root) {
        Ok(r) => r,
        Err(e) => {
            warnings.push(format!("git repository unavailable: {e}"));
            return (String::new(), String::new(), Vec::new(), warnings);
        }
    };
    let (head, branch) = match get_head_info(&repo) {
        Ok((hash, name)) => (hash.unwrap_or_default(), name.unwrap_or_default()),
        Err(e) => {
            warnings.push(format!("git HEAD unavailable: {e}"));
            (String::new(), String::new())
        }
    };
    let dirty = match get_repo_status(&repo) {
        Ok(changes) => {
            let changes = match crate::git::ignore::filter_ignored_changes(
                changes.clone(),
                &config.watch.ignore_patterns,
                true,
            ) {
                Ok(filtered) => filtered,
                Err(e) => {
                    warnings.push(format!("git ignore filter failed: {e}"));
                    changes
                }
            };
            let mut paths: Vec<String> = changes
                .into_iter()
                .map(|c| c.path.to_string_lossy().replace('\\', "/"))
                .collect();
            paths.sort();
            paths.dedup();
            paths
        }
        Err(e) => {
            warnings.push(format!("git status unavailable: {e}"));
            Vec::new()
        }
    };
    (branch, head, dirty, warnings)
}

fn read_ledger_rows(storage: &StorageManager) -> (Vec<Transaction>, usize, Vec<String>) {
    let mut warnings = Vec::new();
    let db = LedgerDb::new(storage.get_connection());
    let pending = match db.get_all_pending() {
        Ok(rows) => rows,
        Err(e) => {
            warnings.push(format!(
                "ledger pending read failed; pendingCount may be incomplete: {e}"
            ));
            Vec::new()
        }
    };
    let unaudited_drift = match db.get_all_unaudited() {
        Ok(rows) => rows.len(),
        Err(e) => {
            warnings.push(format!(
                "ledger unaudited read failed; unauditedDrift may be incomplete: {e}"
            ));
            0
        }
    };
    (pending, unaudited_drift, warnings)
}

fn collect_hotspots(
    storage: &StorageManager,
    config: &Config,
    project_root: &Path,
) -> (
    Vec<SessionHotspotFile>,
    Option<String>,
    Option<crate::impact::budget::AnalysisCompleteness>,
    crate::impact::budget::HotspotProvenance,
) {
    let commits = config.hotspots.max_commits.min(SESSION_HOTSPOT_COMMITS_CAP) as u64;
    let repo = match open_repo(project_root) {
        Ok(r) => r,
        Err(e) => {
            return (
                Vec::new(),
                Some(format!(
                    "hotspots unavailable: git repository not openable: {e}"
                )),
                Some(completeness_for_error(
                    commits,
                    Some(SESSION_HOTSPOT_DAYS),
                    CompletenessFilter::Session,
                    Some(config.hotspots.history_budget_secs).filter(|s| *s > 0),
                )),
                session_hotspots_provenance(commits, None),
            );
        }
    };
    let cancel = Arc::new(AtomicBool::new(false));
    let overall_secs =
        resolve_hotspots_overall_budget_secs(None, config.hotspots.overall_budget_secs);
    let overall_deadline = session_hotspots_overall_deadline(overall_secs);
    let query = HotspotQuery {
        commits: commits as usize,
        days: Some(SESSION_HOTSPOT_DAYS),
        limit: SESSION_HOTSPOT_LIMIT,
        decay_half_life: config.hotspots.decay_half_life,
        exclude_test_paths: true,
        exclude_vendor_paths: true,
        budget: Some(AnalysisBudget::capped_by_overall(
            config.hotspots.history_budget_secs,
            overall_deadline,
            Arc::clone(&cancel),
        )),
        skip_unindexed_complexity_fallback: overall_deadline.is_some(),
        ..HotspotQuery::default()
    };
    let provider = GixHistoryProvider::new(&repo);
    match calculate_hotspots_detailed(storage, &provider, &query) {
        Ok(calc) => {
            let files = calc
                .hotspots
                .iter()
                .take(SESSION_HOTSPOT_LIMIT)
                .map(|h| session_hotspot_file_with_head(h, &repo))
                .collect();
            let completeness = session_hotspot_completeness(
                SessionWalkCompleteness {
                    walk_stop: calc.walk_stop,
                    commits_requested: commits,
                    commits_walked: calc.commits_walked as u64,
                    head: calc.head.clone(),
                    history_budget_secs: config.hotspots.history_budget_secs,
                },
                overall_deadline,
                overall_secs,
                &cancel,
            );
            (
                files,
                None,
                completeness,
                session_hotspots_provenance(commits, calc.head),
            )
        }
        Err(e) => (
            Vec::new(),
            Some(format!("hotspots unavailable: {e}")),
            Some(completeness_for_error(
                commits,
                Some(SESSION_HOTSPOT_DAYS),
                CompletenessFilter::Session,
                Some(config.hotspots.history_budget_secs).filter(|s| *s > 0),
            )),
            session_hotspots_provenance(commits, None),
        ),
    }
}

pub(crate) fn session_hotspots_overall_deadline(overall_secs: u64) -> Option<Instant> {
    (overall_secs > 0).then(|| Instant::now() + Duration::from_secs(overall_secs))
}

pub(crate) struct SessionWalkCompleteness {
    pub walk_stop: HistoryWalkStop,
    pub commits_requested: u64,
    pub commits_walked: u64,
    pub head: Option<String>,
    pub history_budget_secs: u64,
}

pub(crate) fn session_hotspot_completeness(
    walk: SessionWalkCompleteness,
    overall_deadline: Option<Instant>,
    overall_secs: u64,
    cancel: &AtomicBool,
) -> Option<crate::impact::budget::AnalysisCompleteness> {
    if let Some(stop) = poll_overall_stop(overall_deadline, cancel) {
        return Some(completeness_for_overall(
            stop,
            Some(overall_secs).filter(|s| *s > 0),
            "hotspots",
        ));
    }
    completeness_for_walk(
        walk.walk_stop,
        walk.commits_requested,
        walk.commits_walked,
        Some(SESSION_HOTSPOT_DAYS),
        CompletenessFilter::Session,
        walk.head,
        Some(walk.history_budget_secs).filter(|s| *s > 0),
    )
}

fn compose_session_next(
    cc: &ChangeContextPacket,
    impact_cache: &SessionImpactCache,
    collision_count: usize,
    hotspot_warning: Option<&str>,
    ledger_warnings: &[String],
    cache_warning: Option<&str>,
) -> Vec<String> {
    let mut next = cc.next_actions.clone();
    if impact_cache.present && !impact_cache.valid_for_head {
        next.push("do not read latest-impact.json (validForHead=false)".to_string());
    }
    if collision_count > 0 {
        next.push("resolve pending entity collision before ledger start".to_string());
    }
    if let Some(w) = hotspot_warning {
        next.push(w.to_string());
    }
    if let Some(w) = cache_warning {
        next.push(w.to_string());
    }
    next.extend(ledger_warnings.iter().cloned());
    next.sort();
    next.dedup();
    next
}
