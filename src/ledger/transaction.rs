use chrono::Utc;
use globset::{Glob, GlobSetBuilder};
use rusqlite::Connection;
use std::path::{Path, PathBuf};
use uuid::Uuid;

use crate::config::model::Config;
use crate::ledger::db::LedgerDb;
use crate::ledger::enforcement::ValidationLevel;
use crate::ledger::error::LedgerError;
use crate::ledger::provenance::{ProvenanceAction, TokenProvenance};
use crate::ledger::session::get_session_id;
use crate::ledger::types::*;
use crate::ledger::validators::ValidatorRunner;
use crate::platform::process_policy::ProcessPolicy;

pub struct TransactionManager<'a> {
    storage: &'a mut crate::state::storage::StorageManager,
    repo_root: PathBuf,
    is_case_insensitive: bool,
    config: Config,
}

impl<'a> TransactionManager<'a> {
    pub fn new(
        storage: &'a mut crate::state::storage::StorageManager,
        repo_root: PathBuf,
        config: Config,
    ) -> Self {
        let is_case_insensitive = repo_root.join(".GIT").exists();
        Self {
            storage,
            repo_root,
            is_case_insensitive,
            config,
        }
    }

    pub fn get_connection(&self) -> &Connection {
        self.storage.get_connection()
    }

    pub fn start_change(&mut self, req: TransactionRequest) -> Result<String, LedgerError> {
        let normalized = self.entity_normalized(&req.entity)?;

        let db = LedgerDb::new(self.storage.get_connection());
        if let Some(pending) = db.get_pending_by_entity(&normalized)? {
            return Err(LedgerError::Conflict(pending.entity));
        }

        // Tech Stack Enforcement
        if self.config.ledger.enforcement_enabled {
            let cat_str = serde_json::to_string(&req.category)
                .unwrap_or_default()
                .trim_matches('"')
                .to_string();
            let mappings = db.get_category_mappings(Some(&cat_str))?;

            for m in mappings {
                // Check CategoryStackMapping.glob against entity if present
                if let Some(ref mapping_glob) = m.glob
                    && let Ok(glob) = Glob::new(mapping_glob)
                    && let Ok(set) = GlobSetBuilder::new().add(glob).build()
                    && !set.is_match(&normalized)
                {
                    continue;
                }

                if let Some(rule) = db.get_tech_stack_rule(&m.stack_category)? {
                    for rule_text in &rule.rules {
                        if rule_text.to_uppercase().starts_with("NO ") {
                            let term = &rule_text[3..];
                            let term_lower = term.to_lowercase();

                            // Check planned_action text (if provided)
                            if let Some(ref action) = req.planned_action
                                && action.to_lowercase().contains(&term_lower)
                            {
                                return Err(LedgerError::RuleViolation(format!(
                                    "Planned action violates tech stack rule for {} (forbidden term: {})",
                                    rule.name, term_lower
                                )));
                            }

                            // Also check entity path
                            if normalized.to_lowercase().contains(&term_lower) {
                                return Err(LedgerError::RuleViolation(format!(
                                    "Entity path violates tech stack rule for {} (forbidden term: {})",
                                    rule.name, term_lower
                                )));
                            }
                        }
                    }
                }
            }
        }

        let tx_id = Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();
        let tx = Transaction {
            tx_id: tx_id.clone(),
            operation_id: req.operation_id,
            status: "PENDING".to_string(),
            category: req.category,
            entity: req.entity,
            entity_normalized: normalized,
            planned_action: req.planned_action,
            session_id: get_session_id().to_string(),
            source: req.source.unwrap_or_else(|| "CLI".to_string()),
            started_at: now.clone(),
            resolved_at: None,
            issue_ref: req.issue_ref,
            detected_at: None,
            drift_count: 1,
            first_seen_at: Some(now.clone()),
            last_seen_at: Some(now),
        };

        db.insert_transaction(&tx).map_err(|e| {
            if let LedgerError::Database(ref sqlite_err) = e
                && sqlite_err.to_string().contains("UNIQUE constraint failed")
            {
                return LedgerError::Conflict(tx.entity);
            }
            e
        })?;
        Ok(tx_id)
    }

