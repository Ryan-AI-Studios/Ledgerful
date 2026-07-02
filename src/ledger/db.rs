use crate::ledger::enforcement::*;
use crate::ledger::error::LedgerError;
use crate::ledger::provenance::TokenProvenance;
use crate::ledger::types::*;
use rusqlite::Connection;

mod adr;
mod drift;
mod enforcement;
mod federation;
mod maintenance;
mod provenance;
mod search;
mod transactions;

pub struct LedgerDb<'a> {
    conn: &'a Connection,
}

impl<'a> LedgerDb<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    pub fn insert_transaction(&self, tx: &Transaction) -> Result<(), LedgerError> {
        transactions::insert_transaction(self.conn, tx)
    }

    pub fn get_transaction(&self, tx_id: &str) -> Result<Option<Transaction>, LedgerError> {
        transactions::get_transaction(self.conn, tx_id)
    }

    pub fn get_pending_by_entity(
        &self,
        entity_normalized: &str,
    ) -> Result<Option<Transaction>, LedgerError> {
        transactions::get_pending_by_entity(self.conn, entity_normalized)
    }

    pub fn get_unaudited_by_entity(
        &self,
        entity_normalized: &str,
    ) -> Result<Option<Transaction>, LedgerError> {
        drift::get_unaudited_by_entity(self.conn, entity_normalized)
    }

    pub fn upsert_unaudited_transaction(&self, tx: &Transaction) -> Result<(), LedgerError> {
        drift::upsert_unaudited_transaction(self.conn, tx)
    }

    pub fn update_transaction_status(
        &self,
        tx_id: &str,
        status: &str,
        resolved_at: Option<&str>,
    ) -> Result<usize, LedgerError> {
        transactions::update_transaction_status(self.conn, tx_id, status, resolved_at)
    }

    pub fn get_stale_pending_transactions(
        &self,
        ttl_days: u64,
    ) -> Result<Vec<String>, LedgerError> {
        maintenance::get_stale_pending_transactions(self.conn, ttl_days)
    }

    pub fn delete_stale_pending_transactions(&self, ttl_days: u64) -> Result<usize, LedgerError> {
        maintenance::delete_stale_pending_transactions(self.conn, ttl_days)
    }

    pub fn update_transaction_status_bulk(
        &self,
        tx_ids: &[String],
        status: &str,
        expected_status: &str,
        resolved_at: Option<&str>,
    ) -> Result<usize, LedgerError> {
        transactions::update_transaction_status_bulk(
            self.conn,
            tx_ids,
            status,
            expected_status,
            resolved_at,
        )
    }

    pub fn insert_ledger_entry(&self, entry: &LedgerEntry) -> Result<(), LedgerError> {
        transactions::insert_ledger_entry(self.conn, entry)
    }

    pub fn get_ledger_entries_for_tx(&self, tx_id: &str) -> Result<Vec<LedgerEntry>, LedgerError> {
        transactions::get_ledger_entries_for_tx(self.conn, tx_id)
    }

    pub fn get_ledger_entries_by_entity(
        &self,
        entity_normalized: &str,
    ) -> Result<Vec<LedgerEntry>, LedgerError> {
        self.get_ledger_entries_by_entity_paginated(entity_normalized, 1000, 0)
    }

    pub fn get_ledger_entries_by_entity_paginated(
        &self,
        entity_normalized: &str,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<LedgerEntry>, LedgerError> {
        transactions::get_ledger_entries_by_entity_paginated(
            self.conn,
            entity_normalized,
            limit,
            offset,
        )
    }

    pub fn get_recent_ledger_entries_paginated(
        &self,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<LedgerEntry>, LedgerError> {
        maintenance::get_recent_ledger_entries_paginated(self.conn, limit, offset)
    }

    pub fn get_federated_entries_by_entity(
        &self,
        entity_normalized: &str,
        sibling_name: &str,
        days: u64,
    ) -> Result<Vec<LedgerEntry>, LedgerError> {
        federation::get_federated_entries_by_entity(
            self.conn,
            entity_normalized,
            sibling_name,
            days,
        )
    }

    pub fn get_adr_entries(&self, days: Option<u64>) -> Result<Vec<LedgerEntry>, LedgerError> {
        adr::get_adr_entries(self.conn, days)
    }

    pub fn search_ledger(
        &self,
        query: &str,
        category: Option<&str>,
        days: Option<u64>,
        breaking_only: bool,
        limit: Option<usize>,
        offset: usize,
    ) -> Result<Vec<LedgerEntry>, LedgerError> {
        search::search_ledger(
            self.conn,
            query,
            category,
            days,
            breaking_only,
            limit,
            offset,
        )
    }

    pub fn get_all_pending(&self) -> Result<Vec<Transaction>, LedgerError> {
        transactions::get_all_pending(self.conn)
    }

    pub fn get_all_unaudited(&self) -> Result<Vec<Transaction>, LedgerError> {
        drift::get_all_unaudited(self.conn)
    }

    pub fn drift_status_counts(&self) -> Result<(usize, usize), LedgerError> {
        drift::drift_status_counts(self.conn)
    }

    pub fn get_unaudited_by_pattern(&self, pattern: &str) -> Result<Vec<Transaction>, LedgerError> {
        drift::get_unaudited_by_pattern(self.conn, pattern)
    }

    pub fn resolve_tx_id_fuzzy(&self, prefix: &str) -> Result<Vec<String>, LedgerError> {
        transactions::resolve_tx_id_fuzzy(self.conn, prefix)
    }

    pub fn insert_tech_stack_rule(&self, rule: &TechStackRule) -> Result<(), LedgerError> {
        enforcement::insert_tech_stack_rule(self.conn, rule)
    }

    pub fn get_tech_stack_rules(
        &self,
        category: Option<&str>,
    ) -> Result<Vec<TechStackRule>, LedgerError> {
        enforcement::get_tech_stack_rules(self.conn, category)
    }

    pub fn get_tech_stack_rule(
        &self,
        category: &str,
    ) -> Result<Option<TechStackRule>, LedgerError> {
        enforcement::get_tech_stack_rule(self.conn, category)
    }

    pub fn insert_commit_validator(&self, validator: &CommitValidator) -> Result<(), LedgerError> {
        enforcement::insert_commit_validator(self.conn, validator)
    }

    pub fn set_validator_enabled(&self, name: &str, enabled: bool) -> Result<(), LedgerError> {
        enforcement::set_validator_enabled(self.conn, name, enabled)
    }

    pub fn remove_validator(&self, name: &str) -> Result<(), LedgerError> {
        enforcement::remove_validator(self.conn, name)
    }

    pub fn get_commit_validators(
        &self,
        category: Option<&str>,
    ) -> Result<Vec<CommitValidator>, LedgerError> {
        enforcement::get_commit_validators(self.conn, category)
    }

    pub fn insert_category_mapping(
        &self,
        mapping: &CategoryStackMapping,
    ) -> Result<(), LedgerError> {
        enforcement::insert_category_mapping(self.conn, mapping)
    }

    pub fn get_category_mappings(
        &self,
        category: Option<&str>,
    ) -> Result<Vec<CategoryStackMapping>, LedgerError> {
        enforcement::get_category_mappings(self.conn, category)
    }

    pub fn insert_watcher_pattern(&self, pattern: &WatcherPattern) -> Result<(), LedgerError> {
        enforcement::insert_watcher_pattern(self.conn, pattern)
    }

    pub fn get_watcher_patterns(&self) -> Result<Vec<WatcherPattern>, LedgerError> {
        enforcement::get_watcher_patterns(self.conn)
    }

    pub fn insert_token_provenance(&self, prov: &TokenProvenance) -> Result<(), LedgerError> {
        provenance::insert_token_provenance(self.conn, prov)
    }

    pub fn get_token_provenance_for_tx(
        &self,
        tx_id: &str,
    ) -> Result<Vec<TokenProvenance>, LedgerError> {
        provenance::get_token_provenance_for_tx(self.conn, tx_id)
    }

    pub fn get_token_provenance_by_entity(
        &self,
        entity_normalized: &str,
    ) -> Result<Vec<TokenProvenance>, LedgerError> {
        provenance::get_token_provenance_by_entity(self.conn, entity_normalized)
    }

    pub fn get_all_committed_ledger_entries(&self) -> Result<Vec<LedgerEntry>, LedgerError> {
        transactions::get_all_committed_ledger_entries(self.conn)
    }

    pub fn get_committed_ledger_entries_paginated(
        &self,
        category: Option<&str>,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<LedgerEntry>, LedgerError> {
        transactions::get_committed_ledger_entries_paginated(self.conn, category, limit, offset)
    }

    pub fn update_ledger_entry_signature(
        &self,
        tx_id: &str,
        signature: &str,
        public_key: &str,
    ) -> Result<usize, LedgerError> {
        transactions::update_ledger_entry_signature(self.conn, tx_id, signature, public_key)
    }

    pub fn find_transactions_by_file(
        &self,
        file_path: &str,
    ) -> Result<Vec<LedgerEntry>, LedgerError> {
        provenance::find_transactions_by_file(self.conn, file_path)
    }

    pub fn get_related_ledger_entries(
        &self,
        directory: &str,
        limit: usize,
    ) -> Result<Vec<LedgerEntry>, LedgerError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, tx_id, category, entry_type, entity, entity_normalized,
                change_type, summary, reason, is_breaking, committed_at,
                verification_status, verification_basis, outcome_notes,
                origin, trace_id, signature, public_key, risk, related_tickets, author
             FROM ledger_entries
             WHERE entity_normalized LIKE ?1
             ORDER BY committed_at DESC
             LIMIT ?2",
        )?;

        let pattern = format!("{}/%", directory.trim_end_matches('/'));
        let rows = stmt.query_map(rusqlite::params![pattern, limit as i64], map_ledger_entry)?;

        let mut entries = Vec::new();
        for entry in rows {
            entries.push(entry?);
        }
        Ok(entries)
    }

    pub fn get_transaction_velocity(&self, days: u64) -> Result<usize, LedgerError> {
        maintenance::get_transaction_velocity(self.conn, days)
    }

    pub fn get_top_churned_entities(
        &self,
        limit: usize,
    ) -> Result<Vec<(String, usize)>, LedgerError> {
        maintenance::get_top_churned_entities(self.conn, limit)
    }

    pub fn get_oldest_adr(&self) -> Result<Option<LedgerEntry>, LedgerError> {
        adr::get_oldest_adr(self.conn)
    }

    pub fn register_forbidden_term(
        &self,
        term: &str,
        category: &str,
        reason: &str,
    ) -> Result<(), LedgerError> {
        enforcement::register_forbidden_term(self.conn, term, category, reason)
    }

    pub fn register_validator(
        &self,
        name: &str,
        command: &str,
        category: &str,
        timeout_secs: u64,
    ) -> Result<(), LedgerError> {
        enforcement::register_validator(self.conn, name, command, category, timeout_secs)
    }

    pub fn update_adr_metadata(
        &self,
        adr_id: &str,
        update: AdrMetadataUpdate,
    ) -> Result<(), LedgerError> {
        adr::update_adr_metadata(self.conn, adr_id, update)
    }

    pub fn get_adr_metadata(&self, adr_id: &str) -> Result<Option<AdrMetadata>, LedgerError> {
        adr::get_adr_metadata(self.conn, adr_id)
    }

    pub fn link_adr_supersedes(
        &self,
        adr_id: &str,
        supersedes_id: &str,
    ) -> Result<(), LedgerError> {
        adr::link_adr_supersedes(self.conn, adr_id, supersedes_id)
    }
}

