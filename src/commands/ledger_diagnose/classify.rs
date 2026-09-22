//! Second-pass chain classes. `ChainWalk` stays unchanged (0409).

use crate::ledger::chain_iter::iter_local_chain_with_head;
use crate::ledger::crypto::{compute_entry_hash_for_entry, verify_ledger_entry_signature};
use crate::ledger::types::{ChainHead, LedgerEntry};
use std::collections::{BTreeMap, BTreeSet};

pub(crate) const PROVENANCE_LIMITS: &str = "m51 added nullable prev_hash with no backfill. Timestamp order is not authenticated chronology.";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AnomalyRow {
    pub tx_id: String,
    pub prev_hash: Option<String>,
    pub computed_hash: String,
    pub sig_version: u32,
    pub origin: String,
    pub committed_at: String,
    pub signature_status: &'static str,
    pub in_signed_segment: bool,
    pub anomaly_class: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Diagnosis {
    pub valid_local: usize,
    pub federated_skip: usize,
    pub linked_entries: usize,
    pub extra_genesis_count: usize,
    pub orphan_count: usize,
    pub fork_count: usize,
    pub cycle_count: usize,
    pub duplicate_hash_count: usize,
    pub invalid_signature_count: usize,
    pub head_disagreement: bool,
    pub anomalies: Vec<AnomalyRow>,
    pub candidate_tx_ids: Vec<String>,
    pub row_digests: BTreeMap<String, String>,
    pub exclusions: Vec<String>,
    pub stored_head: Option<String>,
}

pub(crate) fn classify(entries: &[LedgerEntry], head: Option<&ChainHead>) -> Diagnosis {
    classify_with(entries, head, hash_of)
}

pub(crate) fn classify_with(
    entries: &[LedgerEntry],
    head: Option<&ChainHead>,
    hash_fn: impl Fn(&LedgerEntry) -> String,
) -> Diagnosis {
    let walk = iter_local_chain_with_head(entries, head);
    let signed: BTreeSet<String> = walk.ordered.iter().map(|e| e.tx_id.clone()).collect();
    let extra: BTreeSet<String> = walk.extra_genesis.iter().map(|e| e.tx_id.clone()).collect();
    let orphans: BTreeSet<String> = walk.orphans.iter().map(|e| e.tx_id.clone()).collect();
    let mut forks = BTreeSet::new();
    for (_parent, kids) in &walk.forks {
        for kid in kids {
            forks.insert(kid.clone());
        }
    }

    let mut local = Vec::new();
    let mut exclusions = Vec::new();
    for entry in entries {
        if entry.origin != "LOCAL" {
            exclusions.push(entry.tx_id.clone());
        } else {
            local.push(entry);
        }
    }
    exclusions.sort();
    exclusions.dedup();

    let mut hash_of_tx: BTreeMap<String, String> = BTreeMap::new();
    let mut tx_by_hash: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for entry in &local {
        let hash = hash_fn(entry);
        hash_of_tx.insert(entry.tx_id.clone(), hash.clone());
        tx_by_hash
            .entry(hash)
            .or_default()
            .push(entry.tx_id.clone());
    }
    let mut by_hash_one: BTreeMap<String, String> = BTreeMap::new();
    let mut duplicates = BTreeSet::new();
    for (hash, txs) in &tx_by_hash {
        let mut sorted = txs.clone();
        sorted.sort();
        if sorted.len() > 1 {
            for tx in &sorted {
                duplicates.insert(tx.clone());
            }
        }
        if let Some(first) = sorted.first() {
            by_hash_one.insert(hash.clone(), first.clone());
        }
    }
    let cycles = cycle_ids(&local, &by_hash_one);

    let mut valid_local = 0usize;
    let mut invalid_signature_count = 0usize;
    let mut anomalies = Vec::new();
    for entry in &local {
        let status = signature_status(entry);
        if status == "valid" {
            valid_local += 1;
        } else {
            invalid_signature_count += 1;
        }
        let flags = ClassFlags {
            invalid: status != "valid",
            duplicate: duplicates.contains(&entry.tx_id),
            cycle: cycles.contains(&entry.tx_id),
            extra: extra.contains(&entry.tx_id),
            fork: forks.contains(&entry.tx_id),
            orphan: orphans.contains(&entry.tx_id),
        };
        let Some(class) = primary_class(flags) else {
            continue;
        };
        anomalies.push(AnomalyRow {
            tx_id: entry.tx_id.clone(),
            prev_hash: entry.prev_hash.clone(),
            computed_hash: hash_of_tx.get(&entry.tx_id).cloned().unwrap_or_default(),
            sig_version: entry.sig_version,
            origin: entry.origin.clone(),
            committed_at: entry.committed_at.clone(),
            signature_status: status,
            in_signed_segment: signed.contains(&entry.tx_id),
            anomaly_class: class,
        });
    }
    anomalies.sort_by(|a, b| a.tx_id.cmp(&b.tx_id));

    let tail = walk.tail_hash();
    let stored_head = head.map(|h| h.latest_entry_hash.clone());
    let head_disagreement = match &stored_head {
        None => true,
        Some(stored) => tail.as_ref().is_none_or(|tip| stored != tip),
    };

    let mut candidate_tx_ids: Vec<String> = extra.iter().cloned().collect();
    candidate_tx_ids.sort();
    let mut row_digests = BTreeMap::new();
    for tx in &candidate_tx_ids {
        if let Some(hash) = hash_of_tx.get(tx) {
            row_digests.insert(tx.clone(), hash.clone());
        }
    }

    Diagnosis {
        valid_local,
        federated_skip: walk.federated_skipped,
        linked_entries: walk.ordered.len(),
        extra_genesis_count: extra.len(),
        orphan_count: orphans.len(),
        fork_count: walk.forks.len(),
        cycle_count: cycles.len(),
        duplicate_hash_count: duplicates.len(),
        invalid_signature_count,
        head_disagreement,
        anomalies,
        candidate_tx_ids,
        row_digests,
        exclusions,
        stored_head,
    }
}

struct ClassFlags {
    invalid: bool,
    duplicate: bool,
    cycle: bool,
    extra: bool,
    fork: bool,
    orphan: bool,
}

fn primary_class(flags: ClassFlags) -> Option<&'static str> {
    if flags.invalid {
        Some("invalid_signature")
    } else if flags.duplicate {
        Some("duplicate_hash")
    } else if flags.cycle {
        Some("cycle")
    } else if flags.extra {
        Some("extra_genesis")
    } else if flags.fork {
        Some("fork")
    } else if flags.orphan {
        Some("orphan")
    } else {
        None
    }
}