    pub fn commit_change(
        &mut self,
        tx_id: String,
        req: CommitRequest,
        force: bool,
    ) -> Result<(), LedgerError> {
        let tx_id = self.resolve_tx_id(&tx_id)?;

        let tx = {
            let db = LedgerDb::new(self.storage.get_connection());
            db.get_transaction(&tx_id)?
                .ok_or_else(|| LedgerError::NotFound(tx_id.clone()))?
        };

        if tx.status != "PENDING" {
            return Err(LedgerError::InvalidState(tx_id, tx.status));
        }

        // Verification gate: require verification status for high-risk categories
        if self.config.ledger.verify_to_commit {
            let requires_verification = matches!(
                tx.category,
                Category::Architecture | Category::Feature | Category::Bugfix | Category::Infra
            );

            if requires_verification {
                if force {
                    tracing::warn!(
                        "Verification gate bypassed with --force for transaction {} (category: {:?})",
                        tx_id,
                        tx.category
                    );
                } else if req.verification_status.is_none() || req.verification_basis.is_none() {
                    return Err(LedgerError::VerificationRequired(format!(
                        "{} (both status and basis required)",
                        tx.category
                    )));
                }
            }
        }

        // Commit Validation
        let db = LedgerDb::new(self.storage.get_connection());
        let cat_str = serde_json::to_string(&tx.category)
            .unwrap_or_default()
            .trim_matches('"')
            .to_string();

        let validators = db.get_commit_validators(Some(&cat_str))?;
        if !validators.is_empty() {
            // Build process policy from config
            let policy = ProcessPolicy {
                default_timeout_secs: self.config.verify.default_timeout_secs,
                ..Default::default()
            };

            for v in validators {
                if !v.enabled {
                    continue;
                }
                // Check glob if present using proper globset matching
                if let Some(ref pattern) = v.glob {
                    let glob = Glob::new(pattern).map_err(|e| {
                        LedgerError::Validation(format!(
                            "Invalid glob pattern '{}': {}",
                            pattern, e
                        ))
                    })?;
                    let mut builder = GlobSetBuilder::new();
                    builder.add(glob);
                    let globset = builder.build().map_err(|e| {
                        LedgerError::Validation(format!(
                            "Failed to build globset for '{}': {}",
                            pattern, e
                        ))
                    })?;
                    if !globset.is_match(&tx.entity_normalized) {
                        continue;
                    }
                }

                let absolute_path = self.repo_root.join(&tx.entity_normalized);
                let entity_path_str = absolute_path.to_string_lossy();

                let result = ValidatorRunner::run(
                    v.name.clone(),
                    &v.executable,
                    &v.args,
                    &entity_path_str,
                    v.timeout_ms as u64,
                    v.validation_level,
                    &policy,
                )?;

                if !result.success {
                    match v.validation_level {
                        ValidationLevel::Error => {
                            return Err(LedgerError::ValidatorFailed(
                                v.name,
                                format!("STDOUT: {}\nSTDERR: {}", result.stdout, result.stderr),
                            ));
                        }
                        ValidationLevel::Warning => {
                            eprintln!(
                                "WARNING: Validator '{}' failed:\nSTDOUT: {}\nSTDERR: {}",
                                v.name, result.stdout, result.stderr
                            );
                        }
                    }
                }
            }
        }

        let now = req.committed_at.unwrap_or_else(|| Utc::now().to_rfc3339());

        // Use a database transaction to ensure atomicity
        let sqlite_tx = self
            .storage
            .get_connection_mut()
            .transaction()
            .map_err(LedgerError::from)?;
        {
            let db = LedgerDb::new(&sqlite_tx);

            // 1. Update transaction status to COMMITTED
            let count = db.update_transaction_status(&tx_id, "COMMITTED", Some(&now))?;
            if count == 0 {
                return Err(LedgerError::InvalidState(
                    tx_id,
                    "already resolved".to_string(),
                ));
            }

            // 2. Create ledger entry
            let entry_type = if tx.category == Category::Architecture {
                EntryType::Architecture
            } else {
                EntryType::Implementation
            };

            let mut outcome_notes = req.outcome_notes;
            let (signature, pub_key) = if req.signature.is_some() {
                (req.signature, req.public_key)
            } else {
                match crate::ledger::crypto::sign_ledger_entry(
                    &tx_id,
                    &tx.category.to_string(),
                    &req.summary,
                    &req.reason,
                    &now,
                ) {
                    Ok(res) => res,
                    Err(e) => {
                        let err_msg = format!("Cryptographic signing failed: {}", e);
                        if self.config.intent.require_signing {
                            return Err(LedgerError::Validation(err_msg));
                        } else {
                            tracing::warn!("{} (non-blocking)", err_msg);
                            let notes = outcome_notes.take().unwrap_or_default();
                            outcome_notes = Some(
                                format!("{}\n[Warning] {}", notes, err_msg)
                                    .trim()
                                    .to_string(),
                            );
                            (None, None)
                        }
                    }
                }
            };

            // Capture author from git config at commit time
            let author = capture_git_author(&self.repo_root);

            let entry = LedgerEntry {
                id: 0, // DB will assign
                tx_id: tx_id.clone(),
                category: tx.category,
                entry_type,
                entity: tx.entity,
                entity_normalized: tx.entity_normalized,
                change_type: req.change_type,
                summary: req.summary.clone(),
                reason: req.reason.clone(),
                is_breaking: req.is_breaking,
                committed_at: now,
                verification_status: req.verification_status,
                verification_basis: req.verification_basis,
                outcome_notes,
                origin: "LOCAL".to_string(),
                trace_id: None,
                signature,
                public_key: pub_key,
                risk: req.risk,
                related_tickets: req.related_tickets.or(req.issue_ref),
                author,
            };

            db.insert_ledger_entry(&entry)?;
        }
        sqlite_tx.commit().map_err(LedgerError::from)?;

        // 3. Update Knowledge Graph (CozoDB)
        if let Some(ref cozo) = self.storage.cozo {
            let changed_files = if let Some(files) = req.changed_files {
                files
            } else {
                match self.get_transaction_files(&tx_id) {
                    Ok(files) => files,
                    Err(e) => {
                        tracing::warn!(
                            "ledger commit: could not discover changed files for KG edges (tx={tx_id}): {e}"
                        );
                        vec![]
                    }
                }
            };

            let tx_urn = crate::platform::urn::build_urn(
                crate::state::graph_kinds::NodeKind::LedgerTransaction,
                &tx_id,
            );
            let mut nodes = Vec::new();
            let mut edges = Vec::new();

            nodes.push(crate::state::storage_cozo::GraphNode {
                id: tx_urn.clone(),
                label: tx_id.to_string(),
                category: crate::state::graph_kinds::NodeKind::LedgerTransaction,
                risk_score: 0.0,
                metadata: Some(serde_json::json!({
                    "summary": req.summary,
                    "reason": req.reason,
                    "category": tx.category.to_string(),
                })),
            });

            for file in changed_files {
                // Filter out synthetic entities and non-file paths
                if !is_real_file_path(&file) {
                    continue;
                }

                let file_urn = crate::platform::urn::build_urn(
                    crate::state::graph_kinds::NodeKind::File,
                    &file,
                );
                edges.push(crate::state::storage_cozo::GraphEdge {
                    source: tx_urn.clone(),
                    target: file_urn,
                    relation: crate::state::graph_kinds::EdgeKind::Affects,
                    confidence: 1.0,
                    provenance_id: tx_id.to_string(),
                });
            }

            if let Err(e) = cozo.insert_nodes(&nodes) {
                tracing::warn!(
                    "ledger commit: failed to write transaction node to CozoDB (tx={tx_id}): {e}"
                );
            }
            if !edges.is_empty()
                && let Err(e) = cozo.insert_edges(&edges)
            {
                tracing::warn!(
                    "ledger commit: failed to write affects edges to CozoDB (tx={tx_id}): {e}"
                );
            }
        }

        Ok(())
    }

