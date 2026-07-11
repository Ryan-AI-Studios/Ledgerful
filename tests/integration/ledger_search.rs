use ledgerful::ledger::db::LedgerDb;
use ledgerful::ledger::error::LedgerError;
use ledgerful::ledger::types::*;
use ledgerful::state::migrations::get_migrations;
use rusqlite::Connection;

fn setup_db() -> Connection {
    let mut conn = Connection::open_in_memory().unwrap();
    get_migrations().to_latest(&mut conn).unwrap();
    conn
}

fn insert_dummy_tx(db: &LedgerDb, tx_id: &str) {
    let tx = Transaction {
        tx_id: tx_id.to_string(),
        operation_id: None,
        status: "COMMITTED".to_string(),
        category: Category::Feature,
        entity: "test".to_string(),
        entity_normalized: "test".to_string(),
        planned_action: None,
        session_id: "test".to_string(),
        source: "test".to_string(),
        started_at: "2026-01-01T00:00:00Z".to_string(),
        resolved_at: Some("2026-01-01T01:00:00Z".to_string()),
        issue_ref: None,
        detected_at: None,
        drift_count: 1,
        first_seen_at: None,
        last_seen_at: None,
        snapshot_id: None,
    };
    db.insert_transaction(&tx).unwrap();
}

#[test]
fn test_search_basic() {
    let conn = setup_db();
    let db = LedgerDb::new(&conn);

    insert_dummy_tx(&db, "tx1");
    insert_dummy_tx(&db, "tx2");

    let entry1 = LedgerEntry {
        id: 1,
        tx_id: "tx1".to_string(),
        category: Category::Feature,
        entry_type: EntryType::Implementation,
        entity: "src/main.rs".to_string(),
        entity_normalized: "src/main.rs".to_string(),
        change_type: ChangeType::Modify,
        summary: "Implement database search".to_string(),
        reason: "Required for track L4-2".to_string(),
        is_breaking: false,
        committed_at: "2026-01-01T10:00:00Z".to_string(),
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
    };

    let entry2 = LedgerEntry {
        id: 2,
        tx_id: "tx2".to_string(),
        category: Category::Bugfix,
        entry_type: EntryType::Implementation,
        entity: "src/lib.rs".to_string(),
        entity_normalized: "src/lib.rs".to_string(),
        change_type: ChangeType::Modify,
        summary: "Fix bug in search results".to_string(),
        reason: "Search was returning wrong items".to_string(),
        is_breaking: false,
        committed_at: "2026-01-01T11:00:00Z".to_string(),
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
    };

    db.insert_ledger_entry(&entry1).unwrap();
    db.insert_ledger_entry(&entry2).unwrap();

    // Search for "database"
    let results = db
        .search_ledger("database", None, None, false, None, 0)
        .unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].summary, "Implement database search");

    // Search for "search"
    let results = db
        .search_ledger("search", None, None, false, None, 0)
        .unwrap();
    assert_eq!(results.len(), 2);
}

#[test]
fn test_search_filters() {
    let conn = setup_db();
    let db = LedgerDb::new(&conn);

    insert_dummy_tx(&db, "tx1");
    insert_dummy_tx(&db, "tx2");

    let entry1 = LedgerEntry {
        id: 1,
        tx_id: "tx1".to_string(),
        category: Category::Feature,
        entry_type: EntryType::Implementation,
        entity: "src/main.rs".to_string(),
        entity_normalized: "src/main.rs".to_string(),
        change_type: ChangeType::Modify,
        summary: "FTS search".to_string(),
        reason: "Reason 1".to_string(),
        is_breaking: true,
        committed_at: "2026-01-01T10:00:00Z".to_string(),
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
    };

    let entry2 = LedgerEntry {
        id: 2,
        tx_id: "tx2".to_string(),
        category: Category::Bugfix,
        entry_type: EntryType::Implementation,
        entity: "src/lib.rs".to_string(),
        entity_normalized: "src/lib.rs".to_string(),
        change_type: ChangeType::Modify,
        summary: "FTS fix".to_string(),
        reason: "Reason 2".to_string(),
        is_breaking: false,
        committed_at: "2026-01-01T11:00:00Z".to_string(),
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
    };

    db.insert_ledger_entry(&entry1).unwrap();
    db.insert_ledger_entry(&entry2).unwrap();

    // Filter by category
    let results = db
        .search_ledger("FTS", Some("BUGFIX"), None, false, None, 0)
        .unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].summary, "FTS fix");

    // Filter by breaking
    let results = db.search_ledger("FTS", None, None, true, None, 0).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].summary, "FTS search");
}

