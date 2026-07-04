use crate::git::repo::open_repo;
use crate::impact::enrichment::{EnrichmentContext, EnrichmentProvider};
use crate::impact::hotspots::calculate_hotspots;
use crate::impact::packet::ImpactPacket;
use crate::impact::temporal::GixHistoryProvider;
use miette::Result;
use tracing::{debug, warn};

pub struct HotspotProvider;

impl EnrichmentProvider for HotspotProvider {
    fn name(&self) -> &'static str {
        "Hotspot Enrichment Provider"
    }

    fn enrich(&self, context: &EnrichmentContext, packet: &mut ImpactPacket) -> Result<()> {
        debug!("Calculating hotspots...");

        let repo = open_repo(&context.project_root)
            .map_err(|e| miette::miette!("Failed to open repo for hotspot analysis: {}", e))?;

        let history_provider = GixHistoryProvider::new(&repo);

        match calculate_hotspots(
            context.storage,
            &history_provider,
            &crate::impact::hotspots::HotspotQuery {
                commits: context.config.hotspots.max_commits,
                limit: context.config.hotspots.limit,
                all_parents: context.config.temporal.all_parents,
                decay_half_life: context.config.hotspots.decay_half_life,
                ..Default::default()
            },
        ) {
            Ok(hotspots) => {
                packet.hotspots = hotspots;
            }
            Err(e) => {
                warn!("Hotspot analysis failed: {e}");
                context.add_warning(format!("Hotspot analysis failed: {e}"));
            }
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
    fn enrich_calculates_hotspots() {
        let mut conn = Connection::open_in_memory().unwrap();
        get_migrations().to_latest(&mut conn).unwrap();
        let storage = StorageManager::init_from_conn(conn);
        let config = crate::config::model::Config::default();
        let context = EnrichmentContext {
            storage: &storage,
            config: &config,
            file_id_map: HashMap::new(),
            project_root: std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from(r"C:\dev\ledgerful")),
            warnings: Arc::new(Mutex::new(Vec::new())),
            deadline: std::time::Instant::now() + std::time::Duration::from_secs(120),
        };
        let mut packet = ImpactPacket::default();

        HotspotProvider.enrich(&context, &mut packet).unwrap();

        assert!(packet.hotspots.len() <= config.hotspots.limit);
    }
}
