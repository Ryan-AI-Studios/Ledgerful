use super::backup::nanos_since_epoch;
use super::preview::{ReSignCandidate, key_fingerprint};
use crate::ledger::db::LedgerDb;
use crate::ledger::types::{Category, ChangeType, EntryType, LedgerEntry};
use crate::state::layout::Layout;
use crate::state::storage::StorageManager;
use chrono::Utc;
use miette::{Result, miette};
use std::path::Path;

pub(crate) struct ReSignMutation {
    pub repaired_tx_ids: Vec<String>,
    pub maintenance_tx_id: String,
}

/// Sign candidates, rewrite LOCAL chain links, insert one MAINTENANCE row, CAS head.
#[allow(clippy::too_many_arguments)]
pub(crate) fn apply_re_sign(
    storage: &mut StorageManager,
    keys_dir: &Path,
    candidates: &[ReSignCandidate],
    new_pub_key: &str,
    layout: &Layout,
    config: &crate::config::model::Config,
    signing_required: bool,
    is_upgrade_mode: bool,
) -> Result<ReSignMutation> {
    let author = current_actor(layout, config);
    let now = Utc::now().to_rfc3339();

    let mut repaired_tx_ids: Vec<String> = Vec::with_capacity(candidates.len());
    let mut old_keys: Vec<String> = Vec::with_capacity(candidates.len());
    let mut new_sigs: Vec<String> = Vec::with_capacity(candidates.len());

    let sqlite_tx = storage
        .get_connection_mut()
        .transaction()
        .map_err(|e| miette!("Failed to begin re-sign transaction: {}", e))?;

    let old_head_opt: Option<crate::ledger::types::ChainHead> = {
        let db = LedgerDb::new(&sqlite_tx);
        db.get_chain_head()
            .map_err(|e| miette!("Failed to read chain head: {}", e))?
    };
    let old_head_hash = old_head_opt.as_ref().map(|h| h.latest_entry_hash.as_str());

    {
        let db = LedgerDb::new(&sqlite_tx);
        for (tx_id, _old_sig, _old_pub) in candidates {
            // Re-read the entry row so we sign the exact committed payload fields.
            let entry_rows = db
                .get_ledger_entries_for_tx(tx_id)
                .map_err(|e| miette!("Failed to read entry for {}: {}", tx_id, e))?;
            let entry = entry_rows
                .into_iter()
                .next()
                .ok_or_else(|| miette!("Ledger entry for {} disappeared during re-sign", tx_id))?;

            let mut sign_input = crate::ledger::crypto::LedgerSignInput::from_entry(&entry);
            sign_input.sig_version = crate::ledger::crypto::CURRENT_LEDGER_SIG_VERSION;
            let (new_sig_opt, new_pub_opt) =
                crate::ledger::crypto::sign_ledger_entry_in_v2(keys_dir, &sign_input)
                    .map_err(|e| miette!("Signing failed for {}: {}", tx_id, e))?;

            let new_sig = new_sig_opt.ok_or_else(|| {
                miette!(
                    "sign_ledger_entry_in_v2 returned no signature for {}",
                    tx_id
                )
            })?;
            let new_pub = new_pub_opt.ok_or_else(|| {
                miette!(
                    "sign_ledger_entry_in_v2 returned no public key for {}",
                    tx_id
                )
            })?;

            let updated = db
                .update_ledger_entry_signature(&entry.tx_id, &new_sig, &new_pub)
                .map_err(|e| miette!("Failed to update signature for {}: {}", tx_id, e))?;
            if updated == 0 {
                return Err(miette!(
                    "No ledger entry row updated for {} (already invalid state?)",
                    tx_id
                ));
            }

            repaired_tx_ids.push(entry.tx_id.clone());
            old_keys.push(
                entry
                    .public_key
                    .clone()
                    .as_deref()
                    .map(key_fingerprint)
                    .unwrap_or_else(|| "(none)".to_string()),
            );
            new_sigs.push(key_fingerprint(&new_sig));
        }
    }

    // Rebuild chain links for LOCAL entries only, ordered by prev_hash linkage
    // (shared iterator — RT-C4). Federated rows are excluded from the local
    // chain; forks/orphans surface as hard errors.
    let (new_chain_length, new_genesis, new_tail_hash) = {
        let db = LedgerDb::new(&sqlite_tx);
        let entries = db
            .get_all_committed_ledger_entries()
            .map_err(|e| miette!("Failed to read ledger entries for chain rebuild: {}", e))?;

        let walk = crate::ledger::chain_iter::iter_local_chain(&entries);
        if !walk.forks.is_empty() {
            return Err(miette!(
                "CHAIN_BREAK: cannot re-sign while local chain has {} fork(s) (first parent hash {}). Resolve forks before re-sign.",
                walk.forks.len(),
                walk.forks[0].0
            ));
        }
        if !walk.orphans.is_empty() {
            return Err(miette!(
                "CHAIN_BREAK: cannot re-sign while {} orphan LOCAL entr(y/ies) are unlinked (first: {}).",
                walk.orphans.len(),
                walk.orphans[0].tx_id
            ));
        }
        if !walk.extra_genesis.is_empty() {
            return Err(miette!(
                "CHAIN_BREAK: cannot re-sign with {} additional genesis entr(y/ies) (first: {}).",
                walk.extra_genesis.len(),
                walk.extra_genesis[0].tx_id
            ));
        }

        // Prefer the LOCAL walk order. When the ledger is pre-chain (no prev_hash
        // links yet), fall back to a deterministic committed_at/tx_id order over
        // LOCAL rows only so re-sign can establish the first chain segment.
        let rebuild_order: Vec<LedgerEntry> = if walk.ordered.is_empty() {
            let mut local: Vec<LedgerEntry> = entries
                .into_iter()
                .filter(|e| e.origin == "LOCAL")
                .collect();
            local.sort_by(|a, b| {
                a.committed_at
                    .cmp(&b.committed_at)
                    .then_with(|| a.tx_id.cmp(&b.tx_id))
            });
            local
        } else {
            walk.ordered
        };

        // Re-sign creates a fresh chain segment from the earliest LOCAL entry
        // through the new maintenance tail. Signatures (and therefore entry
        // hashes) changed, so prev_hash links are rewritten in walk order.
        let genesis = rebuild_order
            .first()
            .map(|e| e.committed_at.clone())
            .unwrap_or_else(|| now.clone());
        let mut chain_length: i64 = 0;
        let mut prev_hash: Option<String> = None;
        for entry in &rebuild_order {
            let prev = prev_hash.as_deref().unwrap_or("");
            if prev.is_empty() {
                db.update_ledger_entry_prev_hash(&entry.tx_id, None)
                    .map_err(|e| {
                        miette!(
                            "Failed to clear genesis prev_hash for {}: {}",
                            entry.tx_id,
                            e
                        )
                    })?;
            } else {
                db.update_ledger_entry_prev_hash(&entry.tx_id, Some(prev))
                    .map_err(|e| {
                        miette!("Failed to update prev_hash for {}: {}", entry.tx_id, e)
                    })?;
            }
            chain_length += 1;
            // Re-read after signature update so sig_version/signature are current.
            let refreshed = db
                .get_ledger_entries_for_tx(&entry.tx_id)
                .map_err(|e| miette!("Failed to re-read entry {}: {}", entry.tx_id, e))?
                .into_iter()
                .next()
                .unwrap_or_else(|| entry.clone());
            let mut for_hash = refreshed;
            for_hash.prev_hash = if prev.is_empty() {
                None
            } else {
                Some(prev.to_string())
            };
            prev_hash = Some(
                crate::ledger::crypto::compute_entry_hash_for_entry(&for_hash).map_err(|e| {
                    miette!(
                        "Failed to compute entry hash for {} during re-sign rebuild: {}",
                        for_hash.tx_id,
                        e
                    )
                })?,
            );
        }
        (chain_length, genesis, prev_hash)
    };

    // Exactly one MAINTENANCE entry documents the whole batch / single repair
    // and serves as the new chain head, linking the old head to the new head.
    let maintenance_entry = build_maintenance_entry(
        candidates,
        &repaired_tx_ids,
        &old_keys,
        &new_sigs,
        new_pub_key,
        &now,
        &author,
        old_head_hash,
        is_upgrade_mode,
    );

    let maintenance_tx_id = {
        let db = LedgerDb::new(&sqlite_tx);
        insert_maintenance_transaction(
            &db,
            &maintenance_entry.tx_id,
            &maintenance_entry.committed_at,
            &maintenance_entry.author,
            is_upgrade_mode,
        )?;

        // Sign the maintenance entry so it has a stable hash for the chain head
        // and so it does not itself become an invalid-signature row when signing
        // is required.
        let mut signed_maintenance_entry = maintenance_entry.clone();
        if signing_required {
            let maint_input =
                crate::ledger::crypto::LedgerSignInput::from_entry(&signed_maintenance_entry);
            let (maint_sig, maint_pub) =
                crate::ledger::crypto::sign_ledger_entry_in_v2(keys_dir, &maint_input)
                    .map_err(|e| miette!("Failed to sign maintenance entry: {}", e))?;
            signed_maintenance_entry.signature = maint_sig;
            signed_maintenance_entry.public_key = maint_pub;
            signed_maintenance_entry.sig_version =
                crate::ledger::crypto::CURRENT_LEDGER_SIG_VERSION;
        }

        let maint_prev = new_tail_hash.as_deref().unwrap_or("");
        signed_maintenance_entry.prev_hash = if maint_prev.is_empty() {
            None
        } else {
            Some(maint_prev.to_string())
        };

        db.insert_ledger_entry(&signed_maintenance_entry)
            .map_err(|e| miette!("Failed to insert maintenance ledger entry: {}", e))?;

        let new_latest_hash =
            crate::ledger::crypto::compute_entry_hash_for_entry(&signed_maintenance_entry)
                .map_err(|e| miette!("Failed to compute maintenance entry hash: {e}"))?;

        let (head_sig, head_pub) = match crate::ledger::crypto::sign_chain_head(
            keys_dir,
            &new_latest_hash,
            &new_genesis,
            new_chain_length + 1,
        ) {
            Ok(res) => res,
            Err(e) => {
                if signing_required {
                    return Err(miette!("Failed to sign new chain head: {}", e));
                }
                tracing::warn!(
                    "Chain head signing failed (signing not required, storing unsigned head): {}",
                    e
                );
                (None, None)
            }
        };

        let new_head = crate::ledger::types::ChainHead {
            latest_entry_hash: new_latest_hash,
            genesis: new_genesis,
            length: new_chain_length + 1,
            head_signature: head_sig,
            head_public_key: head_pub,
            updated_at: now.clone(),
        };
        let updated = db
            .update_chain_head(&new_head, old_head_opt.as_ref())
            .map_err(|e| miette!("Failed to update chain head: {}", e))?;
        if !updated {
            return Err(miette!(
                "Chain head moved during re-sign (CAS mismatch). Aborting to prevent stale head."
            ));
        }

        signed_maintenance_entry.tx_id.clone()
    };

    sqlite_tx
        .commit()
        .map_err(|e| miette!("Failed to commit re-sign transaction: {}", e))?;

    Ok(ReSignMutation {
        repaired_tx_ids,
        maintenance_tx_id,
    })
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn build_maintenance_entry(
    candidates: &[ReSignCandidate],
    repaired_tx_ids: &[String],
    old_keys: &[String],
    new_sigs: &[String],
    new_pub_key: &str,
    committed_at: &str,
    author: &str,
    old_head_hash: Option<&str>,
    is_upgrade_mode: bool,
) -> LedgerEntry {
    let is_batch = candidates.len() > 1;
    let affected = if is_batch {
        // Sort deterministically; list all repaired tx_ids for batch provenance.
        let mut ids = repaired_tx_ids.to_vec();
        ids.sort();
        ids.join(", ")
    } else {
        let (_, old_sig, old_pub) = &candidates[0];
        let old_sig_fp = if old_sig.is_empty() {
            "(none)".to_string()
        } else {
            key_fingerprint(old_sig)
        };
        let old_pub_fp = if old_pub.is_empty() {
            "(none)".to_string()
        } else {
            key_fingerprint(old_pub)
        };
        format!(
            "tx_id={}; old_sig={}; old_pub={}; new_sig={}; new_pub={}",
            repaired_tx_ids[0],
            old_sig_fp,
            old_pub_fp,
            new_sigs[0],
            key_fingerprint(new_pub_key)
        )
    };

    let old_head_fp = old_head_hash
        .map(key_fingerprint)
        .unwrap_or_else(|| "(none)".to_string());

    let op_label = if is_upgrade_mode {
        "sig-upgrade"
    } else {
        "re-sign"
    };
    let summary = if is_batch {
        format!(
            "Chain segment break: {op_label} — re-signed {} ledger entries",
            candidates.len()
        )
    } else {
        format!("Chain segment break: {op_label} — re-signed one ledger entry")
    };

    let reason_prefix = if is_upgrade_mode {
        "Signature upgrade / re-sign operation (v1→current and/or invalid repair)."
    } else {
        "Key-repair / re-sign operation."
    };
    let reason = format!(
        "{reason_prefix} Old key fingerprints: [{}]. New public key fingerprint: {}. Old chain head: {}. Affected: {}.",
        old_keys.join(", "),
        key_fingerprint(new_pub_key),
        old_head_fp,
        affected
    );

    // The ledger_entries table has a foreign-key constraint on transactions(tx_id).
    // The maintenance entry documents a batch re-sign operation, so we create a synthetic
    // COMMITTED transaction row to satisfy the schema and preserve the audit trail.
    let maintenance_tx_id = format!("resign-{}-maintenance", nanos_since_epoch());

    LedgerEntry {
        id: 0,
        tx_id: maintenance_tx_id,
        category: Category::Chore,
        entry_type: EntryType::Maintenance,
        entity: "ledger/signatures".to_string(),
        entity_normalized: "ledger/signatures".to_string(),
        change_type: ChangeType::Modify,
        summary,
        reason,
        is_breaking: false,
        committed_at: committed_at.to_string(),
        verification_status: None,
        verification_basis: None,
        outcome_notes: Some(format!(
            "re_sign_count={}; mode={}; new_pub_fp={}; affected_tx_ids=[{}]",
            candidates.len(),
            if is_upgrade_mode {
                "upgrade"
            } else {
                "key-repair"
            },
            key_fingerprint(new_pub_key),
            if is_batch {
                repaired_tx_ids.join(", ")
            } else {
                repaired_tx_ids[0].clone()
            }
        )),
        origin: "LOCAL".to_string(),
        trace_id: None,
        signature: None,
        public_key: None,
        risk: None,
        related_tickets: None,
        author: author.to_string(),
        observed: None,
        prev_hash: None,
        sig_version: crate::ledger::crypto::CURRENT_LEDGER_SIG_VERSION,
    }
}

fn insert_maintenance_transaction(
    db: &LedgerDb,
    tx_id: &str,
    committed_at: &str,
    _author: &str,
    is_upgrade_mode: bool,
) -> Result<(), miette::Error> {
    let planned = if is_upgrade_mode {
        "Ledger signature upgrade / re-sign".to_string()
    } else {
        "Ledger signature re-sign / key-repair".to_string()
    };
    let tx = crate::ledger::types::Transaction {
        tx_id: tx_id.to_string(),
        operation_id: None,
        status: "COMMITTED".to_string(),
        category: Category::Chore,
        entity: "ledger/signatures".to_string(),
        entity_normalized: "ledger/signatures".to_string(),
        planned_action: Some(planned),
        session_id: crate::ledger::session::get_session_id().to_string(),
        source: "CLI".to_string(),
        started_at: committed_at.to_string(),
        resolved_at: Some(committed_at.to_string()),
        detected_at: None,
        drift_count: 1,
        first_seen_at: Some(committed_at.to_string()),
        last_seen_at: Some(committed_at.to_string()),
        issue_ref: None,
        snapshot_id: None,
    };
    db.insert_transaction(&tx)
        .map_err(|e| miette!("Failed to insert maintenance transaction row: {}", e))?;
    Ok(())
}

fn current_actor(repo_root: &Layout, config: &crate::config::model::Config) -> String {
    let from_git = || {
        let policy = config.verify.effective_process_policy();
        let read = |key: &str| -> Option<String> {
            crate::git::git_command_with_policy(&policy)
                .ok()?
                .args(["config", key])
                .current_dir(repo_root.root.as_path())
                .output()
                .ok()
                .and_then(|o| {
                    if o.status.success() {
                        let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
                        if s.is_empty() { None } else { Some(s) }
                    } else {
                        None
                    }
                })
        };
        read("user.name").or_else(|| read("user.email"))
    };

    from_git()
        .or_else(|| std::env::var("USER").ok())
        .or_else(|| std::env::var("USERNAME").ok())
        .unwrap_or_else(|| "unknown".to_string())
}
