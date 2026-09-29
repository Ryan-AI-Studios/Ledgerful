//! Recovery adoption decision.
//!
//! The stored manifest matches the live rows and the maintenance
//! signature. The recovery command stays in the command layer.

pub(crate) mod classify;
pub(crate) mod manifest;

use self::manifest::{
    HISTORICAL_CONTINUITY, RecoveryManifest, build_manifest, diagnose, eligible_rows, from_json,
};
use crate::ledger::crypto::verify_ledger_entry_signature;
use crate::ledger::types::{ChainHead, LedgerEntry};
use miette::{Result, miette};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdoptionDecision {
    pub accepted: bool,
    pub adopted_count: usize,
    pub unresolved_count: usize,
    pub manifest_digest: String,
    pub message: String,
}

pub fn decide_adoption(
    conn: &rusqlite::Connection,
    entries: &[LedgerEntry],
    head: Option<&ChainHead>,
) -> AdoptionDecision {
    let stored = match load_manifest_row(conn) {
        Ok(Some(row)) => row,
        Ok(None) => {
            return AdoptionDecision {
                accepted: false,
                adopted_count: 0,
                unresolved_count: 1,
                manifest_digest: String::new(),
                message: "no adoption manifest is stored".to_string(),
            };
        }
        Err(err) => {
            return AdoptionDecision {
                accepted: false,
                adopted_count: 0,
                unresolved_count: 1,
                manifest_digest: String::new(),
                message: err.to_string(),
            };
        }
    };
    let diagnosis = diagnose(entries, head);
    let unresolved = diagnosis
        .candidate_tx_ids
        .iter()
        .filter(|id| stored.adopted.iter().all(|row| row.tx_id != **id))
        .count();
    let live = match eligible_rows(&diagnosis) {
        Ok(rows) => rows,
        Err(err) => {
            return AdoptionDecision {
                accepted: false,
                adopted_count: stored.adopted.len(),
                unresolved_count: unresolved.max(diagnosis.extra_genesis_count).max(1),
                manifest_digest: stored.manifest_digest,
                message: err.to_string(),
            };
        }
    };
    let live_manifest = match build_manifest(&stored.repo_identity, &stored.stored_head, live) {
        Ok(manifest) => manifest,
        Err(err) => {
            return AdoptionDecision {
                accepted: false,
                adopted_count: stored.adopted.len(),
                unresolved_count: unresolved.max(1),
                manifest_digest: stored.manifest_digest,
                message: err.to_string(),
            };
        }
    };
    if live_manifest.manifest_digest != stored.manifest_digest {
        return AdoptionDecision {
            accepted: false,
            adopted_count: stored.adopted.len(),
            unresolved_count: unresolved.max(1),
            manifest_digest: stored.manifest_digest,
            message: "stored adoption manifest does not match the current rows".to_string(),
        };
    }
    let tx_id = adoption_tx_id(&stored.manifest_digest);
    let maintenance = entries.iter().find(|entry| entry.tx_id == tx_id);
    let Some(maintenance) = maintenance else {
        return AdoptionDecision {
            accepted: false,
            adopted_count: stored.adopted.len(),
            unresolved_count: unresolved.max(1),
            manifest_digest: stored.manifest_digest,
            message: format!("adoption maintenance entry {tx_id} is missing"),
        };
    };
    if !verify_ledger_entry_signature(maintenance) {
        return AdoptionDecision {
            accepted: false,
            adopted_count: stored.adopted.len(),
            unresolved_count: unresolved.max(1),
            manifest_digest: stored.manifest_digest,
            message: "adoption maintenance signature is not valid".to_string(),
        };
    }
    if maintenance.prev_hash.as_deref() != Some(stored.stored_head.as_str()) {
        return AdoptionDecision {
            accepted: false,
            adopted_count: stored.adopted.len(),
            unresolved_count: unresolved.max(1),
            manifest_digest: stored.manifest_digest,
            message: "adoption maintenance prev_hash is not the original head".to_string(),
        };
    }
    AdoptionDecision {
        accepted: unresolved == 0,
        adopted_count: stored.adopted.len(),
        unresolved_count: unresolved,
        manifest_digest: stored.manifest_digest,
        message: format!("historical continuity {HISTORICAL_CONTINUITY}"),
    }
}

fn load_manifest_row(conn: &rusqlite::Connection) -> Result<Option<RecoveryManifest>> {
    let body: Option<String> = conn
        .query_row(
            "SELECT manifest_json FROM ledger_recovery_manifest ORDER BY created_at, manifest_digest LIMIT 1",
            [],
            |row| row.get(0),
        )
        .map(Some)
        .or_else(|err| {
            if matches!(err, rusqlite::Error::QueryReturnedNoRows) {
                Ok(None)
            } else {
                Err(err)
            }
        })
        .map_err(|e| miette!("read adoption manifest: {e}"))?;
    match body {
        Some(json) => Ok(Some(from_json(&json)?)),
        None => Ok(None),
    }
}

pub(crate) fn adoption_tx_id(digest: &str) -> String {
    format!("adopt-{digest}")
}
