use crate::ledger::error::LedgerError;
use crate::ledger::types::*;
use rusqlite::{Connection, OptionalExtension, params};

pub fn insert_transaction(conn: &Connection, tx: &Transaction) -> Result<(), LedgerError> {
    conn.execute(
        "INSERT INTO transactions (
            tx_id, operation_id, status, category, entity, entity_normalized,
            planned_action, session_id, source, started_at, resolved_at, issue_ref,
            detected_at, drift_count, first_seen_at, last_seen_at, snapshot_id
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
        params![
            tx.tx_id,
            tx.operation_id,
            tx.status,
            serde_json::to_string(&tx.category)
                .map_err(|e| LedgerError::Config(e.to_string()))?
                .trim_matches('"'),
            tx.entity,
            tx.entity_normalized,
            tx.planned_action,
            tx.session_id,
            tx.source,
            tx.started_at,
            tx.resolved_at,
            tx.issue_ref,
            tx.detected_at,
            tx.drift_count,
            tx.first_seen_at,
            tx.last_seen_at,
            tx.snapshot_id,
        ],
    )?;
    Ok(())
}

pub fn get_transaction(conn: &Connection, tx_id: &str) -> Result<Option<Transaction>, LedgerError> {
    conn.query_row(
        "SELECT tx_id, operation_id, status, category, entity, entity_normalized,
            planned_action, session_id, source, started_at, resolved_at, issue_ref,
            detected_at, drift_count, first_seen_at, last_seen_at, snapshot_id
     FROM transactions WHERE tx_id = ?1",
        [tx_id],
        map_transaction,
    )
    .optional()
    .map_err(LedgerError::from)
}

pub fn get_pending_by_entity(
    conn: &Connection,
    entity_normalized: &str,
) -> Result<Option<Transaction>, LedgerError> {
    conn.query_row(
        "SELECT tx_id, operation_id, status, category, entity, entity_normalized,
            planned_action, session_id, source, started_at, resolved_at, issue_ref,
            detected_at, drift_count, first_seen_at, last_seen_at, snapshot_id
     FROM transactions WHERE entity_normalized = ?1 AND status = 'PENDING'",
        [entity_normalized],
        map_transaction,
    )
    .optional()
    .map_err(LedgerError::from)
}

pub fn get_unaudited_by_entity(
    conn: &Connection,
    entity_normalized: &str,
) -> Result<Option<Transaction>, LedgerError> {
    conn.query_row(
        "SELECT tx_id, operation_id, status, category, entity, entity_normalized,
            planned_action, session_id, source, started_at, resolved_at, issue_ref,
            detected_at, drift_count, first_seen_at, last_seen_at, snapshot_id
     FROM transactions WHERE entity_normalized = ?1 AND status = 'UNAUDITED'",
        [entity_normalized],
        map_transaction,
    )
    .optional()
    .map_err(LedgerError::from)
}

pub fn upsert_unaudited_transaction(
    conn: &Connection,
    tx: &Transaction,
) -> Result<(), LedgerError> {
    conn.execute(
        "INSERT INTO transactions (
            tx_id, status, category, entity, entity_normalized, session_id, source,
            started_at, detected_at, drift_count, first_seen_at, last_seen_at, snapshot_id
        ) VALUES (?1, 'UNAUDITED', ?2, ?3, ?4, ?5, 'WATCHER', ?6, ?7, 1, ?8, ?9, ?10)
        ON CONFLICT(entity_normalized) WHERE status = 'UNAUDITED' DO UPDATE SET
            drift_count = drift_count + 1,
            last_seen_at = EXCLUDED.last_seen_at",
        params![
            tx.tx_id,
            serde_json::to_string(&tx.category)
                .map_err(|e| LedgerError::Config(e.to_string()))?
                .trim_matches('"'),
            tx.entity,
            tx.entity_normalized,
            tx.session_id,
            tx.started_at,
            tx.detected_at,
            tx.first_seen_at,
            tx.last_seen_at,
            tx.snapshot_id,
        ],
    )?;
    Ok(())
}

