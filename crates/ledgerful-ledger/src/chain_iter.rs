//! Shared chain walk for verify / re-sign / export (RT-C4, RT-C5).
//!
//! Walks by `prev_hash` linkage rather than `committed_at ASC, tx_id ASC`
//! (UUID-v4 order can diverge from the signed chain). Federated rows
//! (`origin != "LOCAL"`) are excluded from the local chain and counted as
//! `SKIP (federated)`.

use crate::crypto::compute_entry_hash_for_entry;
use crate::types::{ChainHead, LedgerEntry};
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
        let last = self.ordered.last()?;
        match compute_entry_hash_for_entry(last) {
            Ok(h) => Some(h),
            Err(err) => {
                tracing::error!(
                    tx_id = %last.tx_id,
                    error = %err,
                    "chain_iter::tail_hash encode failed"
                );
                None
            }
        }
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

/// Walk backward from `start` along `prev_hash`. A repeated `tx_id` stops the
/// walk (cycle). The result is genesis → tip.
fn walk_backward(
    start: LedgerEntry,
    by_hash: &BTreeMap<String, String>,
    by_tx: &BTreeMap<String, LedgerEntry>,
) -> Vec<LedgerEntry> {
    let mut rev = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut current = start;
    loop {
        if !seen.insert(current.tx_id.clone()) {
            break;
        }
        let prev = current.prev_hash.clone();
        rev.push(current);
        match prev.as_deref() {
            None | Some("") => break,
            Some(prev_hash) => {
                let Some(next_tx) = by_hash.get(prev_hash) else {
                    break;
                };
                let Some(next) = by_tx.get(next_tx) else {
                    break;
                };
                current = next.clone();
            }
        }
    }
    rev.reverse();
    rev
}

/// Iterate the local chain from the earliest empty-`prev_hash` row.
///
/// Head-less callers (re-sign, synthesized heads) keep this entry point.
/// Verify and export continuity pass a real head to
/// [`iter_local_chain_with_head`].
pub fn iter_local_chain(entries: &[LedgerEntry]) -> ChainWalk {
    iter_local_chain_with_head(entries, None)
}