    pub fn rollback_change(&mut self, tx_id: String, reason: String) -> Result<(), LedgerError> {
        let tx_id = self.resolve_tx_id(&tx_id)?;
        let tx = {
            let db = LedgerDb::new(self.storage.get_connection());
            db.get_transaction(&tx_id)?
                .ok_or_else(|| LedgerError::NotFound(tx_id.clone()))?
        };

        if tx.status != "PENDING" {
            return Err(LedgerError::InvalidState(tx_id, tx.status));
        }

        let now = Utc::now().to_rfc3339();

        let sqlite_tx = self
            .storage
            .get_connection_mut()
            .transaction()
            .map_err(LedgerError::from)?;
        {
            let db = LedgerDb::new(&sqlite_tx);

            // 1. Update status
            let count = db.update_transaction_status(&tx_id, "ROLLED_BACK", Some(&now))?;
            if count == 0 {
                return Err(LedgerError::InvalidState(
                    tx_id,
                    "already resolved".to_string(),
                ));
            }

            // 2. Insert auditable entry
            let (signature, pub_key) = match crate::ledger::crypto::sign_ledger_entry(
                &tx_id,
                &tx.category.to_string(),
                "Transaction Rolled Back",
                &reason,
                &now,
            ) {
                Ok(res) => res,
                Err(e) => {
                    let err_msg = format!("Cryptographic signing failed: {}", e);
                    if self.config.intent.require_signing {
                        return Err(LedgerError::Validation(err_msg));
                    } else {
                        tracing::warn!("{} (non-blocking)", err_msg);
                        (None, None)
                    }
                }
            };

            let entry = LedgerEntry {
                id: 0,
                tx_id,
                category: tx.category,
                entry_type: EntryType::Rollback,
                entity: tx.entity,
                entity_normalized: tx.entity_normalized,
                change_type: ChangeType::Modify,
                summary: "Transaction Rolled Back".to_string(),
                reason,
                is_breaking: false,
                committed_at: now,
                verification_status: None,
                verification_basis: None,
                outcome_notes: None,
                origin: "LOCAL".to_string(),
                trace_id: None,
                signature,
                public_key: pub_key,
                risk: Some("TRIVIAL".to_string()),
                related_tickets: tx.issue_ref,
                author: capture_git_author(&self.repo_root),
            };
            db.insert_ledger_entry(&entry)?;
        }
        sqlite_tx.commit().map_err(LedgerError::from)?;

        Ok(())
    }

