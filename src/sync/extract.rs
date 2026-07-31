use crate::sync::bundle::{Bundle, Entry, Manifest};
use crate::sync::error::SyncError;
use crate::sync::hlc::HLC;
use crate::sync::state::SyncState;
use ed25519_dalek::SigningKey;
use rusqlite::{Connection, params};
use std::path::Path;

/// Finalized extract: signed zip bytes plus metadata for logging/filename.
///
/// Callers must **not** call [`Bundle::build`] again — the signature covers the
/// exact `manifest.json` bytes inside `zip_bytes`.
#[derive(Debug)]
pub struct ExtractResult {
    pub bundle: Bundle,
    pub zip_bytes: Vec<u8>,
}

/// Extract local ledger deltas into a signed bundle.
///
/// # HLC watermark compare
/// Entry and tombstone SQL filters use string compare of the Display form
/// `{physical_ms}-{:04}-{node_id}`. This is era-safe only while `physical_ms`
/// stays fixed-width (13-digit epoch ms) and logical is zero-padded to 4 digits.
pub fn extract(
    state_dir: &Path,
    device_id: &str,
    sign_key: &SigningKey,
    batch_size: usize,
) -> Result<ExtractResult, SyncError> {
    let db_path = state_dir.join("state").join("ledger.db");
    let conn = Connection::open(&db_path)?;

    // 1. Read sync_state.
    // Note: when the row exists with NULL last_extract_hlc, we must use
    // `Option<String>` in the row mapper. Combining bare `String` + `.optional()`
    // treats NULL as a type error (row exists ≠ QueryReturnedNoRows).
    let last_extract_hlc_str: Option<String> = match conn.query_row(
        "SELECT last_extract_hlc FROM sync_state WHERE id = 1",
        [],
        |row| row.get::<_, Option<String>>(0),
    ) {
        Ok(v) => v,
        Err(rusqlite::Error::QueryReturnedNoRows) => None,
        Err(e) => return Err(e.into()),
    };

    let last_extract_hlc = match last_extract_hlc_str {
        Some(s) => s.parse::<HLC>()?,
        None => HLC {
            physical_ms: 0,
            logical: 0,
            node_id: "".to_string(),
        },
    };
    let watermark = last_extract_hlc.to_string();

    // 2. Query ledger_entries
    // We extract entries that haven't been assigned an entry_hlc yet, or have been updated.
    let mut stmt = conn.prepare(
        "SELECT 
            tx_id, category, entry_type, entity, entity_normalized, 
            change_type, summary, reason, is_breaking, committed_at, 
            origin, trace_id, signature, public_key, risk, 
            verification_status, verification_basis, outcome_notes, related_tickets
         FROM ledger_entries
         WHERE (entry_hlc IS NULL OR entry_hlc > ?2)
           AND origin IN ('LOCAL', 'SIBLING')
           AND signature IS NOT NULL
         ORDER BY committed_at ASC
         LIMIT ?1",
    )?;

    let mut entries = Vec::new();
    let rows = stmt.query_map(params![batch_size as i64, &watermark], |row| {
        let committed_at_str: String = row.get(9)?;
        let committed_at = chrono::DateTime::parse_from_rfc3339(&committed_at_str)
            .map(|dt| dt.with_timezone(&chrono::Utc))
            .map_err(|_| rusqlite::Error::InvalidQuery)?;

        Ok(Entry {
            tx_id: row.get(0)?,
            category: row.get(1)?,
            entry_type: row.get(2)?,
            entity: row.get(3)?,
            entity_normalized: row.get(4)?,
            change_type: row.get(5)?,
            summary: row.get(6)?,
            reason: row.get(7)?,
            is_breaking: row.get::<_, i32>(8)? != 0,
            committed_at,
            origin: row.get(10)?,
            trace_id: row.get(11)?,
            signature: row.get(12)?,
            public_key: row.get(13)?,
            risk: row.get(14)?,
            verification_status: row.get(15)?,
            verification_basis: row.get(16)?,
            outcome_notes: row.get(17)?,
            related_tickets: row.get(18)?,
            entry_hlc: HLC {
                physical_ms: 0,
                logical: 0,
                node_id: "".to_string(),
            }, // Placeholder
        })
    })?;

    for entry_res in rows {
        entries.push(entry_res?);
    }

    // 3. Tombstone incremental export (same Display-string `>` discipline as entries).
    let mut tombstones = Vec::new();
    {
        let mut stmt = conn.prepare(
            "SELECT tx_id, tombstone_hlc, reason FROM tx_tombstones
             WHERE tombstone_hlc > ?1",
        )?;
        let tombstone_rows = stmt.query_map(params![&watermark], |row| {
            let hlc_str: String = row.get(1)?;
            Ok(crate::sync::bundle::Tombstone {
                tx_id: row.get(0)?,
                tombstone_hlc: hlc_str.parse().map_err(|_| rusqlite::Error::InvalidQuery)?,
                reason: row.get(2)?,
            })
        })?;

        for tombstone in tombstone_rows {
            tombstones.push(tombstone?);
        }
    }

    // 4. Empty / no-op: do not advance watermark, do not build empty bundle.
    if entries.is_empty() && tombstones.is_empty() {
        return Err(SyncError::NoNewEntries);
    }

    // 5. Assign HLCs (unique logical per entry in-batch)
    let bundle_hlc = HLC::now(&last_extract_hlc, device_id);

    let mut current_hlc = bundle_hlc.clone();
    for entry in &mut entries {
        entry.entry_hlc = current_hlc.clone();
        current_hlc.logical += 1;
    }

    // 6. Build Manifest and signed zip once
    let mut manifest = Manifest {
        version: 1,
        device_id: device_id.to_string(),
        bundle_hlc: bundle_hlc.clone(),
        manifest_sha256: "".to_string(),
        entry_count: entries.len(),
        entries,
        tombstones,
    };

    let (zip_bytes, signature) =
        Bundle::build(&mut manifest, sign_key).map_err(SyncError::Other)?;

    let bundle = Bundle {
        manifest,
        signature,
        device_pub: sign_key.verifying_key().to_bytes(),
    };

    // Intentionally do NOT stamp entry_hlc or advance last_extract_hlc here.
    // Callers must invoke [`commit_extract_export`] only after the signed zip
    // has been successfully encrypted and put on the transport. Otherwise a
    // failed upload permanently drops local deltas on retry.
    Ok(ExtractResult { bundle, zip_bytes })
}