#[test]
fn test_search_ranking() {
    let conn = setup_db();
    let db = LedgerDb::new(&conn);

    insert_dummy_tx(&db, "tx1");
    insert_dummy_tx(&db, "tx2");

    let entry1 = LedgerEntry {
        id: 1,
        tx_id: "tx1".to_string(),
        category: Category::Feature,
        entry_type: EntryType::Implementation,
        entity: "search.rs".to_string(),
        entity_normalized: "search.rs".to_string(),
        change_type: ChangeType::Modify,
        summary: "Alpha".to_string(),
        reason: "Some search word here".to_string(),
        is_breaking: false,
        committed_at: "2026-01-01T10:00:00Z".to_string(),
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
    };

    let entry2 = LedgerEntry {
        id: 2,
        tx_id: "tx2".to_string(),
        category: Category::Feature,
        entry_type: EntryType::Implementation,
        entity: "other.rs".to_string(),
        entity_normalized: "other.rs".to_string(),
        change_type: ChangeType::Modify,
        summary: "Search search search word in summary".to_string(),
        reason: "Beta".to_string(),
        is_breaking: false,
        committed_at: "2026-01-01T11:00:00Z".to_string(),
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
    };

    db.insert_ledger_entry(&entry1).unwrap();
    db.insert_ledger_entry(&entry2).unwrap();

    // Query for "Search"
    let results = db
        .search_ledger("Search", None, None, false, None, 0)
        .unwrap();
    assert_eq!(results.len(), 2);
    // entry2 has "Search" in summary, entry1 has it in reason.
    assert_eq!(results[0].tx_id, "tx2");
}

#[test]
fn test_search_invalid_syntax() {
    let conn = setup_db();
    let db = LedgerDb::new(&conn);

    // FTS5 queries are now wrapped in double quotes (phrase search) to
    // prevent hyphenated terms from being misinterpreted as column qualifiers.
    // This means previously "invalid" syntax like "summary: ( )" is now treated
    // as a literal phrase search and returns empty results instead of erroring.
    let result = db.search_ledger("summary: ( )", None, None, false, None, 0);
    match result {
        Ok(entries) => {
            // Phrase search for literal "summary: ( )" returns no matches.
            assert!(
                entries.is_empty(),
                "expected empty results for non-matching phrase"
            );
        }
        Err(LedgerError::Validation(msg)) => {
            // Still acceptable if the FTS engine rejects it, but not required.
            assert!(msg.contains("syntax error") || msg.contains("Invalid search query"));
        }
        other => panic!("Expected Ok(empty) or Err(Validation), got {:?}", other),
    }
}

#[test]
fn test_search_days_filtering() {
    let conn = setup_db();
    let db = LedgerDb::new(&conn);

    insert_dummy_tx(&db, "tx1");
    insert_dummy_tx(&db, "tx2");

    // Use a fixed "now" reference by inserting entries relative to real now if possible,
    // or just trust the SQL 'now' and use very old dates for "outside" and very recent for "inside".

    let now = chrono::Utc::now();
    let two_days_ago =
        (now - chrono::Duration::days(2)).to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let ten_days_ago =
        (now - chrono::Duration::days(10)).to_rfc3339_opts(chrono::SecondsFormat::Secs, true);

    let entry1 = LedgerEntry {
        id: 1,
        tx_id: "tx1".to_string(),
        category: Category::Feature,
        entry_type: EntryType::Implementation,
        entity: "recent.rs".to_string(),
        entity_normalized: "recent.rs".to_string(),
        change_type: ChangeType::Modify,
        summary: "Recent change".to_string(),
        reason: "Reason 1".to_string(),
        is_breaking: false,
        committed_at: two_days_ago,
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
    };

    let entry2 = LedgerEntry {
        id: 2,
        tx_id: "tx2".to_string(),
        category: Category::Feature,
        entry_type: EntryType::Implementation,
        entity: "old.rs".to_string(),
        entity_normalized: "old.rs".to_string(),
        change_type: ChangeType::Modify,
        summary: "Old change".to_string(),
        reason: "Reason 2".to_string(),
        is_breaking: false,
        committed_at: ten_days_ago,
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
    };

    db.insert_ledger_entry(&entry1).unwrap();
    db.insert_ledger_entry(&entry2).unwrap();

    // Search with --days 5
    let results = db
        .search_ledger("change", None, Some(5), false, None, 0)
        .unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].summary, "Recent change");

    // Search with --days 15
    let results = db
        .search_ledger("change", None, Some(15), false, None, 0)
        .unwrap();
    assert_eq!(results.len(), 2);
}
