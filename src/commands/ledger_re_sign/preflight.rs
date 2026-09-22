//! Topology gate shared by dry-run and apply (0416). Head-less walk only.

use crate::ledger::chain_iter::iter_local_chain;
use crate::ledger::types::LedgerEntry;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReSignBlock {
    pub reason: &'static str,
    pub count: usize,
    pub first_tx_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ReSignPreflight {
    Clear { order: Vec<String> },
    Blocked(ReSignBlock),
}

pub(crate) fn evaluate_re_sign_preflight(entries: &[LedgerEntry]) -> ReSignPreflight {
    let walk = iter_local_chain(entries);
    if !walk.extra_genesis.is_empty() {
        return ReSignPreflight::Blocked(ReSignBlock {
            reason: "extra_genesis",
            count: walk.extra_genesis.len(),
            first_tx_id: walk.extra_genesis[0].tx_id.clone(),
        });
    }
    if !walk.forks.is_empty() {
        let first = walk.forks[0]
            .1
            .first()
            .cloned()
            .unwrap_or_else(|| walk.forks[0].0.clone());
        return ReSignPreflight::Blocked(ReSignBlock {
            reason: "fork",
            count: walk.forks.len(),
            first_tx_id: first,
        });
    }
    // A bad signature changes the entry hash, so a still-linked repair
    // candidate looks like an orphan. Diagnose reports orphans. Re-sign
    // still repairs them and rewrites prev_hash from the captured order
    // when that order is non-empty; extra genesis and forks stay blocked.
    ReSignPreflight::Clear {
        order: walk.ordered.iter().map(|e| e.tx_id.clone()).collect(),
    }
}

pub(crate) fn format_blocked(block: &ReSignBlock) -> String {
    format!(
        "BLOCKED {} count={} first={}\nRe-sign did not back up or change signatures.\nNext: ledger diagnose --json",
        block.reason, block.count, block.first_tx_id
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger::types::{Category, ChangeType, EntryType, LedgerEntry};

    fn bare(tx_id: &str) -> LedgerEntry {
        LedgerEntry {
            id: 0,
            tx_id: tx_id.into(),
            category: Category::Feature,
            entry_type: EntryType::Implementation,
            entity: "a".into(),
            entity_normalized: "a".into(),
            change_type: ChangeType::Modify,
            summary: "s".into(),
            reason: "r".into(),
            is_breaking: false,
            committed_at: "2020-01-01T00:00:00Z".into(),
            verification_status: None,
            verification_basis: None,
            outcome_notes: None,
            origin: "LOCAL".into(),
            trace_id: None,
            signature: None,
            public_key: None,
            risk: None,
            related_tickets: None,
            author: "t".into(),
            observed: None,
            prev_hash: None,
            sig_version: 1,
        }
    }

    #[test]
    fn preflight_blocks_second_empty_prev_hash() {
        let entries = vec![bare("tx-a"), bare("tx-b")];
        match evaluate_re_sign_preflight(&entries) {
            ReSignPreflight::Blocked(block) => {
                assert_eq!(block.reason, "extra_genesis");
                assert_eq!(block.count, 1);
                let text = format_blocked(&block);
                assert!(text.contains("BLOCKED extra_genesis"));
                assert!(!text.contains("Pass --yes"));
            }
            ReSignPreflight::Clear { .. } => panic!("two genesis rows must block"),
        }
    }

    #[test]
    fn preflight_clear_on_single_genesis() {
        match evaluate_re_sign_preflight(&[bare("only")]) {
            ReSignPreflight::Clear { order } => assert_eq!(order, vec!["only".to_string()]),
            ReSignPreflight::Blocked(block) => panic!("unexpected {block:?}"),
        }
    }
}
