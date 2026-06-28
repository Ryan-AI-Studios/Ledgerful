use crate::ledger::error::LedgerError;
use crate::ledger::types::*;
use rusqlite::Connection;

pub fn get_unaudited_by_entity(
    conn: &Connection,
    entity_normalized: &str,
) -> Result<Option<Transaction>, LedgerError> {
    crate::ledger::db::transactions::get_unaudited_by_entity(conn, entity_normalized)
}

pub fn upsert_unaudited_transaction(
    conn: &Connection,
    tx: &Transaction,
) -> Result<(), LedgerError> {
    crate::ledger::db::transactions::upsert_unaudited_transaction(conn, tx)
}

pub fn get_all_unaudited(conn: &Connection) -> Result<Vec<Transaction>, LedgerError> {
    crate::ledger::db::transactions::get_all_unaudited(conn)
}

pub fn get_unaudited_by_pattern(
    conn: &Connection,
    pattern: &str,
) -> Result<Vec<Transaction>, LedgerError> {
    crate::ledger::db::transactions::get_unaudited_by_pattern(conn, pattern)
}

/// Return drift status counts: (pending_count, unaudited_count).
pub fn drift_status_counts(conn: &Connection) -> Result<(usize, usize), LedgerError> {
    let pending: i64 = conn.query_row(
        "SELECT COUNT(*) FROM transactions WHERE status = 'PENDING'",
        [],
        |row| row.get(0),
    )?;
    let unaudited: i64 = conn.query_row(
        "SELECT COUNT(*) FROM transactions WHERE status = 'UNAUDITED'",
        [],
        |row| row.get(0),
    )?;
    Ok((pending as usize, unaudited as usize))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger::types::{Category, Transaction};
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
                issue_ref TEXT
            );
            CREATE UNIQUE INDEX idx_transactions_unaudited_entity ON transactions(entity_normalized) WHERE status = 'UNAUDITED';"
        ).unwrap();
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
        }
    }

    #[test]
    fn test_drift_lifecycle() {
        let conn = setup_in_memory_db();
        let tx = sample_tx("src/main.rs", "UNAUDITED");

        upsert_unaudited_transaction(&conn, &tx).unwrap();
        let found = get_unaudited_by_entity(&conn, "src/main.rs").unwrap();
        assert!(found.is_some());
        assert_eq!(found.unwrap().drift_count, 1);

        // Upsert again should increment drift_count
        let tx2 = sample_tx("src/main.rs", "UNAUDITED");
        upsert_unaudited_transaction(&conn, &tx2).unwrap();
        let found2 = get_unaudited_by_entity(&conn, "src/main.rs").unwrap();
        assert_eq!(found2.unwrap().drift_count, 2);
    }

    #[test]
    fn test_drift_status_counts() {
        let conn = setup_in_memory_db();
        let tx1 = sample_tx("a.rs", "PENDING");
        let tx2 = sample_tx("b.rs", "UNAUDITED");
        let tx3 = sample_tx("c.rs", "UNAUDITED");

        crate::ledger::db::transactions::insert_transaction(&conn, &tx1).unwrap();
        upsert_unaudited_transaction(&conn, &tx2).unwrap();
        upsert_unaudited_transaction(&conn, &tx3).unwrap();

        let (pending, unaudited) = drift_status_counts(&conn).unwrap();
        assert_eq!(pending, 1);
        assert_eq!(unaudited, 2);
    }
}