/// Shared ledger-entry row mapper used by domain modules.
pub(crate) fn map_ledger_entry(row: &rusqlite::Row) -> rusqlite::Result<LedgerEntry> {
    let cat_str: String = row.get(2)?;
    let category: Category = serde_json::from_str(&format!("\"{}\"", cat_str))
        .map_err(|_| rusqlite::Error::InvalidQuery)?;
    let et_str: String = row.get(3)?;
    let entry_type: EntryType = serde_json::from_str(&format!("\"{}\"", et_str))
        .map_err(|_| rusqlite::Error::InvalidQuery)?;
    let ct_str: String = row.get(6)?;
    let change_type: ChangeType = serde_json::from_str(&format!("\"{}\"", ct_str))
        .map_err(|_| rusqlite::Error::InvalidQuery)?;

    let vs_str: Option<String> = row.get(11)?;
    let verification_status = match vs_str {
        Some(s) => Some(
            serde_json::from_str(&format!("\"{}\"", s))
                .map_err(|_| rusqlite::Error::InvalidQuery)?,
        ),
        None => None,
    };
    let vb_str: Option<String> = row.get(12)?;
    let verification_basis = match vb_str {
        Some(s) => Some(
            serde_json::from_str(&format!("\"{}\"", s))
                .map_err(|_| rusqlite::Error::InvalidQuery)?,
        ),
        None => None,
    };

    Ok(LedgerEntry {
        id: row.get(0)?,
        tx_id: row.get(1)?,
        category,
        entry_type,
        entity: row.get(4)?,
        entity_normalized: row.get(5)?,
        change_type,
        summary: row.get(7)?,
        reason: row.get(8)?,
        is_breaking: row.get::<_, i32>(9)? != 0,
        committed_at: row.get(10)?,
        verification_status,
        verification_basis,
        outcome_notes: row.get(13)?,
        origin: row.get(14)?,
        trace_id: row.get(15)?,
        signature: row.get(16)?,
        public_key: row.get(17)?,
        risk: row.get(18)?,
        related_tickets: row.get(19)?,
        author: row.get(20)?,
    })
}