    pub fn atomic_change(
        &mut self,
        tx_req: TransactionRequest,
        commit_req: CommitRequest,
        force: bool,
    ) -> Result<String, LedgerError> {
        let tx_id = self.start_change(tx_req)?;
        if let Err(commit_err) = self.commit_change(tx_id.clone(), commit_req, force) {
            // Attempt cleanup rollback; prefer returning the original error
            if let Err(rollback_err) =
                self.rollback_change(tx_id, "Rollback after commit failure".to_string())
            {
                tracing::warn!(
                    "atomic_change: rollback after commit failure also failed: {rollback_err}"
                );
            }
            return Err(commit_err);
        }
        Ok(tx_id)
    }

    pub fn reconcile_drift(
        &mut self,
        tx_id: Option<String>,
        pattern: Option<String>,
        all: bool,
        reason: String,
    ) -> Result<(), LedgerError> {
        let db = LedgerDb::new(self.storage.get_connection());
        let to_reconcile = if all {
            db.get_all_unaudited()?
        } else if let Some(p) = pattern {
            db.get_unaudited_by_pattern(&p)?
        } else if let Some(id) = tx_id {
            let full_id = self.resolve_tx_id(&id)?;
            let tx = db
                .get_transaction(&full_id)?
                .ok_or_else(|| LedgerError::NotFound(full_id.clone()))?;
            if tx.status != "UNAUDITED" {
                return Err(LedgerError::InvalidState(full_id, tx.status));
            }
            vec![tx]
        } else {
            return Err(LedgerError::Config(
                "Must specify --tx-id, --entity-pattern, or --all for reconciliation".to_string(),
            ));
        };

        if to_reconcile.is_empty() {
            return Ok(());
        }

        let now = Utc::now().to_rfc3339();
        let sqlite_tx = self
            .storage
            .get_connection_mut()
            .transaction()
            .map_err(LedgerError::from)?;
        {
            let db = LedgerDb::new(&sqlite_tx);
            let tx_ids: Vec<String> = to_reconcile.iter().map(|tx| tx.tx_id.clone()).collect();
            let count =
                db.update_transaction_status_bulk(&tx_ids, "RECONCILED", "UNAUDITED", Some(&now))?;
            if count != tx_ids.len() {
                return Err(LedgerError::InvalidState(
                    "bulk".to_string(),
                    "Concurrent modification detected during reconciliation".to_string(),
                ));
            }

            for tx in to_reconcile {
                let summary_text = format!("Reconciled drift ({} changes)", tx.drift_count);
                let mut outcome_notes = None;
                let (signature, pub_key) = match crate::ledger::crypto::sign_ledger_entry(
                    &tx.tx_id,
                    &tx.category.to_string(),
                    &summary_text,
                    &reason,
                    &now,
                ) {
                    Ok(res) => res,
                    Err(e) => {
                        let err_msg = format!("Cryptographic signing failed: {}", e);
                        if self.config.intent.require_signing {
                            return Err(LedgerError::Validation(err_msg));
                        } else {
                            tracing::warn!("{} (non-blocking)", err_msg);
                            outcome_notes = Some(format!("[Warning] {}", err_msg));
                            (None, None)
                        }
                    }
                };

                let entry = LedgerEntry {
                    id: 0,
                    tx_id: tx.tx_id,
                    category: tx.category,
                    entry_type: EntryType::Reconciliation,
                    entity: tx.entity,
                    entity_normalized: tx.entity_normalized,
                    change_type: ChangeType::Modify,
                    summary: summary_text,
                    reason: reason.clone(),
                    is_breaking: false,
                    committed_at: now.clone(),
                    verification_status: None,
                    verification_basis: None,
                    outcome_notes,
                    origin: "LOCAL".to_string(),
                    trace_id: None,
                    signature,
                    public_key: pub_key,
                    risk: Some("TRIVIAL".to_string()),
                    related_tickets: None,
                    author: capture_git_author(&self.repo_root),
                };
                db.insert_ledger_entry(&entry)?;
            }
        }
        sqlite_tx.commit().map_err(LedgerError::from)?;

        Ok(())
    }

