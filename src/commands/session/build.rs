//! Compose the session briefing (in-memory impact; no latest-impact rewrite).

use super::packet::*;
use crate::commands::change_context::{
    ChangeContextOpts, ChangeContextPacket, DoctorSection, build_change_context,
};
use crate::config::model::Config;
use crate::git::repo::{get_head_info, open_repo};
use crate::git::status::get_repo_status;
use crate::impact::hotspots::{HotspotQuery, calculate_hotspots};
use crate::impact::temporal::GixHistoryProvider;
use crate::ledger::Transaction;
use crate::ledger::db::LedgerDb;
use crate::ledger::find_start_collisions;
use crate::state::layout::Layout;
use crate::state::reports::read_latest_impact_report;
use crate::state::storage::StorageManager;
use miette::Result;
use std::path::Path;

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

    let (branch, head, dirty_paths_all, git_warnings) = collect_git(project_root);
    let (dirty_paths, dirty_count) = cap_dirty_paths(dirty_paths_all.clone());

    let opts = ChangeContextOpts {
        max_files: SESSION_MAX_FILES,
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

    let (hotspot_files, hotspot_warning) = collect_hotspots(storage, config, project_root);

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
        },
        impact_cache,
        next,
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

fn collect_git(project_root: &Path) -> (String, String, Vec<String>, Vec<String>) {
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
) -> (Vec<SessionHotspotFile>, Option<String>) {
    let repo = match open_repo(project_root) {
        Ok(r) => r,
        Err(e) => {
            return (
                Vec::new(),
                Some(format!(
                    "hotspots unavailable: git repository not openable: {e}"
                )),
            );
        }
    };
    let commits = config.hotspots.max_commits.min(SESSION_HOTSPOT_COMMITS_CAP);
    let query = HotspotQuery {
        commits,
        days: Some(SESSION_HOTSPOT_DAYS),
        limit: SESSION_HOTSPOT_LIMIT,
        decay_half_life: config.hotspots.decay_half_life,
        exclude_test_paths: true,
        ..HotspotQuery::default()
    };
    let provider = GixHistoryProvider::new(&repo);
    match calculate_hotspots(storage, &provider, &query) {
        Ok(hotspots) => {
            let files = hotspots
                .into_iter()
                .take(SESSION_HOTSPOT_LIMIT)
                .map(|h| SessionHotspotFile {
                    path: h.path.to_string_lossy().replace('\\', "/"),
                    score: h.score,
                })
                .collect();
            (files, None)
        }
        Err(e) => (Vec::new(), Some(format!("hotspots unavailable: {e}"))),
    }
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
