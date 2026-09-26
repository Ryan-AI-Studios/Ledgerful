//! Chain-head checkpoint helpers: shared LOCAL ordering and
//! checkpoint/exact compare for `verify --against-export`.
//!
//! Zip/JSON load of a retained head stays in the engine (`export` feature).

use crate::crypto::compute_entry_hash_for_entry;
use crate::types::{ChainHead, LedgerEntry};
use miette::Result;

/// Non-error checkpoint outcome for `verify --json` (0321).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CheckpointResultKind {
    Match,
    Extends,
    Diverges,
    ExactMismatch,
    ExportSigInvalid,
}

impl CheckpointResultKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Match => "match",
            Self::Extends => "extends",
            Self::Diverges => "diverges",
            Self::ExactMismatch => "exactMismatch",
            Self::ExportSigInvalid => "exportSigInvalid",
        }
    }

    pub fn is_match(self) -> bool {
        matches!(self, Self::Match)
    }

    /// JSON/human pass: live equals or cleanly extends the export.
    /// Fail kinds are `diverges` / `exactMismatch` / `exportSigInvalid`.
    pub fn is_pass(self) -> bool {
        matches!(self, Self::Match | Self::Extends)
    }
}

/// Comparison mode for `verify --against-export`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CheckpointMode {
    /// Live ordered chain must extend or equal the retained export head
    /// (hash at export.length matches export.latest_entry_hash).
    #[default]
    Checkpoint,
    /// Full head equality: latest hash, genesis, and length must match.
    Exact,
}

/// Ordered LOCAL entries used for chain-head synthesis and checkpoint compare.
///
/// Head-less. Re-sign and engine `export::soc2::synthesize_chain_head` stay
/// on this function. A real stored head uses [`ordered_local_for_head_with_head`].
///
/// - **Post-chain** (any LOCAL entry with non-empty `prev_hash`): `iter_local_chain`
///   walk order (earliest empty `prev_hash`, then linkage).
/// - **Pre-chain** (no LOCAL entry has non-empty `prev_hash`): full LOCAL list
///   sorted by `(committed_at, tx_id)`.
///
/// Federated rows (`origin != "LOCAL"`) are always excluded.
pub fn ordered_local_for_head(entries: &[LedgerEntry]) -> Vec<&LedgerEntry> {
    ordered_local_for_head_with_head(entries, None)
}

/// Same partition as [`ordered_local_for_head`], but a real head selects the
/// backward walk from `latest_entry_hash` when that hash matches a local entry.
pub fn ordered_local_for_head_with_head<'a>(
    entries: &'a [LedgerEntry],
    head: Option<&ChainHead>,
) -> Vec<&'a LedgerEntry> {
    let any_linked = entries
        .iter()
        .any(|e| e.origin == "LOCAL" && e.prev_hash.as_deref().is_some_and(|p| !p.is_empty()));

    if any_linked {
        let walk = crate::chain_iter::iter_local_chain_with_head(entries, head);
        // Map walk order (owned clones) back to references into the input slice
        // so callers can hash without re-cloning entry payloads.
        walk.ordered
            .iter()
            .filter_map(|w| entries.iter().find(|e| e.tx_id == w.tx_id))
            .collect()
    } else {
        let mut local: Vec<&LedgerEntry> = entries.iter().filter(|e| e.origin == "LOCAL").collect();
        local.sort_by(|a, b| {
            a.committed_at
                .cmp(&b.committed_at)
                .then_with(|| a.tx_id.cmp(&b.tx_id))
        });
        local
    }
}

/// Compare ordered local entries + local head against a retained export head.
///
/// Caller is responsible for empty-local wipe, local head bind/sig when real,
/// and synthesizing a local head for pre-chain when needed. This function
/// performs genesis/sig/prefix-or-exact checks only.
pub fn compare_against_export(
    ordered_local: &[&LedgerEntry],
    local_head: &ChainHead,
    export_head: &ChainHead,
    mode: CheckpointMode,
) -> Result<()> {
    match classify_against_export(ordered_local, local_head, export_head, mode) {
        Ok(_) => Ok(()),
        Err((_, msg)) => Err(miette::miette!("{}", msg)),
    }
}

