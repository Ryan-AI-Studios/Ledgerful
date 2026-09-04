use crate::cli::{HotspotArgs, HotspotSubcommands};
use crate::commands::helpers::get_layout;
use crate::config::load_config;
use crate::git::repo::open_repo;
use crate::index::warn_if_stale;
use crate::state::storage::StorageManager;
use miette::Result;
use std::env;

mod budget;
mod explain;
mod list;
mod trend;

#[cfg(test)]
mod tests;

pub use explain::{HotspotExplanation, compute_hotspot_explanation};

pub fn execute_hotspots(args: HotspotArgs) -> Result<()> {
    let current_dir = env::current_dir()
        .map_err(|e| miette::miette!("Failed to get current directory: {}", e))?;
    let repo = open_repo(&current_dir)?;
    let layout = get_layout()?;

    // --- Staleness check ---
    let config = load_config(&layout).unwrap_or_default();
    let threshold_days = config.index.stale_threshold_days;
    let need_cozo = args.semantic || args.centrality;
    // Trend --bootstrap inserts snapshots; must open write storage (true RO
    // open fails with "attempt to write a readonly database").
    let need_write = matches!(
        &args.command,
        Some(HotspotSubcommands::Trend {
            bootstrap: true,
            ..
        })
    );
    let storage = if need_write {
        layout.ensure_state_dir()?;
        let storage = StorageManager::init_with_layout(&layout)?;
        if args.auto_index {
            let (storage, _) =
                crate::index::staleness::try_auto_index(storage, threshold_days, &layout)?;
            storage
        } else {
            let _ = warn_if_stale(&storage, threshold_days);
            storage
        }
    } else if args.auto_index {
        // Missing DB must still bootstrap under --auto-index (DoD-3).
        let opened = if need_cozo {
            StorageManager::open_read_only(&layout)
        } else {
            StorageManager::open_read_only_sqlite_only(&layout)
        };
        let storage = match opened {
            Ok(s) => s,
            Err(_) => {
                layout.ensure_state_dir()?;
                StorageManager::init_with_layout(&layout)?
            }
        };
        let (storage, _) =
            crate::index::staleness::try_auto_index(storage, threshold_days, &layout)?;
        storage
    } else {
        let storage = if need_cozo {
            StorageManager::open_read_only(&layout)?
        } else {
            StorageManager::open_read_only_sqlite_only(&layout)?
        };
        let _ = warn_if_stale(&storage, threshold_days);
        storage
    };

    if let Some(command) = args.command {
        match command {
            HotspotSubcommands::Trend {
                entity,
                days,
                limit,
                all,
                json,
                bootstrap,
                samples,
                force,
            } => {
                // clap range 1.. guarantees limit ≥ 1; cast is safe for summary cap.
                let limit = usize::try_from(limit).unwrap_or(usize::MAX);
                return trend::execute_hotspots_trend(
                    &storage, &repo, &config, entity, days, limit, all, json, bootstrap, samples,
                    force,
                );
            }
            HotspotSubcommands::Explain { entity } => {
                return explain::execute_hotspots_explain(&storage, entity, &repo);
            }
            HotspotSubcommands::Budget { json } => {
                return budget::execute_hotspots_budget(&storage, &config, json);
            }
        }
    }

    list::execute_hotspots_list(args, &storage, &repo, &config, &layout)
}
