//! Chain-head checkpoint helpers: shared LOCAL ordering, multi-format load,
//! and checkpoint/exact compare for `verify --against-export`.
//!
//! Ordering is load-bearing: `synthesize_chain_head` and against-export must
//! agree so multi-entry pre-chain and post-chain export lengths stay consistent.

use crate::ledger::crypto::compute_entry_hash_for_entry;
use crate::ledger::types::{ChainHead, LedgerEntry};
use miette::Result;
use std::path::Path;

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
/// - **Post-chain** (any LOCAL entry with non-empty `prev_hash`): `iter_local_chain`
///   walk order (genesis → head by linkage).
/// - **Pre-chain** (no LOCAL entry has non-empty `prev_hash`): full LOCAL list
///   sorted by `(committed_at, tx_id)`.
///
/// Federated rows (`origin != "LOCAL"`) are always excluded.
pub fn ordered_local_for_head(entries: &[LedgerEntry]) -> Vec<&LedgerEntry> {
    let any_linked = entries
        .iter()
        .any(|e| e.origin == "LOCAL" && e.prev_hash.as_deref().is_some_and(|p| !p.is_empty()));

    if any_linked {
        let walk = crate::ledger::chain_iter::iter_local_chain(entries);
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

/// Load a retained chain head from a SOC2 evidence zip (`chain_head.json`
/// entry) or a bare JSON file of the same `ChainHead` shape.
///
/// Preference: `.zip` extension → zip path; `.json` → bare JSON; otherwise try
/// zip then bare JSON so extension-less artifacts still work.
#[cfg(feature = "export")]
pub fn load_checkpoint_head(path: &Path) -> Result<ChainHead> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();

    match ext.as_str() {
        "zip" => load_from_zip(path),
        "json" => load_from_json_file(path),
        _ => match load_from_zip(path) {
            Ok(head) => Ok(head),
            Err(zip_err) => match load_from_json_file(path) {
                Ok(head) => Ok(head),
                Err(json_err) => Err(miette::miette!(
                    "Failed to load chain head from {}: not a valid zip ({}); not bare ChainHead JSON ({})",
                    path.display(),
                    zip_err,
                    json_err
                )),
            },
        },
    }
}

#[cfg(feature = "export")]
fn load_from_zip(path: &Path) -> Result<ChainHead> {
    let file = std::fs::File::open(path)
        .map_err(|e| miette::miette!("Failed to open export zip {}: {}", path.display(), e))?;
    let mut archive = zip::ZipArchive::new(file)
        .map_err(|e| miette::miette!("Failed to read export zip {}: {}", path.display(), e))?;
    let mut entry = archive
        .by_name("chain_head.json")
        .map_err(|e| miette::miette!("Export missing chain_head.json: {}", e))?;
    let mut buf = Vec::new();
    std::io::Read::read_to_end(&mut entry, &mut buf)
        .map_err(|e| miette::miette!("Failed to read chain_head.json from export: {}", e))?;
    let head: ChainHead = serde_json::from_slice(&buf)
        .map_err(|e| miette::miette!("Failed to parse chain_head.json: {}", e))?;
    Ok(head)
}

#[cfg(feature = "export")]
fn load_from_json_file(path: &Path) -> Result<ChainHead> {
    let buf = std::fs::read(path)
        .map_err(|e| miette::miette!("Failed to read chain head file {}: {}", path.display(), e))?;
    let head: ChainHead = serde_json::from_slice(&buf).map_err(|e| {
        miette::miette!(
            "Failed to parse chain head JSON from {}: {}",
            path.display(),
            e
        )
    })?;
    Ok(head)
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
    // Genesis must match in both modes.
    if local_head.genesis != export_head.genesis {
        return Err(miette::miette!(
            "Live chain genesis {} does not match exported genesis {}.",
            local_head.genesis,
            export_head.genesis
        ));
    }

    // Export head signature: fail-closed when sig+pub present; soft-note if unsigned.
    let export_sig = export_head.head_signature.as_deref().unwrap_or("");
    let export_pub = export_head.head_public_key.as_deref().unwrap_or("");
    if export_sig.is_empty() || export_pub.is_empty() {
        tracing::info!(
            target: "cli_summary",
            "Exported chain head is unsigned (synthesized), cannot verify signature; length/hash/genesis comparison completed."
        );
    } else if !crate::ledger::crypto::verify_chain_head(
        &export_head.latest_entry_hash,
        &export_head.genesis,
        export_head.length,
        export_sig,
        export_pub,
    ) {
        return Err(miette::miette!(
            "Exported chain head signature verification failed."
        ));
    }

    match mode {
        CheckpointMode::Exact => compare_exact(local_head, export_head),
        CheckpointMode::Checkpoint => compare_checkpoint(ordered_local, export_head),
    }
}

fn compare_exact(local_head: &ChainHead, export_head: &ChainHead) -> Result<()> {
    if local_head.latest_entry_hash != export_head.latest_entry_hash {
        return Err(miette::miette!(
            "Live chain head {} does not match exported head {} (exact mode: snapshot equality required).",
            local_head.latest_entry_hash,
            export_head.latest_entry_hash
        ));
    }
    if local_head.length != export_head.length {
        return Err(miette::miette!(
            "Live chain length {} does not match exported length {} (exact mode: snapshot equality required).",
            local_head.length,
            export_head.length
        ));
    }
    Ok(())
}

fn compare_checkpoint(ordered_local: &[&LedgerEntry], export_head: &ChainHead) -> Result<()> {
    let k = export_head.length;
    if k < 0 {
        return Err(miette::miette!(
            "Exported chain head has invalid length {}.",
            k
        ));
    }
    let k_usize = k as usize;
    if ordered_local.len() < k_usize {
        return Err(miette::miette!(
            "Local chain has {} linked entries but export requires length {} (rollback/tail-truncation detected).",
            ordered_local.len(),
            k
        ));
    }
    if k_usize == 0 {
        // Empty export head: nothing to prefix-match.
        return Ok(());
    }

    let entry_at_k = ordered_local[k_usize - 1];
    let hash_at_k = compute_entry_hash_for_entry(entry_at_k).map_err(|e| {
        miette::miette!(
            "Failed to compute entry hash at checkpoint position {} (TX {}): {e}",
            k,
            entry_at_k.tx_id
        )
    })?;

    if hash_at_k != export_head.latest_entry_hash {
        return Err(miette::miette!(
            "Chain fork/rewrite at checkpoint position {}: local entry hash {} does not match exported latest_entry_hash {} (not a clean extension of the retained head).",
            k,
            hash_at_k,
            export_head.latest_entry_hash
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger::types::{Category, ChangeType, EntryType};

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