/// Map checkpoint compare to a result kind (0321). `Ok` is `match` or `extends`.
pub fn classify_against_export(
    ordered_local: &[&LedgerEntry],
    local_head: &ChainHead,
    export_head: &ChainHead,
    mode: CheckpointMode,
) -> std::result::Result<CheckpointResultKind, (CheckpointResultKind, String)> {
    if local_head.genesis != export_head.genesis {
        return Err((
            CheckpointResultKind::Diverges,
            format!(
                "Live chain genesis {} does not match exported genesis {}.",
                local_head.genesis, export_head.genesis
            ),
        ));
    }

    let export_sig = export_head.head_signature.as_deref().unwrap_or("");
    let export_pub = export_head.head_public_key.as_deref().unwrap_or("");
    if export_sig.is_empty() || export_pub.is_empty() {
        tracing::info!(
            target: "cli_summary",
            "Exported chain head is unsigned (synthesized), cannot verify signature; length/hash/genesis comparison completed."
        );
    } else if !crate::crypto::verify_chain_head(
        &export_head.latest_entry_hash,
        &export_head.genesis,
        export_head.length,
        export_sig,
        export_pub,
    ) {
        return Err((
            CheckpointResultKind::ExportSigInvalid,
            "Exported chain head signature verification failed.".to_string(),
        ));
    }

    match mode {
        CheckpointMode::Exact => classify_exact(local_head, export_head),
        CheckpointMode::Checkpoint => classify_checkpoint(ordered_local, export_head),
    }
}

fn classify_exact(
    local_head: &ChainHead,
    export_head: &ChainHead,
) -> std::result::Result<CheckpointResultKind, (CheckpointResultKind, String)> {
    if local_head.latest_entry_hash != export_head.latest_entry_hash {
        return Err((
            CheckpointResultKind::ExactMismatch,
            format!(
                "Live chain head {} does not match exported head {} (exact mode: snapshot equality required).",
                local_head.latest_entry_hash, export_head.latest_entry_hash
            ),
        ));
    }
    if local_head.length != export_head.length {
        return Err((
            CheckpointResultKind::ExactMismatch,
            format!(
                "Live chain length {} does not match exported length {} (exact mode: snapshot equality required).",
                local_head.length, export_head.length
            ),
        ));
    }
    Ok(CheckpointResultKind::Match)
}