    pub fn adopt_drift(
        &mut self,
        tx_id: Option<String>,
        pattern: Option<String>,
        all: bool,
        reason: Option<String>,
    ) -> Result<Vec<String>, LedgerError> {
        let db = LedgerDb::new(self.storage.get_connection());
        let to_adopt = if all {
            db.get_all_unaudited()?
        } else if let Some(p) = pattern {
            db.get_unaudited_by_pattern(&p)?
        } else if let Some(id) = tx_id {
            let full_id = self.resolve_tx_id(&id)?;
            let tx = db
                .get_transaction(&full_id)?
                .ok_or_else(|| LedgerError::NotFound(full_id.clone()))?;
            if tx.status != "UNAUDITED" {
                return Err(LedgerError::InvalidState(full_id, tx.status));
            }
            vec![tx]
        } else {
            return Err(LedgerError::Config(
                "Must specify --tx-id, --entity-pattern, or --all for adoption".to_string(),
            ));
        };

        if to_adopt.is_empty() {
            return Ok(vec![]);
        }

        let tx_ids: Vec<String> = to_adopt.iter().map(|tx| tx.tx_id.clone()).collect();
        let count = db.update_transaction_status_bulk(&tx_ids, "PENDING", "UNAUDITED", None)?;
        if count != tx_ids.len() {
            return Err(LedgerError::InvalidState(
                "bulk".to_string(),
                "Concurrent modification detected during adoption".to_string(),
            ));
        }

        if let Some(reason_text) = reason {
            tracing::info!("Adopted drift with reason: {reason_text}");
        }

        Ok(tx_ids)
    }

    pub fn auto_reconcile_entity(
        &mut self,
        entity_normalized: &str,
        reason: String,
    ) -> Result<(), LedgerError> {
        let db = LedgerDb::new(self.storage.get_connection());
        if let Some(tx) = db.get_unaudited_by_entity(entity_normalized)? {
            self.reconcile_drift(Some(tx.tx_id), None, false, reason)?;
        }
        Ok(())
    }

