//! CLI copy for the re-sign topology gate (0435).
//!
//! Topology types and `evaluate_re_sign_preflight` live in `crate::ledger::re_sign`.

pub(crate) use crate::ledger::re_sign::{ReSignBlock, ReSignPreflight, evaluate_re_sign_preflight};

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
    fn format_blocked_includes_diagnose_next() {
        let entries = vec![bare("tx-a"), bare("tx-b")];
        match evaluate_re_sign_preflight(&entries) {
            ReSignPreflight::Blocked(block) => {
                let text = format_blocked(&block);
                assert!(text.contains("BLOCKED extra_genesis"));
                assert!(text.contains("Next: ledger diagnose --json"));
                assert!(!text.contains("Pass --yes"));
            }
            ReSignPreflight::Clear { .. } => panic!("two genesis rows must block"),
        }
    }
}