fn signature_status(entry: &LedgerEntry) -> &'static str {
    if entry.origin != "LOCAL" {
        return "federated_skip";
    }
    match (&entry.signature, &entry.public_key) {
        (Some(sig), Some(key)) if !sig.is_empty() && !key.is_empty() => {
            if verify_ledger_entry_signature(entry) {
                "valid"
            } else {
                "invalid"
            }
        }
        _ => "unsigned",
    }
}

fn hash_of(entry: &LedgerEntry) -> String {
    compute_entry_hash_for_entry(entry).unwrap_or_else(|_| format!("!encode_fail!{}", entry.tx_id))
}

fn cycle_ids(local: &[&LedgerEntry], by_hash: &BTreeMap<String, String>) -> BTreeSet<String> {
    let by_tx: BTreeMap<&str, &LedgerEntry> =
        local.iter().map(|e| (e.tx_id.as_str(), *e)).collect();
    let mut cycles = BTreeSet::new();
    for start in local {
        let mut seen = BTreeSet::new();
        let mut current = start.tx_id.as_str();
        loop {
            if !seen.insert(current.to_string()) {
                cycles.insert(current.to_string());
                break;
            }
            let Some(entry) = by_tx.get(current) else {
                break;
            };
            let Some(prev) = entry.prev_hash.as_deref().filter(|p| !p.is_empty()) else {
                break;
            };
            let Some(parent) = by_hash.get(prev) else {
                break;
            };
            current = parent.as_str();
        }
    }
    cycles
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger::types::{Category, ChangeType, EntryType};

    fn entry(tx_id: &str, committed_at: &str, prev_hash: Option<&str>) -> LedgerEntry {
        LedgerEntry {
            id: 0,
            tx_id: tx_id.to_string(),
            category: Category::Feature,
            entry_type: EntryType::Implementation,
            entity: "src/a.rs".into(),
            entity_normalized: "src/a.rs".into(),
            change_type: ChangeType::Modify,
            summary: "s".into(),
            reason: "r".into(),
            is_breaking: false,
            committed_at: committed_at.into(),
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
            prev_hash: prev_hash.map(str::to_string),
            sig_version: 1,
        }
    }

    #[test]
    fn chain_diagnose_pages_past_twenty() {
        let mut entries = vec![entry("tx-primary", "2020-01-01T00:00:00Z", None)];
        for i in 0..25 {
            entries.push(entry(
                &format!("tx-extra-{i:02}"),
                "2020-01-01T00:00:00Z",
                None,
            ));
        }
        let diagnosis = classify(&entries, None);
        assert_eq!(diagnosis.extra_genesis_count, 25);
        assert!(diagnosis.anomalies.len() > 20);
        let mut ids: Vec<_> = diagnosis
            .anomalies
            .iter()
            .map(|a| a.tx_id.clone())
            .collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), diagnosis.anomalies.len());
        let page: Vec<_> = diagnosis
            .anomalies
            .iter()
            .skip(10)
            .take(10)
            .map(|a| a.tx_id.as_str())
            .collect();
        assert_eq!(page.len(), 10);
        assert!(page.windows(2).all(|w| w[0] < w[1]));
    }

    #[test]
    fn chain_diagnose_classes() {
        let mut orphan = entry("tx-orphan", "2020-01-02T00:00:00Z", Some("missing-parent"));
        orphan.signature = Some("deadbeef".into());
        orphan.public_key = Some("00".into());
        let mut federated = entry("tx-fed", "2020-01-03T00:00:00Z", None);
        federated.origin = "FEDERATED".into();
        federated.sig_version = 2;
        let entries = vec![
            entry("tx-a", "2020-01-01T00:00:00Z", None),
            entry("tx-b", "2020-01-01T00:00:00Z", None),
            orphan,
            federated,
        ];
        let head = ChainHead {
            latest_entry_hash: "not-the-tail".into(),
            genesis: "g".into(),
            length: 1,
            head_signature: None,
            head_public_key: None,
            updated_at: "2020-01-01T00:00:00Z".into(),
        };
        let diagnosis = classify(&entries, Some(&head));
        assert!(diagnosis.head_disagreement);
        assert_eq!(diagnosis.federated_skip, 1);
        assert!(diagnosis.exclusions.iter().any(|id| id == "tx-fed"));
        assert!(diagnosis.extra_genesis_count >= 1);
        let invalid = diagnosis
            .anomalies
            .iter()
            .find(|a| a.tx_id == "tx-orphan")
            .expect("orphan row");
        assert_eq!(invalid.anomaly_class, "invalid_signature");
        assert_eq!(invalid.signature_status, "invalid");
        assert_eq!(invalid.origin, "LOCAL");
        assert_eq!(invalid.sig_version, 1);
        assert!(diagnosis.orphan_count >= 1);

        let dup = classify_with(&entries, None, |entry| {
            if entry.tx_id == "tx-a" || entry.tx_id == "tx-b" {
                "same-hash".into()
            } else {
                hash_of(entry)
            }
        });
        assert!(
            dup.duplicate_hash_count >= 2,
            "duplicate count {}",
            dup.duplicate_hash_count
        );

        let mut a = entries[0].clone();
        let mut b = entries[1].clone();
        a.prev_hash = Some("hash-b".into());
        b.prev_hash = Some("hash-a".into());
        let cycled = classify_with(&[a, b], None, |entry| match entry.tx_id.as_str() {
            "tx-a" => "hash-a".into(),
            "tx-b" => "hash-b".into(),
            _ => hash_of(entry),
        });
        assert!(
            cycled.cycle_count >= 1,
            "cycle count {} {:?}",
            cycled.cycle_count,
            cycled.anomalies
        );

        let parent = entry("tx-parent", "2020-01-01T00:00:00Z", None);
        let parent_hash = hash_of(&parent);
        let mut child_a = entry("tx-child-a", "2020-01-02T00:00:00Z", Some(&parent_hash));
        let mut child_b = entry("tx-child-b", "2020-01-02T00:00:00Z", Some(&parent_hash));
        child_a.signature = Some("aa".into());
        child_a.public_key = Some("bb".into());
        child_b.signature = Some("cc".into());
        child_b.public_key = Some("dd".into());
        let forked = classify(&[parent, child_a, child_b], None);
        assert!(
            forked.anomalies.iter().any(|a| a.anomaly_class == "fork") || forked.fork_count > 0,
            "fork_count {} {:?}",
            forked.fork_count,
            forked.anomalies
        );
        assert!(forked.fork_count >= 1);
    }

    fn signed(
        keys: &std::path::Path,
        tx_id: &str,
        committed_at: &str,
        prev: Option<&str>,
    ) -> LedgerEntry {
        let (sig, pub_key) = crate::ledger::crypto::sign_ledger_entry_in(
            keys,
            tx_id,
            "FEATURE",
            "s",
            "r",
            committed_at,
        )
        .expect("v1 sign");
        let mut row = entry(tx_id, committed_at, prev);
        row.signature = sig;
        row.public_key = pub_key;
        row
    }

    #[test]
    fn chain_diagnose_signed_classes() {
        let tmp = tempfile::tempdir().unwrap();
        let keys = tmp.path().join("keys");
        std::fs::create_dir_all(&keys).unwrap();
        let when = "2020-01-01T00:00:00Z";
        let primary = signed(&keys, "tx-a", when, None);
        let extra = signed(&keys, "tx-b", when, None);
        let diagnosis = classify(&[primary.clone(), extra], None);
        assert_eq!(diagnosis.extra_genesis_count, 1);
        let extra_row = diagnosis
            .anomalies
            .iter()
            .find(|row| row.tx_id == "tx-b")
            .expect("extra genesis row");
        assert_eq!(extra_row.anomaly_class, "extra_genesis");
        assert_eq!(extra_row.committed_at, primary.committed_at);
        assert_eq!(extra_row.sig_version, 1);

        let orphan = signed(
            &keys,
            "tx-orphan",
            "2020-01-02T00:00:00Z",
            Some("missing-parent"),
        );
        let orphaned = classify(std::slice::from_ref(&orphan), None);
        let orphan_row = orphaned
            .anomalies
            .iter()
            .find(|row| row.tx_id == "tx-orphan")
            .expect("orphan row");
        assert_eq!(orphan_row.anomaly_class, "orphan");
        assert_eq!(orphan_row.signature_status, "valid");
        assert!(orphaned.orphan_count >= 1);

        let parent = signed(&keys, "tx-parent", when, None);
        let parent_hash = hash_of(&parent);
        let child_a = signed(
            &keys,
            "tx-child-a",
            "2020-01-02T00:00:00Z",
            Some(&parent_hash),
        );
        let child_b = signed(
            &keys,
            "tx-child-b",
            "2020-01-02T00:00:00Z",
            Some(&parent_hash),
        );
        let forked = classify(&[parent, child_a, child_b], None);
        assert!(forked.fork_count >= 1);
        assert!(
            forked
                .anomalies
                .iter()
                .any(|row| row.anomaly_class == "fork"),
            "{:?}",
            forked
                .anomalies
                .iter()
                .map(|row| (row.tx_id.as_str(), row.anomaly_class))
                .collect::<Vec<_>>()
        );

        let mut cycle_a = signed(&keys, "tx-cycle-a", when, None);
        let mut cycle_b = signed(&keys, "tx-cycle-b", when, None);
        cycle_a.prev_hash = Some("hash-b".into());
        cycle_b.prev_hash = Some("hash-a".into());
        let cycled = classify_with(&[cycle_a, cycle_b], None, |row| match row.tx_id.as_str() {
            "tx-cycle-a" => "hash-a".into(),
            "tx-cycle-b" => "hash-b".into(),
            _ => hash_of(row),
        });
        assert!(
            cycled
                .anomalies
                .iter()
                .any(|row| row.anomaly_class == "cycle"),
            "{:?}",
            cycled.anomalies
        );

        let dup_a = signed(&keys, "tx-dup-a", when, None);
        let dup_b = signed(&keys, "tx-dup-b", when, None);
        let duplicated = classify_with(&[dup_a, dup_b], None, |_row| "same-hash".into());
        assert!(duplicated.duplicate_hash_count >= 2);
        assert!(
            duplicated
                .anomalies
                .iter()
                .any(|row| row.anomaly_class == "duplicate_hash")
        );
    }
}