    pub fn resolve_tx_id(&self, tx_id_or_prefix: &str) -> Result<String, LedgerError> {
        let db = LedgerDb::new(self.storage.get_connection());

        // 1. Exact full UUID match
        if tx_id_or_prefix.len() == 36 && db.get_transaction(tx_id_or_prefix)?.is_some() {
            return Ok(tx_id_or_prefix.to_string());
        }

        // 2. UUID prefix match (existing behaviour)
        let uuid_matches = db.resolve_tx_id_fuzzy(tx_id_or_prefix)?;
        if uuid_matches.len() == 1 {
            return Ok(uuid_matches[0].clone());
        }
        if uuid_matches.len() > 1 {
            return Err(LedgerError::Config(format!(
                "Ambiguous transaction ID prefix '{}': matched {}",
                tx_id_or_prefix,
                uuid_matches.join(", ")
            )));
        }

        // 3. Entity / basename fuzzy match against PENDING transactions (H6)
        // Normalise the lookup term: strip any path separators and lowercase.
        let needle = tx_id_or_prefix.to_lowercase();
        let needle_base = std::path::Path::new(tx_id_or_prefix)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(tx_id_or_prefix)
            .to_lowercase();

        let pending = db.get_all_pending()?;
        let entity_matches: Vec<String> = pending
            .into_iter()
            .filter(|tx| {
                let entity_lower = tx.entity.to_lowercase();
                let norm_lower = tx.entity_normalized.to_lowercase();
                let entity_base = std::path::Path::new(&tx.entity_normalized)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or(&tx.entity_normalized)
                    .to_lowercase();

                entity_lower.contains(&needle)
                    || norm_lower.contains(&needle)
                    || entity_base == needle_base
            })
            .map(|tx| tx.tx_id)
            .collect();

        match entity_matches.len() {
            0 => Err(LedgerError::NotFound(tx_id_or_prefix.to_string())),
            1 => Ok(entity_matches[0].clone()),
            _ => Err(LedgerError::Config(format!(
                "Ambiguous entity lookup '{}': matched {} pending transactions. \
                 Use the transaction ID prefix instead.",
                tx_id_or_prefix,
                entity_matches.len()
            ))),
        }
    }

    pub fn get_pending(&self, entity: &str) -> Result<Option<Transaction>, LedgerError> {
        let normalized = self.entity_normalized(entity)?;
        let db = LedgerDb::new(self.storage.get_connection());
        db.get_pending_by_entity(&normalized)
    }

    pub fn get_transaction(&self, tx_id: &str) -> Result<Option<Transaction>, LedgerError> {
        let db = LedgerDb::new(self.storage.get_connection());
        db.get_transaction(tx_id)
    }

    pub fn get_ledger_entries_for_tx(&self, tx_id: &str) -> Result<Vec<LedgerEntry>, LedgerError> {
        let db = LedgerDb::new(self.storage.get_connection());
        db.get_ledger_entries_for_tx(tx_id)
    }

    pub fn get_adr_entries(&self, days: Option<u64>) -> Result<Vec<LedgerEntry>, LedgerError> {
        let db = LedgerDb::new(self.storage.get_connection());
        db.get_adr_entries(days)
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
        let db = LedgerDb::new(self.storage.get_connection());
        db.search_ledger(query, category, days, breaking_only, limit, offset)
    }

    pub fn get_ledger_entries(&self, entity: &str) -> Result<Vec<LedgerEntry>, LedgerError> {
        self.get_ledger_entries_paginated(entity, 1000, 0)
    }

