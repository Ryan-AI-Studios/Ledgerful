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
}