pub fn update_transaction_status(
    conn: &Connection,
    tx_id: &str,
    status: &str,
    resolved_at: Option<&str>,
) -> Result<usize, LedgerError> {
    let count = conn.execute(
        "UPDATE transactions SET status = ?1, resolved_at = ?2 WHERE tx_id = ?3 AND status = 'PENDING'",
        params![status, resolved_at, tx_id],
    )?;
    Ok(count)
}

/// Promote a pending transaction to committed, also linking it to the staged
/// snapshot captured by the commit-msg hook so `changed_files` can be joined.
pub fn commit_transaction(
    conn: &Connection,
    tx_id: &str,
    status: &str,
    resolved_at: Option<&str>,
    snapshot_id: Option<i64>,
) -> Result<usize, LedgerError> {
    let count = conn.execute(
        "UPDATE transactions
         SET status = ?1, resolved_at = ?2, snapshot_id = ?3
         WHERE tx_id = ?4 AND status = 'PENDING'",
        params![status, resolved_at, snapshot_id, tx_id],
    )?;
    Ok(count)
}

pub fn update_transaction_status_bulk(
    conn: &Connection,
    tx_ids: &[String],
    status: &str,
    expected_status: &str,
    resolved_at: Option<&str>,
) -> Result<usize, LedgerError> {
    if tx_ids.is_empty() {
        return Ok(0);
    }
    let placeholders: Vec<String> = tx_ids.iter().map(|_| "?".to_string()).collect();
    let sql = format!(
        "UPDATE transactions SET status = ?1, resolved_at = ?2 WHERE status = ?3 AND tx_id IN ({})",
        placeholders.join(",")
    );
    let mut params: Vec<&dyn rusqlite::ToSql> = vec![&status, &resolved_at, &expected_status];
    for id in tx_ids {
        params.push(id);
    }
    let count = conn.execute(&sql, rusqlite::params_from_iter(params))?;
    Ok(count)
}

pub fn get_all_pending(conn: &Connection) -> Result<Vec<Transaction>, LedgerError> {
    let mut stmt = conn.prepare(
        "SELECT tx_id, operation_id, status, category, entity, entity_normalized,
            planned_action, session_id, source, started_at, resolved_at, issue_ref,
            detected_at, drift_count, first_seen_at, last_seen_at, snapshot_id
     FROM transactions WHERE status = 'PENDING' ORDER BY started_at DESC",
    )?;

    let rows = stmt.query_map([], map_transaction)?;
    let mut entries = Vec::new();
    for entry in rows {
        entries.push(entry?);
    }
    Ok(entries)
}

pub fn get_all_unaudited(conn: &Connection) -> Result<Vec<Transaction>, LedgerError> {
    let mut stmt = conn.prepare(
        "SELECT tx_id, operation_id, status, category, entity, entity_normalized,
            planned_action, session_id, source, started_at, resolved_at, issue_ref,
            detected_at, drift_count, first_seen_at, last_seen_at, snapshot_id
     FROM transactions WHERE status = 'UNAUDITED' ORDER BY last_seen_at DESC",
    )?;

    let rows = stmt.query_map([], map_transaction)?;
    let mut entries = Vec::new();
    for entry in rows {
        entries.push(entry?);
    }
    Ok(entries)
}

pub fn get_unaudited_by_pattern(
    conn: &Connection,
    pattern: &str,
) -> Result<Vec<Transaction>, LedgerError> {
    let sql_pattern = pattern.replace('*', "%");
    let mut stmt = conn.prepare(
        "SELECT tx_id, operation_id, status, category, entity, entity_normalized,
            planned_action, session_id, source, started_at, resolved_at, issue_ref,
            detected_at, drift_count, first_seen_at, last_seen_at, snapshot_id
     FROM transactions WHERE status = 'UNAUDITED' AND entity_normalized LIKE ?1",
    )?;

    let rows = stmt.query_map([sql_pattern], map_transaction)?;
    let mut entries = Vec::new();
    for entry in rows {
        entries.push(entry?);
    }
    Ok(entries)
}

pub fn resolve_tx_id_fuzzy(conn: &Connection, prefix: &str) -> Result<Vec<String>, LedgerError> {
    let sql_prefix = format!("{}%", prefix.replace('_', "\\_").replace('%', "\\%"));
    let mut stmt =
        conn.prepare("SELECT tx_id FROM transactions WHERE tx_id LIKE ?1 ESCAPE '\\'")?;

    let rows = stmt.query_map([sql_prefix], |row| row.get(0))?;
    let mut matches = Vec::new();
    for m in rows {
        matches.push(m?);
    }
    Ok(matches)
}

