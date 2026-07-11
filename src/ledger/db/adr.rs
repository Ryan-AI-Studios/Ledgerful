use crate::ledger::error::LedgerError;
use crate::ledger::types::*;
use rusqlite::{Connection, OptionalExtension, params};

pub fn get_adr_entries(
    conn: &Connection,
    days: Option<u64>,
) -> Result<Vec<LedgerEntry>, LedgerError> {
    let mut sql = "SELECT id, tx_id, category, entry_type, entity, entity_normalized,
            change_type, summary, reason, is_breaking, committed_at,
            verification_status, verification_basis, outcome_notes,
            origin, trace_id, signature, public_key, risk, related_tickets, author, observed
     FROM ledger_entries WHERE (entry_type = 'ARCHITECTURE' OR is_breaking = 1)"
        .to_string();

    if let Some(d) = days {
        sql.push_str(&format!(
            " AND committed_at >= strftime('%Y-%m-%dT%H:%M:%SZ', 'now', '-{} days')",
            d
        ));
    }
    sql.push_str(" ORDER BY committed_at DESC");

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], super::map_ledger_entry)?;

    let mut entries = Vec::new();
    for entry in rows {
        entries.push(entry?);
    }
    Ok(entries)
}

pub fn get_oldest_adr(conn: &Connection) -> Result<Option<LedgerEntry>, LedgerError> {
    conn.query_row(
        "SELECT id, tx_id, category, entry_type, entity, entity_normalized,
            change_type, summary, reason, is_breaking, committed_at,
            verification_status, verification_basis, outcome_notes,
            origin, trace_id, NULL, NULL, NULL, NULL, NULL, NULL
     FROM ledger_entries
     WHERE category = 'ARCHITECTURE'
     ORDER BY committed_at ASC LIMIT 1",
        [],
        super::map_ledger_entry,
    )
    .optional()
    .map_err(LedgerError::from)
}

pub fn update_adr_metadata(
    conn: &Connection,
    adr_id: &str,
    update: AdrMetadataUpdate,
) -> Result<(), LedgerError> {
    let now = chrono::Utc::now().to_rfc3339();

    let existing = get_adr_metadata(conn, adr_id)?;
    let mut metadata = existing.unwrap_or_else(|| AdrMetadata {
        adr_id: adr_id.to_string(),
        ..Default::default()
    });

    if let Some(s) = update.status {
        metadata.status = s;
    }
    if let Some(o) = update.owner {
        metadata.owner = Some(o);
    }
    if let Some(r) = update.reviewers {
        metadata.reviewers = Some(r);
    }
    if let Some(s) = update.supersedes {
        metadata.supersedes = Some(s);
    }
    if let Some(sb) = update.superseded_by {
        metadata.superseded_by = Some(sb);
    }
    if let Some(ae) = update.affected_entities {
        metadata.affected_entities = Some(ae);
    }
    if let Some(ds) = update.decision_scope {
        metadata.decision_scope = Some(ds);
    }
    if let Some(ra) = update.reviewed_at {
        metadata.reviewed_at = Some(ra);
    }
    if let Some(rid) = update.review_interval_days {
        metadata.review_interval_days = Some(rid);
    }

    conn.execute(
        "INSERT INTO adr_metadata (
            adr_id, status, owner, reviewers, supersedes, superseded_by,
            affected_entities, decision_scope, reviewed_at, review_interval_days, last_updated_at
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
        ON CONFLICT(adr_id) DO UPDATE SET
            status = EXCLUDED.status,
            owner = EXCLUDED.owner,
            reviewers = EXCLUDED.reviewers,
            supersedes = EXCLUDED.supersedes,
            superseded_by = EXCLUDED.superseded_by,
            affected_entities = EXCLUDED.affected_entities,
            decision_scope = EXCLUDED.decision_scope,
            reviewed_at = EXCLUDED.reviewed_at,
            review_interval_days = EXCLUDED.review_interval_days,
            last_updated_at = EXCLUDED.last_updated_at",
        params![
            metadata.adr_id,
            serde_json::to_string(&metadata.status)
                .map_err(|e| LedgerError::Config(e.to_string()))?
                .trim_matches('"'),
            metadata.owner,
            metadata.reviewers,
            metadata.supersedes,
            metadata.superseded_by,
            metadata.affected_entities,
            metadata.decision_scope,
            metadata.reviewed_at,
            metadata.review_interval_days,
            now,
        ],
    )?;
    Ok(())
}

pub fn get_adr_metadata(
    conn: &Connection,
    adr_id: &str,
) -> Result<Option<AdrMetadata>, LedgerError> {
    conn.query_row(
        "SELECT adr_id, status, owner, reviewers, supersedes, superseded_by,
            affected_entities, decision_scope, reviewed_at, review_interval_days
         FROM adr_metadata WHERE adr_id = ?1",
        [adr_id],
        |row| {
            let status_str: String = row.get(1)?;
            let status: AdrStatus = serde_json::from_str(&format!("\"{}\"", status_str))
                .map_err(|_| rusqlite::Error::InvalidQuery)?;

            Ok(AdrMetadata {
                adr_id: row.get(0)?,
                status,
                owner: row.get(2)?,
                reviewers: row.get(3)?,
                supersedes: row.get(4)?,
                superseded_by: row.get(5)?,
                affected_entities: row.get(6)?,
                decision_scope: row.get(7)?,
                reviewed_at: row.get(8)?,
                review_interval_days: row.get(9)?,
            })
        },
    )
    .optional()
    .map_err(LedgerError::from)
}

pub fn link_adr_supersedes(
    conn: &Connection,
    adr_id: &str,
    supersedes_id: &str,
) -> Result<(), LedgerError> {
    update_adr_metadata(
        conn,
        adr_id,
        AdrMetadataUpdate {
            supersedes: Some(supersedes_id.to_string()),
            ..Default::default()
        },
    )?;

    update_adr_metadata(
        conn,
        supersedes_id,
        AdrMetadataUpdate {
            status: Some(AdrStatus::Superseded),
            superseded_by: Some(adr_id.to_string()),
            ..Default::default()
        },
    )?;

    Ok(())
}