    pub fn get_ledger_entries_paginated(
        &self,
        entity: &str,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<LedgerEntry>, LedgerError> {
        let normalized = self.entity_normalized(entity)?;
        let db = LedgerDb::new(self.storage.get_connection());
        db.get_ledger_entries_by_entity_paginated(&normalized, limit, offset)
    }

    pub fn get_all_pending(&self) -> Result<Vec<Transaction>, LedgerError> {
        let db = LedgerDb::new(self.storage.get_connection());
        db.get_all_pending()
    }

    pub fn get_all_unaudited(&self) -> Result<Vec<Transaction>, LedgerError> {
        let db = LedgerDb::new(self.storage.get_connection());
        db.get_all_unaudited()
    }

    pub fn record_token_provenance(
        &mut self,
        tx_id: &str,
        symbol_diff: Vec<(crate::index::symbols::Symbol, ProvenanceAction)>,
    ) -> Result<(), LedgerError> {
        let tx_id = self.resolve_tx_id(tx_id)?;
        let tx = {
            let db = LedgerDb::new(self.storage.get_connection());
            db.get_transaction(&tx_id)?
                .ok_or_else(|| LedgerError::NotFound(tx_id.clone()))?
        };

        let db = LedgerDb::new(self.storage.get_connection());
        for (symbol, action) in symbol_diff {
            let prov = TokenProvenance {
                id: None,
                tx_id: tx_id.clone(),
                entity: tx.entity.clone(),
                entity_normalized: tx.entity_normalized.clone(),
                symbol_name: symbol.name,
                symbol_type: format!("{:?}", symbol.kind),
                action,
            };
            db.insert_token_provenance(&prov)?;
        }
        Ok(())
    }

    pub fn entity_normalized(&self, entity: &str) -> Result<String, LedgerError> {
        let mut normalized = crate::util::path::normalize_relative_path(&self.repo_root, entity)
            .map_err(LedgerError::Config)?;

        if self.is_case_insensitive {
            normalized = normalized.to_lowercase();
        }

        Ok(normalized)
    }

    pub fn update_adr_metadata(
        &mut self,
        adr_id: &str,
        update: AdrMetadataUpdate,
    ) -> Result<(), LedgerError> {
        let db = LedgerDb::new(self.storage.get_connection());
        db.update_adr_metadata(adr_id, update)
    }

    pub fn get_adr_metadata(&self, adr_id: &str) -> Result<AdrMetadata, LedgerError> {
        let db = LedgerDb::new(self.storage.get_connection());
        db.get_adr_metadata(adr_id)?
            .ok_or_else(|| LedgerError::NotFound(format!("ADR metadata for ID {}", adr_id)))
    }

    pub fn link_adr_supersedes(
        &mut self,
        adr_id: &str,
        supersedes_id: &str,
    ) -> Result<(), LedgerError> {
        let db = LedgerDb::new(self.storage.get_connection());
        db.link_adr_supersedes(adr_id, supersedes_id)
    }

    pub fn get_transaction_files(&self, tx_id: &str) -> Result<Vec<String>, LedgerError> {
        let tx_id = self.resolve_tx_id(tx_id)?;
        let db = LedgerDb::new(self.storage.get_connection());
        let tx = db
            .get_transaction(&tx_id)?
            .ok_or_else(|| LedgerError::NotFound(tx_id.clone()))?;

        let mut files = std::collections::BTreeSet::new();

        // Only insert entity_normalized if it looks like a real file path and is not synthetic
        if is_real_file_path(&tx.entity_normalized) {
            files.insert(tx.entity_normalized);
        }

        let provs = db.get_token_provenance_for_tx(&tx_id)?;
        for prov in provs {
            if is_real_file_path(&prov.entity_normalized) {
                files.insert(prov.entity_normalized);
            }
        }

        // Check if there are other files in changed_files via snapshot_id
        let stmt = self.storage.get_connection().prepare(
            "SELECT path FROM changed_files WHERE snapshot_id = (SELECT snapshot_id FROM transactions WHERE tx_id = ?1)"
        );
        if let Ok(mut stmt) = stmt
            && let Ok(mut rows) = stmt.query([&tx_id])
        {
            while let Ok(Some(row)) = rows.next() {
                if let Ok(file_path) = row.get::<_, String>(0)
                    && is_real_file_path(&file_path)
                {
                    files.insert(file_path);
                }
            }
        }

        // Check transaction_links if the table exists
        let stmt = self.storage.get_connection().prepare(
            "SELECT entity_normalized FROM transaction_links WHERE tx_id = ?1 AND entity_type = 'FILE'"
        );
        if let Ok(mut stmt) = stmt
            && let Ok(mut rows) = stmt.query([&tx_id])
        {
            while let Ok(Some(row)) = rows.next() {
                if let Ok(file_path) = row.get::<_, String>(0)
                    && is_real_file_path(&file_path)
                {
                    files.insert(file_path);
                }
            }
        }

        Ok(files.into_iter().collect())
    }
}

/// Returns true if the string looks like a real file path and is not a synthetic entity.
fn is_real_file_path(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    // Filter out synthetic prefixes
    if s.contains("drift_adoption:") {
        return false;
    }
    // Filter out UUIDs (which are often used as synthetic entity IDs)
    if uuid::Uuid::parse_str(s).is_ok() {
        return false;
    }
    // Most real file paths in a repo will have a dot (extension) or a slash (directory).
    // However, root-level files like LICENSE, Makefile, or Dockerfile are valid.
    // We reject only if it looks like a bare internal ID (alphanumeric only, no punctuation, long).
    if !s.contains('.') && !s.contains('/') && !s.contains('\\') && s.len() > 20 {
        return false;
    }

    true
}

/// Capture the author of the current operation from `git config`.
///
/// Resolution order (per the M8 spec
/// `conductor/trackM8/spec.md:56`):
/// 1. `git config user.name` in `repo_root`
/// 2. `git config user.email` in `repo_root`
/// 3. The literal string `"unknown"`
///
/// Per M8 opencode-review M4: the `git config` invocations are run
/// with `current_dir(repo_root)` so the result is determined by the
/// ledger's own repo config, not by the process CWD. This is the
/// correct behavior for any non-CLI caller (e.g. the web server
/// invoking `commit_change`) and removes the need for the
/// `set_current_dir` hack the original test had to use to make
/// author capture work in a `tempdir`.
///
/// The function is intentionally non-blocking and best-effort: a
/// missing git config, a non-zero `git` exit code, or a non-UTF-8
/// stdout all fall through to the next step. The literal `"unknown"`
/// fallback matches the m43 migration's `DEFAULT 'unknown'` so newly
/// created entries are indistinguishable from backfilled entries.
///
/// Used by `commit_change`, `rollback_change`, and `reconcile_drift`
/// so all three entry-construction sites record a real author instead
/// of hardcoding `"unknown"`. Per the M8 review (`output/m8-review-1.md`
/// M4), this brings rollbacks and reconciliations up to spec — the
/// plan's "no retroactive git-blame" carve-out was about *existing*
/// rows, not about entries we are creating *right now* and for which
/// we know who ran the command.
fn capture_git_author(repo_root: &Path) -> String {
    let read = |key: &str| -> Option<String> {
        std::process::Command::new("git")
            .args(["config", key])
            .current_dir(repo_root)
            .output()
            .ok()
            .and_then(|o| {
                if o.status.success() {
                    let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
                    if s.is_empty() { None } else { Some(s) }
                } else {
                    None
                }
            })
    };
    read("user.name")
        .or_else(|| read("user.email"))
        .unwrap_or_else(|| "unknown".to_string())
}

#[cfg(test)]
mod capture_git_author_tests {
    use super::capture_git_author;
    use std::process::Command;

