//! Session briefing envelope (track 0224).

use crate::commands::change_context::ReadSetEntry;
use crate::ledger::pending_entity_overlap::CollisionHit;
use crate::state::reports::LatestImpactReport;
use serde::{Deserialize, Serialize};

/// Session envelope schema version (doctor/verify style u32).
pub const SESSION_SCHEMA_VERSION: u32 = 1;

/// Top-level `kind` discriminator (not present on change-context).
pub const SESSION_KIND: &str = "session";

/// Briefing `readSet` budget — passed to `build_change_context`, not post-sliced.
pub const SESSION_MAX_FILES: usize = 5;

/// Cap for `git.dirtyPaths`; `dirtyCount` is always the true total.
pub const SESSION_DIRTY_PATH_CAP: usize = 5;

/// In-session hotspot list size.
pub const SESSION_HOTSPOT_LIMIT: usize = 5;

/// Upper bound on hotspot git walk (`min(config.hotspots.max_commits, this)`).
pub const SESSION_HOTSPOT_COMMITS_CAP: usize = 50;

/// Hotspot recency window (days) unless a cheaper bound is measured.
pub const SESSION_HOTSPOT_DAYS: u64 = 30;

/// Canonical agent session briefing (schemaVersion 1, kind `session`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SessionEnvelope {
    pub schema_version: u32,
    pub kind: String,
    pub git: SessionGit,
    pub ledger: SessionLedger,
    pub doctor: SessionDoctor,
    pub change_context: SessionChangeContext,
    pub hotspots: SessionHotspots,
    pub impact_cache: SessionImpactCache,
    pub next: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SessionGit {
    pub branch: String,
    pub head: String,
    pub dirty_count: usize,
    pub dirty_paths: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SessionLedger {
    pub work_root: String,
    pub pending_count: usize,
    pub pending: Vec<SessionPendingTx>,
    pub unaudited_drift: usize,
    pub collisions: Vec<SessionCollision>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SessionPendingTx {
    pub tx_id: String,
    pub entity: String,
    pub category: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SessionCollision {
    pub tx_id: String,
    pub entity: String,
    pub category: String,
    pub message: String,
    pub overlapping_paths: Vec<String>,
}

impl From<&CollisionHit> for SessionCollision {
    fn from(hit: &CollisionHit) -> Self {
        Self {
            tx_id: hit.tx_id.clone(),
            entity: hit.entity.clone(),
            category: hit.category.clone(),
            message: hit.message.clone(),
            overlapping_paths: hit.overlapping_paths.clone(),
        }
    }
}

/// Doctor counts only — no `warnAction` (0209 lives on doctor summary).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SessionDoctor {
    pub ready_for_publish: bool,
    pub block: u64,
    pub warn: u64,
    pub info: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SessionChangeContext {
    pub status: String,
    pub risk_level: String,
    pub read_set_capped: bool,
    pub read_set_total_candidates: usize,
    pub read_set: Vec<ReadSetEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SessionHotspots {
    pub files: Vec<SessionHotspotFile>,
    pub excluded_tests: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SessionHotspotFile {
    pub path: String,
    pub score: f32,
}

/// HEAD-validity of on-disk `latest-impact.json` (not `reports.rs` shape alone).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SessionImpactCache {
    pub present: bool,
    pub valid_for_head: bool,
    pub tree_clean: bool,
}

/// Classify on-disk latest-impact against live HEAD SHA.
///
/// `reports.rs` only distinguishes Packet / CleanTree / missing. This comparator
/// is the briefing's honesty layer.
pub fn classify_impact_cache(
    report: Option<&LatestImpactReport>,
    live_head: Option<&str>,
) -> SessionImpactCache {
    match report {
        None => SessionImpactCache {
            present: false,
            valid_for_head: false,
            tree_clean: false,
        },
        Some(LatestImpactReport::Packet(packet)) => SessionImpactCache {
            present: true,
            valid_for_head: head_matches(packet.head_hash.as_deref(), live_head),
            tree_clean: false,
        },
        Some(LatestImpactReport::CleanTree(tombstone)) => SessionImpactCache {
            present: true,
            valid_for_head: head_matches(tombstone.head_hash.as_deref(), live_head),
            tree_clean: true,
        },
    }
}

fn head_matches(cached: Option<&str>, live: Option<&str>) -> bool {
    match (cached, live) {
        (Some(cached), Some(live)) => cached == live,
        _ => false,
    }
}

/// Cap dirty paths at [`SESSION_DIRTY_PATH_CAP`]; return (capped paths, true count).
pub fn cap_dirty_paths(mut paths: Vec<String>) -> (Vec<String>, usize) {
    paths.sort();
    paths.dedup();
    let dirty_count = paths.len();
    paths.truncate(SESSION_DIRTY_PATH_CAP);
    (paths, dirty_count)
}

impl Default for SessionEnvelope {
    fn default() -> Self {
        Self {
            schema_version: SESSION_SCHEMA_VERSION,
            kind: SESSION_KIND.to_string(),
            git: SessionGit {
                branch: String::new(),
                head: String::new(),
                dirty_count: 0,
                dirty_paths: Vec::new(),
            },
            ledger: SessionLedger {
                work_root: String::new(),
                pending_count: 0,
                pending: Vec::new(),
                unaudited_drift: 0,
                collisions: Vec::new(),
            },
            doctor: SessionDoctor {
                ready_for_publish: false,
                block: 0,
                warn: 0,
                info: 0,
            },
            change_context: SessionChangeContext {
                status: "not_ready".to_string(),
                risk_level: String::new(),
                read_set_capped: false,
                read_set_total_candidates: 0,
                read_set: Vec::new(),
            },
            hotspots: SessionHotspots {
                files: Vec::new(),
                excluded_tests: true,
            },
            impact_cache: SessionImpactCache {
                present: false,
                valid_for_head: false,
                tree_clean: false,
            },
            next: Vec::new(),
        }
    }
}