fn classify_checkpoint(
    ordered_local: &[&LedgerEntry],
    export_head: &ChainHead,
) -> std::result::Result<CheckpointResultKind, (CheckpointResultKind, String)> {
    let k = export_head.length;
    if k < 0 {
        return Err((
            CheckpointResultKind::Diverges,
            format!("Exported chain head has invalid length {}.", k),
        ));
    }
    let k_usize = k as usize;
    if ordered_local.len() < k_usize {
        return Err((
            CheckpointResultKind::Diverges,
            format!(
                "Local chain has {} linked entries but export requires length {} (rollback/tail-truncation detected).",
                ordered_local.len(),
                k
            ),
        ));
    }
    if k_usize == 0 {
        return if ordered_local.is_empty() {
            Ok(CheckpointResultKind::Match)
        } else {
            Ok(CheckpointResultKind::Extends)
        };
    }

    let entry_at_k = ordered_local[k_usize - 1];
    let hash_at_k = compute_entry_hash_for_entry(entry_at_k).map_err(|e| {
        (
            CheckpointResultKind::Diverges,
            format!(
                "Failed to compute entry hash at checkpoint position {} (TX {}): {e}",
                k, entry_at_k.tx_id
            ),
        )
    })?;

    if hash_at_k != export_head.latest_entry_hash {
        return Err((
            CheckpointResultKind::Diverges,
            format!(
                "Chain fork/rewrite at checkpoint position {}: local entry hash {} does not match exported latest_entry_hash {} (not a clean extension of the retained head).",
                k, hash_at_k, export_head.latest_entry_hash
            ),
        ));
    }
    if ordered_local.len() > k_usize {
        Ok(CheckpointResultKind::Extends)
    } else {
        Ok(CheckpointResultKind::Match)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Category, ChangeType, EntryType};

    fn entry(tx: &str, prev: Option<&str>, committed_at: &str) -> LedgerEntry {
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
            committed_at: committed_at.to_string(),
            verification_status: None,
            verification_basis: None,
            outcome_notes: None,
            origin: "LOCAL".into(),
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
    fn ordered_pre_chain_multi_entry_uses_committed_at_sort() {
        let a = entry("tx-b", None, "2026-07-11T10:00:01Z");
        let b = entry("tx-a", None, "2026-07-11T10:00:00Z");
        let entries = [a, b];
        let ordered = ordered_local_for_head(&entries);
        assert_eq!(ordered.len(), 2);
        assert_eq!(ordered[0].tx_id, "tx-a");
        assert_eq!(ordered[1].tx_id, "tx-b");
    }

    #[test]
    fn ordered_post_chain_follows_prev_hash_walk() {
        let a = entry("tx1", None, "2026-07-11T10:00:00Z");
        let a_hash = compute_entry_hash_for_entry(&a).expect("hash");
        let b = entry("tx2", Some(&a_hash), "2026-07-11T10:00:01Z");
        let entries = [b, a];
        let ordered = ordered_local_for_head(&entries);
        assert_eq!(ordered.len(), 2);
        assert_eq!(ordered[0].tx_id, "tx1");
        assert_eq!(ordered[1].tx_id, "tx2");
    }

    #[test]
    fn checkpoint_advance_past_k_passes() {
        let a = entry("tx1", None, "2026-07-11T10:00:00Z");
        let a_hash = compute_entry_hash_for_entry(&a).expect("hash");
        let b = entry("tx2", Some(&a_hash), "2026-07-11T10:00:01Z");
        let entries = [a, b];
        let ordered = ordered_local_for_head(&entries);
        let export = ChainHead {
            latest_entry_hash: a_hash,
            genesis: "2026-07-11T10:00:00Z".into(),
            length: 1,
            head_signature: None,
            head_public_key: None,
            updated_at: "2026-07-11T10:00:00Z".into(),
        };
        let local = ChainHead {
            latest_entry_hash: compute_entry_hash_for_entry(ordered[1]).expect("hash"),
            genesis: export.genesis.clone(),
            length: 2,
            head_signature: None,
            head_public_key: None,
            updated_at: "2026-07-11T10:00:01Z".into(),
        };
        compare_against_export(&ordered, &local, &export, CheckpointMode::Checkpoint)
            .expect("advance past checkpoint must pass");
    }

    #[test]
    fn is_pass_treats_extends_as_success() {
        assert!(CheckpointResultKind::Match.is_pass());
        assert!(CheckpointResultKind::Extends.is_pass());
        assert!(!CheckpointResultKind::Diverges.is_pass());
        assert!(!CheckpointResultKind::ExactMismatch.is_pass());
        assert!(!CheckpointResultKind::ExportSigInvalid.is_pass());
        assert!(CheckpointResultKind::Match.is_match());
        assert!(!CheckpointResultKind::Extends.is_match());
    }

    #[test]
    fn classify_checkpoint_advance_is_extends() {
        let a = entry("tx1", None, "2026-07-11T10:00:00Z");
        let a_hash = compute_entry_hash_for_entry(&a).expect("hash");
        let b = entry("tx2", Some(&a_hash), "2026-07-11T10:00:01Z");
        let entries = [a, b];
        let ordered = ordered_local_for_head(&entries);
        let export = ChainHead {
            latest_entry_hash: a_hash,
            genesis: "2026-07-11T10:00:00Z".into(),
            length: 1,
            head_signature: None,
            head_public_key: None,
            updated_at: "2026-07-11T10:00:00Z".into(),
        };
        let local = ChainHead {
            latest_entry_hash: compute_entry_hash_for_entry(ordered[1]).expect("hash"),
            genesis: export.genesis.clone(),
            length: 2,
            head_signature: None,
            head_public_key: None,
            updated_at: "2026-07-11T10:00:01Z".into(),
        };
        let kind = classify_against_export(&ordered, &local, &export, CheckpointMode::Checkpoint)
            .expect("extends");
        assert_eq!(kind, CheckpointResultKind::Extends);
    }

    #[test]
    fn exact_mode_rejects_advance() {
        let a = entry("tx1", None, "2026-07-11T10:00:00Z");
        let a_hash = compute_entry_hash_for_entry(&a).expect("hash");
        let b = entry("tx2", Some(&a_hash), "2026-07-11T10:00:01Z");
        let entries = [a, b];
        let ordered = ordered_local_for_head(&entries);
        let export = ChainHead {
            latest_entry_hash: a_hash,
            genesis: "2026-07-11T10:00:00Z".into(),
            length: 1,
            head_signature: None,
            head_public_key: None,
            updated_at: "2026-07-11T10:00:00Z".into(),
        };
        let local = ChainHead {
            latest_entry_hash: compute_entry_hash_for_entry(ordered[1]).expect("hash"),
            genesis: export.genesis.clone(),
            length: 2,
            head_signature: None,
            head_public_key: None,
            updated_at: "2026-07-11T10:00:01Z".into(),
        };
        let err = compare_against_export(&ordered, &local, &export, CheckpointMode::Exact)
            .expect_err("exact must fail when advanced");
        let msg = format!("{err}");
        assert!(
            msg.contains("exact mode") || msg.contains("does not match"),
            "unexpected: {msg}"
        );
    }
}
