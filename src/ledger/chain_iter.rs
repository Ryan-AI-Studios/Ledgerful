//! Shared chain walk for verify / re-sign / export (RT-C4, RT-C5).
//!
//! Walks by `prev_hash` linkage rather than `committed_at ASC, tx_id ASC`
//! (UUID-v4 order can diverge from the signed chain). Federated rows
//! (`origin != "LOCAL"`) are excluded from the local chain and counted as
//! `SKIP (federated)`.

use crate::ledger::crypto::compute_entry_hash_for_entry;
use crate::ledger::types::LedgerEntry;
use std::collections::{BTreeMap, BTreeSet};

/// Result of walking the local ledger chain.
#[derive(Debug, Clone)]
pub struct ChainWalk {
    /// Entries in chain order (genesis → head), LOCAL only.
    pub ordered: Vec<LedgerEntry>,
    /// Count of federated (`origin != "LOCAL"`) rows excluded from the walk.
    pub federated_skipped: usize,
    /// Orphan LOCAL entries whose `prev_hash` was not found in the LOCAL set.
    pub orphans: Vec<LedgerEntry>,
    /// Detected forks: parent hash → child tx_ids (more than one successor).
    pub forks: Vec<(String, Vec<String>)>,
    /// Entries with no `prev_hash` that look like additional genesis candidates
    /// beyond the chosen primary genesis.
    pub extra_genesis: Vec<LedgerEntry>,
}

impl ChainWalk {
    pub fn length(&self) -> i64 {
        self.ordered.len() as i64
    }

    pub fn tail_hash(&self) -> Option<String> {
        self.ordered
            .last()
            .and_then(|e| compute_entry_hash_for_entry(e).ok())
    }

    pub fn genesis_committed_at(&self) -> Option<&str> {
        self.ordered.first().map(|e| e.committed_at.as_str())
    }
}

/// Hash an entry for chain walks. Encode failures yield a deterministic
/// non-empty marker (never an empty digest that could silently collide).
fn hash_for_walk(entry: &LedgerEntry) -> String {
    match compute_entry_hash_for_entry(entry) {
        Ok(h) => h,
        Err(err) => {
            tracing::error!(
                tx_id = %entry.tx_id,
                error = %err,
                "entry hash encode failed during chain walk"
            );
            format!("!encode_fail!{}", entry.tx_id)
        }
    }
}

/// Iterate the local chain from genesis through successors by `prev_hash`.
///
/// - Excludes `origin != "LOCAL"`.
/// - Genesis = LOCAL entries with `prev_hash == None` (or empty). If multiple
///   genesis candidates exist, the earliest by `(committed_at, tx_id)` is the
///   primary chain start; others are reported as `extra_genesis`.
/// - Forks (two children of the same entry hash) are recorded; the walk follows
///   the lexicographically smallest child tx_id for determinism and reports the
///   fork.
pub fn iter_local_chain(entries: &[LedgerEntry]) -> ChainWalk {
    let mut federated_skipped = 0usize;
    let mut local: Vec<LedgerEntry> = Vec::new();
    for e in entries {
        if e.origin != "LOCAL" {
            federated_skipped += 1;
        } else {
            local.push(e.clone());
        }
    }

    // Map entry_hash → entry for successor lookup via prev_hash.
    // Also map prev_hash → children.
    let mut hash_of: BTreeMap<String, String> = BTreeMap::new(); // tx_id → entry_hash
    let mut by_tx: BTreeMap<String, LedgerEntry> = BTreeMap::new();
    for e in &local {
        let h = hash_for_walk(e);
        hash_of.insert(e.tx_id.clone(), h);
        by_tx.insert(e.tx_id.clone(), e.clone());
    }

    let mut children: BTreeMap<String, Vec<String>> = BTreeMap::new(); // parent_hash → child tx_ids
    let mut genesis_candidates: Vec<LedgerEntry> = Vec::new();
    for e in &local {
        match e.prev_hash.as_deref() {
            None | Some("") => genesis_candidates.push(e.clone()),
            Some(prev) => {
                children
                    .entry(prev.to_string())
                    .or_default()
                    .push(e.tx_id.clone());
            }
        }
    }
    // Deterministic child order
    for kids in children.values_mut() {
        kids.sort();
    }

    genesis_candidates.sort_by(|a, b| {
        a.committed_at
            .cmp(&b.committed_at)
            .then_with(|| a.tx_id.cmp(&b.tx_id))
    });

    let mut forks: Vec<(String, Vec<String>)> = Vec::new();
    for (parent, kids) in &children {
        if kids.len() > 1 {
            forks.push((parent.clone(), kids.clone()));
        }
    }
    forks.sort_by(|a, b| a.0.cmp(&b.0));

    let mut extra_genesis = Vec::new();
    let primary_genesis = genesis_candidates.first().cloned();
    if genesis_candidates.len() > 1 {
        extra_genesis.extend(genesis_candidates.into_iter().skip(1));
    }

    let mut ordered = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    if let Some(start) = primary_genesis {
        let mut current = start;
        loop {
            if !seen.insert(current.tx_id.clone()) {
                break; // cycle guard
            }
            let cur_hash = hash_of
                .get(&current.tx_id)
                .cloned()
                .unwrap_or_else(|| hash_for_walk(&current));
            ordered.push(current);
            match children.get(&cur_hash) {
                Some(kids) if !kids.is_empty() => {
                    // Follow first (lexicographically smallest) child.
                    let next_tx = &kids[0];
                    match by_tx.get(next_tx) {
                        Some(next) => current = next.clone(),
                        None => break,
                    }
                }
                _ => break,
            }
        }
    }

    let ordered_ids: BTreeSet<String> = ordered.iter().map(|e| e.tx_id.clone()).collect();
    let mut orphans: Vec<LedgerEntry> = local
        .into_iter()
        .filter(|e| !ordered_ids.contains(&e.tx_id))
        .filter(|e| {
            // Orphan = has prev_hash pointing nowhere, or not on primary walk
            // and not already listed as extra genesis.
            e.prev_hash.as_deref().is_some_and(|p| !p.is_empty())
                || !extra_genesis.iter().any(|g| g.tx_id == e.tx_id)
        })
        .collect();
    // Don't double-count extra_genesis as orphans.
    orphans.retain(|e| !extra_genesis.iter().any(|g| g.tx_id == e.tx_id));
    orphans.sort_by(|a, b| {
        a.committed_at
            .cmp(&b.committed_at)
            .then_with(|| a.tx_id.cmp(&b.tx_id))
    });

    ChainWalk {
        ordered,
        federated_skipped,
        orphans,
        forks,
        extra_genesis,
    }
}

