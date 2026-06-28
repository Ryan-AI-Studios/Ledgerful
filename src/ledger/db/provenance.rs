use crate::ledger::error::LedgerError;
use crate::ledger::provenance::{ProvenanceAction, TokenProvenance};
use rusqlite::{Connection, params};
use std::str::FromStr;

pub fn insert_token_provenance(
    conn: &Connection,
    prov: &TokenProvenance,
) -> Result<(), LedgerError> {
    conn.execute(
        "INSERT INTO token_provenance (
            tx_id, entity, entity_normalized, symbol_name, symbol_type, action
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            prov.tx_id,
            prov.entity,
            prov.entity_normalized,
            prov.symbol_name,
            prov.symbol_type,
            prov.action.to_string(),
        ],
    )?;
    Ok(())
}

pub fn get_token_provenance_for_tx(
    conn: &Connection,
    tx_id: &str,
) -> Result<Vec<TokenProvenance>, LedgerError> {
    let mut stmt = conn.prepare(
        "SELECT id, tx_id, entity, entity_normalized, symbol_name, symbol_type, action
         FROM token_provenance WHERE tx_id = ?1",
    )?;

    let rows = stmt.query_map([tx_id], map_token_provenance)?;

    let mut entries = Vec::new();
    for entry in rows {
        entries.push(entry?);
    }
    Ok(entries)
}

pub fn get_token_provenance_by_entity(
    conn: &Connection,
    entity_normalized: &str,
) -> Result<Vec<TokenProvenance>, LedgerError> {
    let mut stmt = conn.prepare(
        "SELECT id, tx_id, entity, entity_normalized, symbol_name, symbol_type, action
         FROM token_provenance WHERE entity_normalized = ?1
         ORDER BY id DESC",
    )?;

    let rows = stmt.query_map([entity_normalized], map_token_provenance)?;

    let mut entries = Vec::new();
    for entry in rows {
        entries.push(entry?);
    }
    Ok(entries)
}

pub fn find_transactions_by_file(
    conn: &Connection,
    file_path: &str,
) -> Result<Vec<crate::ledger::types::LedgerEntry>, LedgerError> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT l.id, l.tx_id, l.category, l.entry_type, l.entity, l.entity_normalized,
            l.change_type, l.summary, l.reason, l.is_breaking, l.committed_at,
            l.verification_status, l.verification_basis, l.outcome_notes,
            l.origin, l.trace_id, l.signature, l.public_key, l.risk, l.related_tickets, l.author
     FROM ledger_entries l
     JOIN token_provenance tp ON l.tx_id = tp.tx_id
     WHERE tp.entity = ?1 OR tp.entity LIKE ?2
     ORDER BY l.committed_at DESC",
    )?;

    let like_pattern = format!("%{}", file_path);
    let rows = stmt.query_map(params![file_path, like_pattern], super::map_ledger_entry)?;

    let mut entries = Vec::new();
    for entry in rows {
        entries.push(entry?);
    }
    Ok(entries)
}

fn map_token_provenance(row: &rusqlite::Row) -> rusqlite::Result<TokenProvenance> {
    let action_str: String = row.get(6)?;
    let action =
        ProvenanceAction::from_str(&action_str).map_err(|_| rusqlite::Error::InvalidQuery)?;

    Ok(TokenProvenance {
        id: Some(row.get(0)?),
        tx_id: row.get(1)?,
        entity: row.get(2)?,
        entity_normalized: row.get(3)?,
        symbol_name: row.get(4)?,
        symbol_type: row.get(5)?,
        action,
    })
}