    /// When the supplied `repo_root` is a tempdir that is *not* a
    /// git repository and has no `user.name` / `user.email` set, the
    /// fallback chain must terminate in a non-empty string. We use a
    /// non-git tempdir so the chain is forced past `git config` (which
    /// would return non-zero for a non-repo CWD) and the test is
    /// independent of the test environment's global git config.
    #[test]
    fn capture_git_author_falls_back_to_unknown() {
        let tmp = tempfile::tempdir().expect("tempdir");

        // `repo_root` is a tempdir; it is intentionally *not* a git
        // repo, so `git config` exits non-zero. This exercises the
        // final fallback branch deterministically without mutating
        // process state.
        let author = capture_git_author(tmp.path());

        // The strict guarantee we can test is that the function does
        // not panic and returns a non-empty string. The exact value
        // depends on the test environment (a global git config could
        // legitimately return a real name even when the tempdir is
        // not a git repo — git walks up looking for a config).
        assert!(
            !author.is_empty(),
            "capture_git_author must always return a non-empty string"
        );
    }

    /// When the supplied `repo_root` is a tempdir that *is* a git
    /// repository with a known `user.name`, the function returns
    /// that name. The test in `tests/integration/web_api.rs`
    /// (`test_api_ledger_includes_author_from_git_config`) is the
    /// end-to-end version of this; this is a fast unit-level check
    /// for the helper itself.
    #[test]
    fn capture_git_author_reads_user_name_from_repo_root() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let run = |args: &[&str]| {
            Command::new("git")
                .args(args)
                .current_dir(tmp.path())
                .output()
                .expect("git invocation");
        };
        run(&["init", "--quiet"]);
        run(&["config", "user.name", "Helper Test User"]);
        run(&["config", "user.email", "helper@test.local"]);

        let author = capture_git_author(tmp.path());
        assert_eq!(author, "Helper Test User");
    }
}
