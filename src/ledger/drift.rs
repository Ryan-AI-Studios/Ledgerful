use chrono::Utc;
use rusqlite::Connection;
use std::path::PathBuf;

use crate::config::model::Config;
use crate::ledger::db::LedgerDb;
use crate::ledger::error::LedgerError;
use crate::ledger::session::get_session_id;
use crate::ledger::types::*;

fn parse_category(s: &str) -> Option<Category> {
    serde_json::from_str(&format!("\"{}\"", s)).ok()
}

pub struct DriftManager<'a> {
    conn: &'a mut Connection,
    repo_root: PathBuf,
    is_case_insensitive: bool,
    config: Config,
}

impl<'a> DriftManager<'a> {
    pub fn new(conn: &'a mut Connection, repo_root: PathBuf, config: Config) -> Self {
        let is_case_insensitive = repo_root.join(".GIT").exists();
        Self {
            conn,
            repo_root,
            is_case_insensitive,
            config,
        }
    }

    pub fn process_event(&mut self, path: &str) -> Result<(), LedgerError> {
        let normalized = self.entity_normalized(path)?;
        let db = LedgerDb::new(self.conn);

        // Check for PENDING transaction
        if db.get_pending_by_entity(&normalized)?.is_some() {
            return Ok(());
        }

        // Determine category from watcher patterns (config + DB)
        let category = self.resolve_category(path);

        // If none, call upsert_unaudited_transaction
        let now = Utc::now().to_rfc3339();
        let tx = Transaction {
            tx_id: uuid::Uuid::new_v4().to_string(),
            operation_id: None,
            status: "UNAUDITED".to_string(),
            category,
            entity: path.to_string(),
            entity_normalized: normalized,
            planned_action: None,
            session_id: get_session_id().to_string(),
            source: "WATCHER".to_string(),
            started_at: now.clone(),
            resolved_at: None,
            detected_at: Some(now.clone()),
            drift_count: 1,
            first_seen_at: Some(now.clone()),
            last_seen_at: Some(now),
            issue_ref: None,
        };

        db.upsert_unaudited_transaction(&tx)?;
        Ok(())
    }

    fn resolve_category(&self, path: &str) -> Category {
        // 1. Check DB watcher patterns first
        let db = LedgerDb::new(self.conn);
        if let Ok(patterns) = db.get_watcher_patterns() {
            for pattern in &patterns {
                if let Ok(glob) = globset::Glob::new(&pattern.glob)
                    && let Ok(set) = globset::GlobSetBuilder::new().add(glob).build()
                    && set.is_match(path)
                    && let Some(cat) = parse_category(&pattern.category)
                {
                    return cat;
                }
            }
        }

        // 2. Check config watcher patterns
        for wp in &self.config.ledger.watcher_patterns {
            if let Ok(glob) = globset::Glob::new(&wp.glob)
                && let Ok(set) = globset::GlobSetBuilder::new().add(glob).build()
                && set.is_match(path)
                && let Some(cat) = parse_category(&wp.category)
            {
                return cat;
            }
        }

        // 3. Default fallback
        Category::Feature
    }

    fn entity_normalized(&self, entity: &str) -> Result<String, LedgerError> {
        let mut normalized = crate::util::path::normalize_relative_path(&self.repo_root, entity)
            .map_err(LedgerError::Config)?;

        if self.is_case_insensitive {
            normalized = normalized.to_lowercase();
        }

        Ok(normalized)
    }
}
