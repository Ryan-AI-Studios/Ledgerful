use crate::sync::bundle::{Bundle, Entry};
use crate::sync::error::SyncError;
use crate::sync::hlc::HLC;
use rusqlite::{Connection, OptionalExtension, params};
use std::collections::HashMap;

#[derive(Debug, Default)]
pub struct ApplyReport {
    pub total_entries: usize,
    pub inserted: usize,
    pub updated: usize,
    pub skipped: usize,
    pub errors: Vec<(String, String)>, // (tx_id, error_message)
}

pub fn apply(
    bundle: &Bundle,
    conn: &mut Connection,
    _device_keys: &HashMap<String, [u8; 32]>,
) -> Result<ApplyReport, SyncError> {
    let mut report = ApplyReport {
        total_entries: bundle.manifest.entries.len(),
        ..Default::default()
    };

    let tx = conn.transaction()?;

    // 1. Process tombstones
    for tombstone in &bundle.manifest.tombstones {
        let existing_hlc: Option<String> = tx
            .query_row(
                "SELECT tombstone_hlc FROM tx_tombstones WHERE tx_id = ?1",
                [&tombstone.tx_id],
                |row| row.get(0),
            )
            .optional()?;

        let should_apply = match existing_hlc {
            Some(h_str) => {
                let h: HLC = h_str.parse().map_err(|_| SyncError::InvalidHLC(h_str))?;
                tombstone.tombstone_hlc > h
            }
            None => true,
        };

        if should_apply {
            tx.execute(
                "INSERT INTO tx_tombstones (tx_id, tombstone_hlc, reason) VALUES (?1, ?2, ?3)
                 ON CONFLICT(tx_id) DO UPDATE SET tombstone_hlc = ?2, reason = ?3",
                params![
                    tombstone.tx_id,
                    tombstone.tombstone_hlc.to_string(),
                    tombstone.reason
                ],
            )?;

            // Also mark transaction as ROLLED_BACK if it exists
            tx.execute(
                "UPDATE transactions SET status = 'ROLLED_BACK' WHERE tx_id = ?1",
                params![tombstone.tx_id],
            )?;
            tx.execute(
                "UPDATE ledger_entries SET verification_status = 'ROLLED_BACK', outcome_notes = ?2
                 WHERE tx_id = ?1 AND entry_hlc < ?3",
                params![
                    tombstone.tx_id,
                    format!("Tombstoned: {}", tombstone.reason),
                    tombstone.tombstone_hlc.to_string()
                ],
            )?;
        }
    }

    // 2. Process entries
    for entry in &bundle.manifest.entries {
        // 2a. Check tombstones
        let tombstone: Option<String> = tx
            .query_row(
                "SELECT tombstone_hlc FROM tx_tombstones WHERE tx_id = ?1",
                [&entry.tx_id],
                |row| row.get(0),
            )
            .optional()?;

        if let Some(t_hlc_str) = tombstone {
            let t_hlc: HLC = t_hlc_str
                .parse()
                .map_err(|_| SyncError::InvalidHLC(t_hlc_str))?;
            if t_hlc >= entry.entry_hlc {
                report.skipped += 1;
                continue;
            }
        }

        let existing: Option<String> = tx
            .query_row(
                "SELECT entry_hlc FROM ledger_entries WHERE tx_id = ?1",
                [&entry.tx_id],
                |row| row.get(0),
            )
            .optional()?;

        if let Some(e_hlc_str) = existing {
            let e_hlc: HLC = e_hlc_str
                .parse()
                .map_err(|_| SyncError::InvalidHLC(e_hlc_str))?;
            if entry.entry_hlc > e_hlc {
                // Ensure transaction exists for FK (update case, though it should exist)
                ensure_transaction(&tx, entry, &bundle.manifest.device_id)?;
                // Update
                update_entry(&tx, entry, &bundle.manifest.device_id)?;
                report.updated += 1;
            } else {
                report.skipped += 1;
            }
        } else {
            // Ensure transaction exists for FK
            ensure_transaction(&tx, entry, &bundle.manifest.device_id)?;
            // Insert
            insert_entry(&tx, entry, &bundle.manifest.device_id)?;
            report.inserted += 1;
        }
    }

    // Update last_apply_hlc
    let current_apply_hlc: Option<String> = tx
        .query_row(
            "SELECT last_apply_hlc FROM sync_state WHERE id = 1",
            [],
            |row| row.get(0),
        )
        .optional()?
        .flatten();

    let should_update = match current_apply_hlc {
        Some(s) => {
            let h: HLC = s.parse().map_err(|_| SyncError::InvalidHLC(s))?;
            bundle.manifest.bundle_hlc > h
        }
        None => true,
    };

    if should_update {
        tx.execute(
            "INSERT INTO sync_state (id, last_apply_hlc) VALUES (1, ?1)
             ON CONFLICT(id) DO UPDATE SET last_apply_hlc = ?1",
            [bundle.manifest.bundle_hlc.to_string()],
        )?;
    }

    tx.commit()?;

    Ok(report)
}