/// Commit export side-effects after a successful transport put.
///
/// Stamps `entry_hlc` on extracted rows and advances `last_extract_hlc` past
/// every HLC shipped in the batch (entries + tombstones). **Never** writes
/// `last_apply_hlc` (uses [`SyncState::save_extract_cursor`]).
///
/// # When to call
/// Only after `put_outgoing_bytes` (or equivalent) succeeds. If put fails,
/// skip this function so the next extract re-selects the same pending rows.
pub fn commit_extract_export(
    state_dir: &Path,
    extracted: &ExtractResult,
    device_id: &str,
) -> Result<(), SyncError> {
    let db_path = state_dir.join("state").join("ledger.db");
    let conn = Connection::open(&db_path)?;

    for entry in &extracted.bundle.manifest.entries {
        conn.execute(
            "UPDATE ledger_entries SET entry_hlc = ?1 WHERE tx_id = ?2",
            params![entry.entry_hlc.to_string(), entry.tx_id],
        )?;
    }

    // Advance extract cursor past every HLC shipped in this batch.
    // - Entries get unique logicals (bundle_hlc, +1, +2, …). Storing only
    //   bundle_hlc (logical 0) would re-select logical>0 rows next run.
    // - Tombstone HLCs may be ahead of wall-clock bundle_hlc; include max so
    //   they are not re-exported every cycle.
    let mut cursor_hlc = extracted.bundle.manifest.bundle_hlc.clone();
    if let Some(last_entry) = extracted.bundle.manifest.entries.last()
        && last_entry.entry_hlc > cursor_hlc
    {
        cursor_hlc = last_entry.entry_hlc.clone();
    }
    for t in &extracted.bundle.manifest.tombstones {
        if t.tombstone_hlc > cursor_hlc {
            cursor_hlc = t.tombstone_hlc.clone();
        }
    }

    SyncState::save_extract_cursor(&conn, &cursor_hlc, device_id, chrono::Utc::now())
        .map_err(|e| SyncError::Other(format!("{e:#}")))?;

    Ok(())
}