pub fn map_transaction(row: &rusqlite::Row) -> rusqlite::Result<Transaction> {
    let cat_str: String = row.get(3)?;
    let category: Category = serde_json::from_str(&format!("\"{}\"", cat_str))
        .map_err(|_| rusqlite::Error::InvalidQuery)?;

    Ok(Transaction {
        tx_id: row.get(0)?,
        operation_id: row.get(1)?,
        status: row.get(2)?,
        category,
        entity: row.get(4)?,
        entity_normalized: row.get(5)?,
        planned_action: row.get(6)?,
        session_id: row.get(7)?,
        source: row.get(8)?,
        started_at: row.get(9)?,
        resolved_at: row.get(10)?,
        issue_ref: row.get(11)?,
        detected_at: row.get(12)?,
        drift_count: row.get(13)?,
        first_seen_at: row.get(14)?,
        last_seen_at: row.get(15)?,
        snapshot_id: row.get(16)?,
    })
}

// Ledger entries (transaction outcomes)

pub fn insert_ledger_entry(conn: &Connection, entry: &LedgerEntry) -> Result<(), LedgerError> {
    conn.execute(
        "INSERT INTO ledger_entries (
            tx_id, category, entry_type, entity, entity_normalized,
            change_type, summary, reason, is_breaking, committed_at,
            verification_status, verification_basis, outcome_notes,
            origin, trace_id, signature, public_key, risk, related_tickets, author
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20)",
        params![
            entry.tx_id,
            serde_json::to_string(&entry.category)
                .map_err(|e| LedgerError::Config(e.to_string()))?
                .trim_matches('"'),
            serde_json::to_string(&entry.entry_type)
                .map_err(|e| LedgerError::Config(e.to_string()))?
                .trim_matches('"'),
            entry.entity,
            entry.entity_normalized,
            serde_json::to_string(&entry.change_type)
                .map_err(|e| LedgerError::Config(e.to_string()))?
                .trim_matches('"'),
            entry.summary,
            entry.reason,
            entry.is_breaking as i32,
            entry.committed_at,
            entry
                .verification_status
                .map(|s| {
                    serde_json::to_string(&s)
                        .map(|json| json.trim_matches('"').to_string())
                        .map_err(|e| LedgerError::Config(e.to_string()))
                })
                .transpose()?,
            entry
                .verification_basis
                .map(|b| {
                    serde_json::to_string(&b)
                        .map(|json| json.trim_matches('"').to_string())
                        .map_err(|e| LedgerError::Config(e.to_string()))
                })
                .transpose()?,
            entry.outcome_notes,
            entry.origin,
            entry.trace_id,
            entry.signature,
            entry.public_key,
            entry.risk,
            entry.related_tickets,
            entry.author,
        ],
    )?;
    Ok(())
}

pub fn get_ledger_entries_for_tx(
    conn: &Connection,
    tx_id: &str,
) -> Result<Vec<LedgerEntry>, LedgerError> {
    let mut stmt = conn.prepare(
        "SELECT id, tx_id, category, entry_type, entity, entity_normalized,
            change_type, summary, reason, is_breaking, committed_at,
            verification_status, verification_basis, outcome_notes,
            origin, trace_id, signature, public_key, risk, related_tickets, author
     FROM ledger_entries WHERE tx_id = ?1",
    )?;

    let rows = stmt.query_map([tx_id], super::map_ledger_entry)?;

    let mut entries = Vec::new();
    for entry in rows {
        entries.push(entry?);
    }
    Ok(entries)
}

pub fn get_ledger_entries_by_entity_paginated(
    conn: &Connection,
    entity_normalized: &str,
    limit: usize,
    offset: usize,
) -> Result<Vec<LedgerEntry>, LedgerError> {
    let mut stmt = conn.prepare(
        "SELECT id, tx_id, category, entry_type, entity, entity_normalized,
            change_type, summary, reason, is_breaking, committed_at,
            verification_status, verification_basis, outcome_notes,
            origin, trace_id, signature, public_key, risk, related_tickets, author
     FROM ledger_entries WHERE entity_normalized = ?1
     ORDER BY committed_at DESC
     LIMIT ?2 OFFSET ?3",
    )?;

    let rows = stmt.query_map(
        params![entity_normalized, limit as i64, offset as i64],
        super::map_ledger_entry,
    )?;

    let mut entries = Vec::new();
    for entry in rows {
        entries.push(entry?);
    }
    Ok(entries)
}

