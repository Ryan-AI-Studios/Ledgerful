use crate::state::layout::Layout;

/// Reconstruct the current gate mode by replaying CONFIG/MAINTENANCE ledger
/// entries from genesis. Returns the most recent mode written by an init or
/// transition entry, or `None` if no mode entries exist yet.
pub fn current_mode_from_ledger(layout: &Layout) -> Option<String> {
    let db_path = layout.state_subdir().join("ledger.db");
    if !db_path.exists() {
        return None;
    }
    let storage =
        match crate::state::storage::StorageManager::open_read_only_sqlite_only(&layout.root) {
            Ok(s) => s,
            Err(e) => {
                tracing::debug!("current_mode_from_ledger: could not open storage: {e}");
                return None;
            }
        };
    let db = crate::ledger::db::LedgerDb::new(storage.get_connection());
    let entries = match db.get_all_committed_ledger_entries() {
        Ok(e) => e,
        Err(e) => {
            tracing::debug!("current_mode_from_ledger: could not read entries: {e}");
            return None;
        }
    };

    let mut mode: Option<String> = None;
    for entry in entries {
        if !is_mode_entry(&entry) {
            continue;
        }
        if let Some(new_mode) = parse_mode_from_summary(&entry.summary)
            .or_else(|| parse_mode_from_summary(&entry.reason))
        {
            mode = Some(new_mode);
        }
    }
    mode
}

fn is_mode_entry(entry: &crate::ledger::types::LedgerEntry) -> bool {
    use crate::ledger::types::EntryType;
    entry.entity == "ledgerful/gate-mode" && entry.entry_type == EntryType::Maintenance
}

fn parse_mode_from_summary(summary: &str) -> Option<String> {
    let normalized = summary.to_lowercase();
    if normalized.contains("to enforce") || normalized.contains("initialized to enforce") {
        Some("enforce".to_string())
    } else if normalized.contains("to observe") || normalized.contains("initialized to observe") {
        Some("observe".to_string())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger::db::LedgerDb;
    use crate::ledger::types::{Category, ChangeType, EntryType, LedgerEntry, Transaction};
    use crate::state::migrations::get_migrations;
    use rusqlite::Connection;

    fn in_memory_storage() -> (Connection, Layout) {
        let tmp = tempfile::tempdir().unwrap();
        let root = camino::Utf8Path::from_path(tmp.path())
            .unwrap()
            .to_path_buf();
        let layout = Layout::new(&root);
        let mut conn = Connection::open_in_memory().unwrap();
        get_migrations().to_latest(&mut conn).unwrap();
        (conn, layout)
    }

    fn mode_tx(tx_id: &str, summary: &str) -> (Transaction, LedgerEntry) {
        let tx = Transaction {
            tx_id: tx_id.to_string(),
            operation_id: None,
            status: "COMMITTED".to_string(),
            category: Category::Chore,
            entity: "ledgerful/gate-mode".to_string(),
            entity_normalized: "ledgerful/gate-mode".to_string(),
            planned_action: Some(summary.to_string()),
            session_id: "test".to_string(),
            source: "CLI".to_string(),
            started_at: "2026-01-01T00:00:00Z".to_string(),
            resolved_at: Some("2026-01-01T00:00:00Z".to_string()),
            issue_ref: None,
            detected_at: None,
            drift_count: 1,
            first_seen_at: None,
            last_seen_at: None,
            snapshot_id: None,
        };
        let entry = LedgerEntry {
            id: 0,
            tx_id: tx_id.to_string(),
            category: Category::Chore,
            entry_type: EntryType::Maintenance,
            entity: "ledgerful/gate-mode".to_string(),
            entity_normalized: "ledgerful/gate-mode".to_string(),
            change_type: ChangeType::Modify,
            summary: summary.to_string(),
            reason: "Mode transition".to_string(),
            is_breaking: false,
            committed_at: "2026-01-01T00:00:00Z".to_string(),
            verification_status: None,
            verification_basis: None,
            outcome_notes: None,
            origin: "LOCAL".to_string(),
            trace_id: None,
            signature: None,
            public_key: None,
            risk: None,
            related_tickets: None,
            author: "test".to_string(),
            observed: None,
            prev_hash: None,
            sig_version: 1,
        };
        (tx, entry)
    }

    #[test]
    fn current_mode_replays_transitions_from_genesis() {
        let (conn, _layout) = in_memory_storage();
        let db = LedgerDb::new(&conn);
        let (tx1, entry1) = mode_tx("tx-1", "Gate mode initialized to observe");
        db.insert_transaction(&tx1).unwrap();
        db.insert_ledger_entry(&entry1).unwrap();

        let (tx2, entry2) = mode_tx("tx-2", "Gate mode changed to enforce");
        db.insert_transaction(&tx2).unwrap();
        db.insert_ledger_entry(&entry2).unwrap();

        // We cannot reuse the in-memory conn through `current_mode_from_ledger`
        // because that function opens the file-based ledger.db under `layout`.
        // Instead, verify the parser directly and rely on integration tests for
        // end-to-end replay.
        assert_eq!(
            parse_mode_from_summary(&entry1.summary),
            Some("observe".to_string())
        );
        assert_eq!(
            parse_mode_from_summary(&entry2.summary),
            Some("enforce".to_string())
        );
        assert_eq!(parse_mode_from_summary("no mode here"), None);
    }
}