/// Verify sequential prev_hash links on an already-ordered chain segment.
/// Returns the first break message, if any.
pub fn check_chain_links(ordered: &[LedgerEntry]) -> Option<String> {
    let mut prev_hash: Option<String> = None;
    for entry in ordered {
        if let Some(expected_prev) = prev_hash.as_ref() {
            match &entry.prev_hash {
                Some(actual_prev) if actual_prev == expected_prev => {}
                other => {
                    let detail = match other {
                        Some(actual) => {
                            format!("expected prev_hash {}, found {}", expected_prev, actual)
                        }
                        None => {
                            format!("expected prev_hash {} but entry has none", expected_prev)
                        }
                    };
                    return Some(format!("Chain break at TX {}: {}", entry.tx_id, detail));
                }
            }
        } else if entry.prev_hash.as_deref().is_some_and(|p| !p.is_empty()) {
            return Some(format!(
                "Chain break at TX {}: genesis entry must have no prev_hash",
                entry.tx_id
            ));
        }
        prev_hash = Some(hash_for_walk(entry));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger::types::{Category, ChangeType, EntryType};

    fn entry(tx: &str, prev: Option<&str>, origin: &str) -> LedgerEntry {
        LedgerEntry {
            id: 0,
            tx_id: tx.to_string(),
            category: Category::Feature,
            entry_type: EntryType::Implementation,
            entity: "e".into(),
            entity_normalized: "e".into(),
            change_type: ChangeType::Modify,
            summary: "s".into(),
            reason: "r".into(),
            is_breaking: false,
            committed_at: format!("2026-01-0{}T00:00:00Z", tx.chars().last().unwrap_or('1')),
            verification_status: None,
            verification_basis: None,
            outcome_notes: None,
            origin: origin.into(),
            trace_id: None,
            signature: Some("sig".into()),
            public_key: Some("pk".into()),
            risk: None,
            related_tickets: None,
            author: "a".into(),
            observed: None,
            prev_hash: prev.map(|s| s.to_string()),
            sig_version: 2,
        }
    }

    #[test]
    fn federated_rows_are_skipped() {
        let a = entry("tx1", None, "LOCAL");
        let a_hash = compute_entry_hash_for_entry(&a).expect("hash");
        let b = entry("tx2", Some(&a_hash), "LOCAL");
        let fed = entry("txf", None, "SIBLING");
        let walk = iter_local_chain(&[a, b, fed]);
        assert_eq!(walk.federated_skipped, 1);
        assert_eq!(walk.ordered.len(), 2);
        assert_eq!(walk.ordered[0].tx_id, "tx1");
        assert_eq!(walk.ordered[1].tx_id, "tx2");
    }

    #[test]
    fn empty_local_chain() {
        let fed = entry("txf", None, "PEER");
        let walk = iter_local_chain(&[fed]);
        assert_eq!(walk.federated_skipped, 1);
        assert!(walk.ordered.is_empty());
    }
}