pub fn get_all_committed_ledger_entries(
    conn: &Connection,
) -> Result<Vec<LedgerEntry>, LedgerError> {
    let mut stmt = conn.prepare(
        "SELECT id, tx_id, category, entry_type, entity, entity_normalized,
            change_type, summary, reason, is_breaking, committed_at,
            verification_status, verification_basis, outcome_notes,
            origin, trace_id, signature, public_key, risk, related_tickets, author
     FROM ledger_entries ORDER BY committed_at ASC",
    )?;

    let rows = stmt.query_map([], super::map_ledger_entry)?;

    let mut entries = Vec::new();
    for entry in rows {
        entries.push(entry?);
    }
    Ok(entries)
}

pub fn update_ledger_entry_signature(
    conn: &Connection,
    tx_id: &str,
    signature: &str,
    public_key: &str,
) -> Result<usize, LedgerError> {
    let count = conn.execute(
        "UPDATE ledger_entries
         SET signature = ?1, public_key = ?2
         WHERE tx_id = ?3",
        params![signature, public_key, tx_id],
    )?;
    Ok(count)
}

pub fn get_committed_ledger_entries_paginated(
    conn: &Connection,
    category: Option<&str>,
    limit: usize,
    offset: usize,
) -> Result<Vec<LedgerEntry>, LedgerError> {
    let sql = if category.is_some() {
        "SELECT id, tx_id, category, entry_type, entity, entity_normalized,
            change_type, summary, reason, is_breaking, committed_at,
            verification_status, verification_basis, outcome_notes,
            origin, trace_id, signature, public_key, risk, related_tickets, author
         FROM ledger_entries
         WHERE category = ?1
         ORDER BY committed_at ASC
         LIMIT ?2 OFFSET ?3"
    } else {
        "SELECT id, tx_id, category, entry_type, entity, entity_normalized,
            change_type, summary, reason, is_breaking, committed_at,
            verification_status, verification_basis, outcome_notes,
            origin, trace_id, signature, public_key, risk, related_tickets, author
         FROM ledger_entries
         ORDER BY committed_at ASC
         LIMIT ?1 OFFSET ?2"
    };

    let mut stmt = conn.prepare(sql)?;
    let rows = if let Some(cat) = category {
        stmt.query_map(
            rusqlite::params![cat, limit as i64, offset as i64],
            super::map_ledger_entry,
        )?
    } else {
        stmt.query_map(
            rusqlite::params![limit as i64, offset as i64],
            super::map_ledger_entry,
        )?
    };

    let mut entries = Vec::new();
    for entry in rows {
        entries.push(entry?);
    }
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger::types::{Category, ChangeType, EntryType, LedgerEntry, Transaction};
    use rusqlite::Connection;

    fn setup_in_memory_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE transactions (
                tx_id TEXT PRIMARY KEY,
                operation_id TEXT,
                status TEXT NOT NULL,
                category TEXT NOT NULL,
                entity TEXT NOT NULL,
                entity_normalized TEXT NOT NULL,
                planned_action TEXT,
                session_id TEXT NOT NULL,
                source TEXT NOT NULL DEFAULT 'CLI',
                started_at TEXT NOT NULL,
                resolved_at TEXT,
                detected_at TEXT,
                drift_count INTEGER DEFAULT 1,
                first_seen_at TEXT,
                last_seen_at TEXT,
                issue_ref TEXT,
                snapshot_id INTEGER
            );
            CREATE UNIQUE INDEX idx_transactions_unaudited_entity ON transactions(entity_normalized) WHERE status = 'UNAUDITED';
            CREATE UNIQUE INDEX idx_transactions_pending_entity ON transactions(entity_normalized) WHERE status = 'PENDING';
            CREATE TABLE ledger_entries (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                tx_id TEXT NOT NULL,
                category TEXT NOT NULL,
                entry_type TEXT NOT NULL DEFAULT 'IMPLEMENTATION',
                entity TEXT NOT NULL,
                entity_normalized TEXT NOT NULL,
                change_type TEXT NOT NULL,
                summary TEXT NOT NULL,
                reason TEXT NOT NULL,
                is_breaking INTEGER DEFAULT 0,
                committed_at TEXT NOT NULL,
                verification_status TEXT,
                verification_basis TEXT,
                outcome_notes TEXT,
                origin TEXT NOT NULL DEFAULT 'LOCAL',
                trace_id TEXT,
                signature TEXT,
                public_key TEXT,
                risk TEXT,
                related_tickets TEXT,
                author TEXT NOT NULL DEFAULT 'unknown'
            );",
        )
        .unwrap();
        conn
    }

    fn sample_tx(entity: &str, status: &str) -> Transaction {
        Transaction {
            tx_id: uuid::Uuid::new_v4().to_string(),
            operation_id: None,
            status: status.to_string(),
            category: Category::Feature,
            entity: entity.to_string(),
            entity_normalized: entity.to_string(),
            planned_action: None,
            session_id: "test".to_string(),
            source: "CLI".to_string(),
            started_at: chrono::Utc::now().to_rfc3339(),
            resolved_at: None,
            detected_at: None,
            drift_count: 1,
            first_seen_at: None,
            last_seen_at: None,
            issue_ref: None,
            snapshot_id: None,
        }
    }

    #[test]
    fn test_pending_conflict() {
        let conn = setup_in_memory_db();
        let tx1 = sample_tx("src/main.rs", "PENDING");
        insert_transaction(&conn, &tx1).unwrap();

        // Same entity should violate unique index if status is PENDING
        let tx2 = sample_tx("src/main.rs", "PENDING");
        let result = insert_transaction(&conn, &tx2);
        assert!(
            result.is_err(),
            "expected unique constraint violation for duplicate pending"
        );
    }

    #[test]
    fn test_commit_promotion() {
        let conn = setup_in_memory_db();
        let tx = sample_tx("src/main.rs", "PENDING");
        insert_transaction(&conn, &tx).unwrap();

        let now = chrono::Utc::now().to_rfc3339();
        let count = update_transaction_status(&conn, &tx.tx_id, "COMMITTED", Some(&now)).unwrap();
        assert_eq!(count, 1);

        let updated = get_transaction(&conn, &tx.tx_id).unwrap();
        assert_eq!(updated.unwrap().status, "COMMITTED");
    }

    #[test]
    fn test_rollback() {
        let conn = setup_in_memory_db();
        let tx = sample_tx("src/main.rs", "PENDING");
        insert_transaction(&conn, &tx).unwrap();

        let now = chrono::Utc::now().to_rfc3339();
        let count = update_transaction_status(&conn, &tx.tx_id, "ROLLED_BACK", Some(&now)).unwrap();
        assert_eq!(count, 1);

        let updated = get_transaction(&conn, &tx.tx_id).unwrap();
        assert_eq!(updated.unwrap().status, "ROLLED_BACK");
    }

    #[test]
    fn test_ledger_entry_roundtrip() {
        let conn = setup_in_memory_db();
        let tx = sample_tx("src/main.rs", "PENDING");
        insert_transaction(&conn, &tx).unwrap();

        let entry = LedgerEntry {
            id: 0,
            tx_id: tx.tx_id.clone(),
            category: Category::Feature,
            entry_type: EntryType::Implementation,
            entity: "src/main.rs".to_string(),
            entity_normalized: "src/main.rs".to_string(),
            change_type: ChangeType::Modify,
            summary: "test".to_string(),
            reason: "reason".to_string(),
            is_breaking: false,
            committed_at: chrono::Utc::now().to_rfc3339(),
            verification_status: None,
            verification_basis: None,
            outcome_notes: None,
            origin: "LOCAL".to_string(),
            trace_id: None,
            signature: None,
            public_key: None,
            risk: None,
            related_tickets: None,
            author: "Test User".to_string(),
        };
        insert_ledger_entry(&conn, &entry).unwrap();

        let entries = get_ledger_entries_for_tx(&conn, &tx.tx_id).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].summary, "test");
    }
}
