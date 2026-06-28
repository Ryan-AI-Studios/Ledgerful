use crate::sync::bundle::{Bundle, Entry, Manifest};
use crate::sync::error::SyncError;
use crate::sync::hlc::HLC;
use ed25519_dalek::SigningKey;
use rusqlite::{Connection, OptionalExtension, params};
use std::path::Path;

pub fn extract(
    state_dir: &Path,
    device_id: &str,
    sign_key: &SigningKey,
    batch_size: usize,
) -> Result<Bundle, SyncError> {
    let db_path = state_dir.join("state").join("ledger.db");
    let conn = Connection::open(&db_path)?;

    // 1. Read sync_state
    let last_extract_hlc_str: Option<String> = conn
        .query_row(
            "SELECT last_extract_hlc FROM sync_state WHERE id = 1",
            [],
            |row| row.get(0),
        )
        .optional()?;

    let last_extract_hlc = match last_extract_hlc_str {
        Some(s) => s.parse::<HLC>()?,
        None => HLC {
            physical_ms: 0,
            logical: 0,
            node_id: "".to_string(),
        },
    };

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
    let rows = stmt.query_map(
        params![batch_size as i64, last_extract_hlc.to_string()],
        |row| {
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
        },
    )?;

    for entry_res in rows {
        entries.push(entry_res?);
    }

    // 3. Assign HLCs
    let bundle_hlc = HLC::now(&last_extract_hlc, device_id);

    // Assign unique HLCs to each entry
    let mut current_hlc = bundle_hlc.clone();
    for entry in &mut entries {
        entry.entry_hlc = current_hlc.clone();
        current_hlc.logical += 1;
    }

    // 4. Extract tombstones
    let mut tombstones = Vec::new();
    {
        let mut stmt = conn.prepare("SELECT tx_id, tombstone_hlc, reason FROM tx_tombstones")?;
        let tombstone_rows = stmt.query_map([], |row| {
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

    // 5. Build Manifest and Bundle
    let manifest = Manifest {
        version: 1,
        device_id: device_id.to_string(),
        bundle_hlc: bundle_hlc.clone(),
        manifest_sha256: "".to_string(),
        entry_count: entries.len(),
        entries,
        tombstones,
    };

    let (_zip_bytes, signature) =
        Bundle::build(manifest.clone(), sign_key).map_err(SyncError::Other)?;

    let bundle = Bundle {
        manifest,
        signature,
        device_pub: sign_key.verifying_key().to_bytes(),
    };

    // 6. Update sync_state
    conn.execute(
        "INSERT OR REPLACE INTO sync_state (id, last_extract_hlc, device_id, last_run_at)
         VALUES (1, ?1, ?2, ?3)",
        params![
            bundle_hlc.to_string(),
            device_id,
            chrono::Utc::now().to_rfc3339()
        ],
    )?;

    // 7. Update entry_hlc in ledger_entries for the extracted entries
    for entry in &bundle.manifest.entries {
        conn.execute(
            "UPDATE ledger_entries SET entry_hlc = ?1 WHERE tx_id = ?2",
            params![entry.entry_hlc.to_string(), entry.tx_id],
        )?;
    }

    Ok(bundle)
}