/// Iterate the local chain.
///
/// - Excludes `origin != "LOCAL"`.
/// - When `head.latest_entry_hash` matches a recomputed local hash, `ordered`
///   is the backward `prev_hash` walk from that entry (genesis → tip).
/// - Otherwise genesis = the earliest `(committed_at, tx_id)` among LOCAL
///   entries with empty `prev_hash`. Other empty-`prev_hash` rows are
///   `extra_genesis`.
/// - Forks (two children of the same entry hash) are recorded; the forward
///   walk follows the lexicographically smallest child tx_id.
pub fn iter_local_chain_with_head(entries: &[LedgerEntry], head: Option<&ChainHead>) -> ChainWalk {
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
    let mut by_hash: BTreeMap<String, String> = BTreeMap::new(); // entry_hash → tx_id
    for e in &local {
        let h = hash_for_walk(e);
        hash_of.insert(e.tx_id.clone(), h.clone());
        by_hash
            .entry(h)
            .and_modify(|existing| {
                if e.tx_id.as_str() < existing.as_str() {
                    *existing = e.tx_id.clone();
                }
            })
            .or_insert_with(|| e.tx_id.clone());
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

    let head_start = head.and_then(|h| {
        let tx_id = by_hash.get(&h.latest_entry_hash)?;
        by_tx.get(tx_id).cloned()
    });

    let ordered = if let Some(start) = head_start {
        walk_backward(start, &by_hash, &by_tx)
    } else {
        let primary_genesis = genesis_candidates.first().cloned();
        let mut forward = Vec::new();
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
                forward.push(current);
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
        forward
    };

    let ordered_ids_for_genesis: BTreeSet<String> =
        ordered.iter().map(|e| e.tx_id.clone()).collect();
    let extra_genesis: Vec<LedgerEntry> = genesis_candidates
        .into_iter()
        .filter(|g| !ordered_ids_for_genesis.contains(&g.tx_id))
        .collect();

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
    use crate::types::{Category, ChainHead, ChangeType, EntryType};
    use std::collections::BTreeMap;

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

    fn entry_at(tx: &str, prev: Option<&str>, committed_at: &str) -> LedgerEntry {
        let mut row = entry(tx, prev, "LOCAL");
        row.committed_at = committed_at.to_string();
        row
    }

    fn signed_chain_with_older_null() -> (Vec<LedgerEntry>, ChainHead) {
        let older = entry_at("tx-old", None, "2026-06-27T00:00:00Z");
        let genesis = entry_at("tx-g", None, "2026-07-11T00:00:00Z");
        let g_hash = compute_entry_hash_for_entry(&genesis).expect("hash genesis");
        let mid = entry_at("tx-m", Some(&g_hash), "2026-07-12T00:00:00Z");
        let m_hash = compute_entry_hash_for_entry(&mid).expect("hash mid");
        let tip = entry_at("tx-t", Some(&m_hash), "2026-07-13T00:00:00Z");
        let t_hash = compute_entry_hash_for_entry(&tip).expect("hash tip");
        let head = ChainHead {
            latest_entry_hash: t_hash,
            genesis: genesis.committed_at.clone(),
            length: 3,
            head_signature: None,
            head_public_key: None,
            updated_at: "2026-07-13T00:00:00Z".to_string(),
        };
        (vec![older, genesis, mid, tip], head)
    }

    #[test]
    fn head_backward_walk_selects_signed_chain() {
        let (entries, head) = signed_chain_with_older_null();
        let walk = iter_local_chain_with_head(&entries, Some(&head));
        assert_eq!(walk.ordered.len(), 3);
        assert_eq!(walk.ordered[0].tx_id, "tx-g");
        assert_eq!(walk.ordered[1].tx_id, "tx-m");
        assert_eq!(walk.ordered[2].tx_id, "tx-t");
        assert_eq!(walk.extra_genesis.len(), 1);
        assert_eq!(walk.extra_genesis[0].tx_id, "tx-old");
        assert!(
            walk.orphans
                .iter()
                .all(|e| !matches!(e.tx_id.as_str(), "tx-g" | "tx-m" | "tx-t"))
        );
        assert_eq!(walk.ordered[0].committed_at, head.genesis);
    }

    #[test]
    fn head_missing_keeps_earliest_null() {
        let (entries, _) = signed_chain_with_older_null();
        let walk = iter_local_chain_with_head(&entries, None);
        assert_eq!(walk.ordered.len(), 1);
        assert_eq!(walk.ordered[0].tx_id, "tx-old");
        assert_eq!(walk.extra_genesis.len(), 1);
        assert_eq!(walk.extra_genesis[0].tx_id, "tx-g");
    }

    #[test]
    fn head_hash_unmatched_falls_back() {
        let (entries, mut head) = signed_chain_with_older_null();
        head.latest_entry_hash = "not-a-local-entry-hash".to_string();
        let walk = iter_local_chain_with_head(&entries, Some(&head));
        assert_eq!(walk.ordered.len(), 1);
        assert_eq!(walk.ordered[0].tx_id, "tx-old");
    }

    #[test]
    fn backward_walk_cycle_terminates() {
        let a = entry_at("tx-a", Some("hb"), "2026-01-01T00:00:00Z");
        let b = entry_at("tx-b", Some("ha"), "2026-01-02T00:00:00Z");
        let mut by_hash = BTreeMap::new();
        by_hash.insert("ha".to_string(), "tx-a".to_string());
        by_hash.insert("hb".to_string(), "tx-b".to_string());
        let mut by_tx = BTreeMap::new();
        by_tx.insert(a.tx_id.clone(), a.clone());
        by_tx.insert(b.tx_id.clone(), b);
        let ordered = walk_backward(a, &by_hash, &by_tx);
        assert!(ordered.len() <= 2, "cycle walk grew: {}", ordered.len());
        let mut ids: Vec<&str> = ordered.iter().map(|e| e.tx_id.as_str()).collect();
        let n = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), n, "duplicate tx_id in backward walk");
    }
}
