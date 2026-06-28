use crate::impact::enrichment::{EnrichmentContext, EnrichmentProvider};
use crate::impact::packet::{ImpactPacket, RelevantDecision};
use crate::retrieval::query::{compute_staleness, compute_staleness_tier, query_docs};
use crate::retrieval::rerank::rerank;
use miette::Result;
use std::path::PathBuf;
use tracing::{debug, warn};

pub struct KnowledgeProvider;

impl EnrichmentProvider for KnowledgeProvider {
    fn name(&self) -> &'static str {
        "Knowledge Enrichment Provider"
    }

    fn enrich(&self, context: &EnrichmentContext, packet: &mut ImpactPacket) -> Result<()> {
        let lm_config = &context.config.local_model;

        // Skip if no embedding model configured
        if lm_config.base_url.is_empty() {
            return Ok(());
        }

        // Skip if no doc paths configured
        if context.config.docs.include.is_empty() {
            return Ok(());
        }

        let conn = context.storage.get_connection();

        // Check if doc_chunks table has any rows
        if !context.storage.table_exists_and_has_data("doc_chunks")? {
            debug!("Skipping knowledge enrichment: doc_chunks table is empty or missing.");
            return Ok(());
        }

        // Build query text from changed files
        let mut query_parts = Vec::new();
        for change in &packet.changes {
            let path = change.path.to_string_lossy().to_string();
            query_parts.push(format!("{} ({})", path, change.status));
        }

        if query_parts.is_empty() {
            return Ok(());
        }

        let query_text = query_parts.join("; ");

        // Truncate to reasonable query length
        let query_text = if query_text.len() > 2000 {
            format!("{}...", &query_text[..2000])
        } else {
            query_text.clone()
        };

        let top_n = context.config.docs.retrieval_top_k;

        // Query docs
        let retrieved = match query_docs(lm_config, conn, &query_text, top_n) {
            Ok(r) => r,
            Err(e) => {
                warn!("Doc retrieval query failed: {e}");
                context.add_warning(format!("Doc retrieval failed: {e}"));
                return Ok(());
            }
        };

        if retrieved.is_empty() {
            return Ok(());
        }

        // Rerank if configured
        let final_chunks = if !lm_config.rerank_model.is_empty() {
            rerank(
                &lm_config.base_url,
                &lm_config.rerank_model,
                &query_text,
                retrieved,
                lm_config.timeout_secs,
            )
        } else {
            retrieved
        };

        // Map to RelevantDecision, taking top top_n
        let adr_enabled = context.config.coverage.adr_staleness.enabled;
        let threshold_days = context.config.coverage.adr_staleness.threshold_days;

        let decisions: Vec<RelevantDecision> = final_chunks
            .into_iter()
            .take(top_n)
            .map(|chunk| {
                let excerpt: String = chunk.content.chars().take(200).collect();
                let mut staleness_days = None;
                let mut staleness_tier = None;

                if adr_enabled {
                    let full_path = PathBuf::from(chunk.file_path.clone());
                    if let Some(days) = compute_staleness(&full_path, threshold_days) {
                        staleness_days = Some(days);
                        staleness_tier = compute_staleness_tier(days, threshold_days);
                    }
                }

                RelevantDecision {
                    file_path: PathBuf::from(chunk.file_path),
                    heading: chunk.heading,
                    excerpt,
                    similarity: chunk.similarity,
                    rerank_score: None,
                    staleness_days,
                    staleness_tier,
                }
            })
            .collect();

        packet.relevant_decisions = decisions;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::migrations::get_migrations;
    use crate::state::storage::StorageManager;
    use rusqlite::Connection;
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    #[test]
    fn enrich_skips_when_base_url_empty() {
        let mut conn = Connection::open_in_memory().unwrap();
        get_migrations().to_latest(&mut conn).unwrap();
        let storage = StorageManager::init_from_conn(conn);
        let config = crate::config::model::Config::default();
        let context = EnrichmentContext {
            storage: &storage,
            config: &config,
            file_id_map: HashMap::new(),
            project_root: PathBuf::new(),
            warnings: Arc::new(Mutex::new(Vec::new())),
        };
        let mut packet = ImpactPacket::default();
        KnowledgeProvider.enrich(&context, &mut packet).unwrap();
        assert!(packet.relevant_decisions.is_empty());
    }

    #[test]
    fn enrich_skips_when_doc_chunks_empty() {
        let mut conn = Connection::open_in_memory().unwrap();
        get_migrations().to_latest(&mut conn).unwrap();
        let storage = StorageManager::init_from_conn(conn);
        let mut config = crate::config::model::Config::default();
        config.local_model.base_url = "http://localhost:11434".to_string();
        config.docs.include = vec!["docs/".to_string()];
        let context = EnrichmentContext {
            storage: &storage,
            config: &config,
            file_id_map: HashMap::new(),
            project_root: PathBuf::new(),
            warnings: Arc::new(Mutex::new(Vec::new())),
        };
        let mut packet = ImpactPacket::default();
        KnowledgeProvider.enrich(&context, &mut packet).unwrap();
        assert!(packet.relevant_decisions.is_empty());
    }
}
