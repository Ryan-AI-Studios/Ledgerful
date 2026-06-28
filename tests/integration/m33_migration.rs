use ledgerful::state::migrations::m33_intent_provenance::m33_intent_provenance;
use rusqlite::Connection;
use rusqlite_migration::Migrations;

#[test]
fn test_m33_adds_columns_to_sqlite() {
    let mut conn = Connection::open_in_memory().unwrap();

    // Setup initial table (this is a simplified version of what would exist before M33)
    conn.execute(
        "CREATE TABLE ledger_entries (
            id TEXT PRIMARY KEY,
            tx_id TEXT NOT NULL,
            entity TEXT NOT NULL,
            category TEXT NOT NULL,
            summary TEXT NOT NULL,
            reason TEXT NOT NULL,
            author TEXT NOT NULL,
            timestamp TEXT NOT NULL,
            outcome_notes TEXT
        )",
        [],
    )
    .unwrap();

    // Define migrations including M33
    let migrations = Migrations::new(m33_intent_provenance());

    // Apply migrations
    migrations.to_latest(&mut conn).unwrap();

    // Verify columns exist
    let stmt = conn
        .prepare("SELECT signature, public_key, risk, related_tickets FROM ledger_entries")
        .unwrap();
    assert!(stmt.column_count() >= 4);
}

#[test]
fn test_m33_backfills_existing_rows() {
    let mut conn = Connection::open_in_memory().unwrap();

    // Setup initial table and data
    conn.execute(
        "CREATE TABLE ledger_entries (
            id TEXT PRIMARY KEY,
            tx_id TEXT NOT NULL,
            entity TEXT NOT NULL,
            category TEXT NOT NULL,
            summary TEXT NOT NULL,
            reason TEXT NOT NULL,
            author TEXT NOT NULL,
            timestamp TEXT NOT NULL
        )",
        [],
    )
    .unwrap();

    conn.execute(
        "INSERT INTO ledger_entries (id, tx_id, entity, category, summary, reason, author, timestamp)
         VALUES ('1', 'tx1', 'file.rs', 'FEATURE', 'sum', 'rease', 'me', '2026-05-23T00:00:00Z')",
        [],
    ).unwrap();

    // Define and apply M33
    let migrations = Migrations::new(m33_intent_provenance());
    migrations.to_latest(&mut conn).unwrap();

    // Verify row survives with NULL for new columns
    let mut stmt = conn.prepare("SELECT signature, public_key, risk, related_tickets FROM ledger_entries WHERE id = '1'").unwrap();
    let mut rows = stmt.query([]).unwrap();
    let row = rows.next().unwrap().unwrap();

    let signature: Option<String> = row.get(0).unwrap();
    let public_key: Option<String> = row.get(1).unwrap();
    assert!(signature.is_none());
    assert!(public_key.is_none());
}