fn insert_entry(
    conn: &rusqlite::Transaction,
    entry: &Entry,
    device_id: &str,
) -> Result<(), SyncError> {
    conn.execute(
        "INSERT INTO ledger_entries (
            tx_id, category, entry_type, entity, entity_normalized,
            change_type, summary, reason, is_breaking, committed_at,
            origin, trace_id, signature, public_key, risk,
            verification_status, verification_basis, outcome_notes,
            related_tickets, entry_hlc
        ) VALUES (
            ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 'PEER', ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20
        )",
        params![
            entry.tx_id, entry.category, entry.entry_type, entry.entity, entry.entity_normalized,
            entry.change_type, entry.summary, entry.reason, if entry.is_breaking { 1 } else { 0 }, entry.committed_at.to_rfc3339(),
            "PEER", device_id, entry.signature, entry.public_key, entry.risk,
            entry.verification_status, entry.verification_basis, entry.outcome_notes,
            entry.related_tickets, entry.entry_hlc.to_string()
        ],
    )?;
    Ok(())
}

fn update_entry(
    conn: &rusqlite::Transaction,
    entry: &Entry,
    device_id: &str,
) -> Result<(), SyncError> {
    // Only update mutable status fields and set trace_id to the sender device.
    // Immutable: tx_id, committed_at, origin, signature, public_key, category, summary, reason, etc.
    conn.execute(
        "UPDATE ledger_entries SET
            verification_status = ?2,
            outcome_notes = ?3,
            trace_id = ?4,
            entry_hlc = ?5
        WHERE tx_id = ?1",
        params![
            entry.tx_id,
            entry.verification_status,
            entry.outcome_notes,
            device_id,
            entry.entry_hlc.to_string()
        ],
    )?;

    // Also ensure the transaction status is updated if the incoming entry is more recent.
    conn.execute(
        "UPDATE transactions SET status = 'COMMITTED' WHERE tx_id = ?1",
        [&entry.tx_id],
    )?;

    Ok(())
}

fn ensure_transaction(
    tx: &rusqlite::Transaction,
    entry: &Entry,
    device_id: &str,
) -> Result<(), SyncError> {
    let exists: bool = tx
        .query_row(
            "SELECT 1 FROM transactions WHERE tx_id = ?1",
            [&entry.tx_id],
            |_| Ok(true),
        )
        .optional()?
        .unwrap_or(false);

    if !exists {
        tx.execute(
            "INSERT INTO transactions (
                tx_id, status, category, entity, entity_normalized,
                session_id, source, started_at, summary, reason,
                change_type, is_breaking
            ) VALUES (
                ?1, 'COMMITTED', ?2, ?3, ?4, ?5, 'PEER', ?6, ?7, ?8, ?9, ?10
            )",
            params![
                entry.tx_id,
                entry.category,
                entry.entity,
                entry.entity_normalized,
                device_id, // session_id
                entry.committed_at.to_rfc3339(),
                entry.summary,
                entry.reason,
                entry.change_type,
                if entry.is_breaking { 1 } else { 0 }
            ],
        )?;
    }
    Ok(())
}
