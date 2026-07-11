use crate::ledger::error::LedgerError;
use rusqlite::Connection;

pub fn get_stale_pending_transactions(
    conn: &Connection,
    ttl_days: u64,
) -> Result<Vec<String>, LedgerError> {
    let threshold = (chrono::Utc::now() - chrono::Duration::days(ttl_days as i64)).to_rfc3339();
    let mut stmt = conn
        .prepare("SELECT tx_id FROM transactions WHERE status = 'PENDING' AND started_at < ?1")?;
    let ids = stmt
        .query_map([threshold], |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<String>>>()?;
    Ok(ids)
}

pub fn delete_stale_pending_transactions(
    conn: &Connection,
    ttl_days: u64,
) -> Result<usize, LedgerError> {
    let threshold = (chrono::Utc::now() - chrono::Duration::days(ttl_days as i64)).to_rfc3339();
    let count = conn.execute(
        "DELETE FROM transactions WHERE status = 'PENDING' AND started_at < ?1",
        rusqlite::params![threshold],
    )?;
    Ok(count)
}

pub fn get_transaction_velocity(conn: &Connection, days: u64) -> Result<usize, LedgerError> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM ledger_entries WHERE committed_at >= strftime('%Y-%m-%dT%H:%M:%SZ', 'now', ?1)",
        [format!("-{} days", days)],
        |row| row.get(0),
    )?;
    Ok(count as usize)
}

pub fn get_top_churned_entities(
    conn: &Connection,
    limit: usize,
) -> Result<Vec<(String, usize)>, LedgerError> {
    let mut stmt = conn.prepare(
        "SELECT entity, COUNT(*) as churn FROM ledger_entries GROUP BY entity ORDER BY churn DESC LIMIT ?1"
    )?;
    let rows = stmt.query_map([limit as i64], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)? as usize))
    })?;
    let mut results = Vec::new();
    for res in rows {
        results.push(res?);
    }
    Ok(results)
}

pub fn get_recent_ledger_entries_paginated(
    conn: &Connection,
    limit: usize,
    offset: usize,
) -> Result<Vec<crate::ledger::types::LedgerEntry>, LedgerError> {
    let mut stmt = conn.prepare(
        "SELECT id, tx_id, category, entry_type, entity, entity_normalized,
            change_type, summary, reason, is_breaking, committed_at,
            verification_status, verification_basis, outcome_notes,
            origin, trace_id, signature, public_key, risk, related_tickets, author, observed, prev_hash
     FROM ledger_entries
     ORDER BY committed_at DESC
     LIMIT ?1 OFFSET ?2",
    )?;

    let rows = stmt.query_map(rusqlite::params![limit as i64, offset as i64], |row| {
        super::map_ledger_entry(row)
    })?;

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
                author TEXT NOT NULL DEFAULT 'unknown',
                observed INTEGER,
                prev_hash TEXT
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

    fn sample_entry(tx_id: &str, entity: &str) -> LedgerEntry {
        LedgerEntry {
            id: 0,
            tx_id: tx_id.to_string(),
            category: Category::Feature,
            entry_type: EntryType::Implementation,
            entity: entity.to_string(),
            entity_normalized: entity.to_string(),
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
            observed: None,
            prev_hash: None,
        }
    }

    #[test]
    fn test_transaction_velocity() {
        let conn = setup_in_memory_db();
        let tx = sample_tx("a.rs", "PENDING");
        crate::ledger::db::transactions::insert_transaction(&conn, &tx).unwrap();
        crate::ledger::db::transactions::insert_ledger_entry(
            &conn,
            &sample_entry(&tx.tx_id, "a.rs"),
        )
        .unwrap();

        let v = get_transaction_velocity(&conn, 7).unwrap();
        assert_eq!(v, 1);
    }

    #[test]
    fn test_top_churned_entities() {
        let conn = setup_in_memory_db();
        let tx1 = sample_tx("a.rs", "PENDING");
        let tx2 = sample_tx("b.rs", "PENDING");
        crate::ledger::db::transactions::insert_transaction(&conn, &tx1).unwrap();
        crate::ledger::db::transactions::insert_transaction(&conn, &tx2).unwrap();
        crate::ledger::db::transactions::insert_ledger_entry(
            &conn,
            &sample_entry(&tx1.tx_id, "a.rs"),
        )
        .unwrap();
        crate::ledger::db::transactions::insert_ledger_entry(
            &conn,
            &sample_entry(&tx1.tx_id, "a.rs"),
        )
        .unwrap();
        crate::ledger::db::transactions::insert_ledger_entry(
            &conn,
            &sample_entry(&tx2.tx_id, "b.rs"),
        )
        .unwrap();

        let churn = get_top_churned_entities(&conn, 10).unwrap();
        assert_eq!(churn.len(), 2);
        assert_eq!(churn[0], ("a.rs".to_string(), 2));
        assert_eq!(churn[1], ("b.rs".to_string(), 1));
    }

    #[test]
    fn test_stale_pending_ttl() {
        let conn = setup_in_memory_db();
        let old = Transaction {
            tx_id: uuid::Uuid::new_v4().to_string(),
            operation_id: None,
            status: "PENDING".to_string(),
            category: Category::Feature,
            entity: "old.rs".to_string(),
            entity_normalized: "old.rs".to_string(),
            planned_action: None,
            session_id: "test".to_string(),
            source: "CLI".to_string(),
            started_at: (chrono::Utc::now() - chrono::Duration::days(10)).to_rfc3339(),
            resolved_at: None,
            detected_at: None,
            drift_count: 1,
            first_seen_at: None,
            last_seen_at: None,
            issue_ref: None,
            snapshot_id: None,
        };
        let new = sample_tx("new.rs", "PENDING");
        crate::ledger::db::transactions::insert_transaction(&conn, &old).unwrap();
        crate::ledger::db::transactions::insert_transaction(&conn, &new).unwrap();

        let stale = get_stale_pending_transactions(&conn, 7).unwrap();
        assert_eq!(stale.len(), 1);
        assert_eq!(stale[0], old.tx_id);
    }
}
