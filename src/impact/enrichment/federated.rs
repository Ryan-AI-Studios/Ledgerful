use crate::impact::enrichment::{EnrichmentContext, EnrichmentProvider};
use crate::impact::packet::ImpactPacket;
use miette::Result;
use tracing::warn;

pub struct FederatedProvider;

impl EnrichmentProvider for FederatedProvider {
    fn name(&self) -> &'static str {
        "Federated Intelligence Enrichment Provider"
    }

    fn enrich(&self, context: &EnrichmentContext, packet: &mut ImpactPacket) -> Result<()> {
        // 0147 B2: defense-in-depth empty-tree guard — no federation walk or
        // cross-repo checks when there are zero structural seeds (orchestrator
        // B1 should already short-circuit; this protects future direct callers).
        if packet.tree_clean && packet.changes.is_empty() {
            return Ok(());
        }

        // Soft-open / RO change-context: skip discovery refresh writes.
        // Cross-repo read-only impact may still run below.
        if !context.storage.is_read_only {
            match crate::federated::refresh::refresh_federated_dependencies(
                &context.project_root,
                packet,
                context.storage,
                context.config,
                Some(context.deadline),
            ) {
                Ok(degradation_warnings) => {
                    // 0034: surface scan degradation warnings (budget hit,
                    // deadline breached) to the packet's analysis_warnings so
                    // the impact output records which provider truncated, not
                    // just the log sink (DoD-5).
                    packet.analysis_warnings.extend(degradation_warnings);
                }
                Err(e) => {
                    warn!("Federated discovery refresh failed: {e}");
                }
            }
        } else {
            tracing::debug!("Storage is read-only; skipping federated discovery refresh writes");
        }

        // Cross-repo impact analysis
        if let Err(e) = crate::federated::impact::check_cross_repo_impact(packet, context.storage) {
            warn!("Federated impact analysis failed: {e}");
        }

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
    fn enrich_returns_ok_with_empty_db() {
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
            deadline: std::time::Instant::now() + std::time::Duration::from_secs(120),
        };
        let mut packet = ImpactPacket::default();

        FederatedProvider.enrich(&context, &mut packet).unwrap();
    }

    /// 0147 B2: empty tree_clean + empty changes is a pure no-op (no federation
    /// refresh, no cross-repo checks / warnings).
    #[test]
    fn enrich_empty_tree_is_noop() {
        let mut conn = Connection::open_in_memory().unwrap();
        get_migrations().to_latest(&mut conn).unwrap();
        let storage = StorageManager::init_from_conn(conn);
        let config = crate::config::model::Config::default();
        let context = EnrichmentContext {
            storage: &storage,
            config: &config,
            file_id_map: HashMap::new(),
            // Non-empty root would normally trigger a federation walk if the
            // guard were missing; keep it empty so a regression does not hang.
            project_root: PathBuf::new(),
            warnings: Arc::new(Mutex::new(Vec::new())),
            deadline: std::time::Instant::now() + std::time::Duration::from_secs(120),
        };
        let mut packet = ImpactPacket {
            tree_clean: true,
            changes: Vec::new(),
            head_hash: Some("deadbeef".to_string()),
            ..ImpactPacket::default()
        };

        FederatedProvider.enrich(&context, &mut packet).unwrap();

        assert!(
            packet.analysis_warnings.is_empty(),
            "empty-tree federated enrich must not add analysis_warnings, got {:?}",
            packet.analysis_warnings
        );
    }
}
