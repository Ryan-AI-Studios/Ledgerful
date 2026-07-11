use crate::ledger::error::LedgerError;
use rusqlite::Connection;

pub fn get_federated_entries_by_entity(
    conn: &Connection,
    entity_normalized: &str,
    sibling_name: &str,
    days: u64,
) -> Result<Vec<crate::ledger::types::LedgerEntry>, LedgerError> {
    let mut stmt = conn.prepare(
        "SELECT id, tx_id, category, entry_type, entity, entity_normalized,
            change_type, summary, reason, is_breaking, committed_at,
            verification_status, verification_basis, outcome_notes,
            origin, trace_id, signature, public_key, risk, related_tickets, author, observed
     FROM ledger_entries
     WHERE entity_normalized = ?1
       AND origin = 'SIBLING'
       AND trace_id = ?2
       AND committed_at >= strftime('%Y-%m-%dT%H:%M:%SZ', 'now', ?3)
     ORDER BY committed_at DESC",
    )?;

    let delta = format!("-{} days", days);
    let rows = stmt.query_map([entity_normalized, sibling_name, &delta], |row| {
        super::map_ledger_entry(row)
    })?;

    let mut entries = Vec::new();
    for entry in rows {
        entries.push(entry?);
    }
    Ok(entries)
}
