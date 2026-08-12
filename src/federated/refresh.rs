use crate::config::model::Config;
use crate::impact::packet::ImpactPacket;
use crate::state::storage::StorageManager;
use camino::Utf8PathBuf;
use miette::{Result, miette};
use std::path::Path;
use std::time::Instant;
use tracing::warn;

pub fn refresh_federated_dependencies(
    current_dir: &Path,
    packet: &ImpactPacket,
    storage: &StorageManager,
    config: &Config,
    deadline: Option<Instant>,
) -> Result<Vec<String>> {
    let utf8_current_dir = Utf8PathBuf::from_path_buf(current_dir.to_path_buf())
        .map_err(|_| miette!("Invalid UTF-8 path in current directory"))?;
    let scanner = crate::federated::scanner::FederatedScanner::new(utf8_current_dir)
        .with_federation_config(&config.federation);
    let scanner = match deadline {
        Some(d) => scanner.with_deadline(d),
        None => scanner,
    };
    let (siblings, warnings) = scanner.scan_siblings()?;

    let mut degradation_warnings = Vec::new();
    for warning in warnings {
        warn!("Federated discovery warning: {warning}");
    }

    let timestamp = chrono::Utc::now().to_rfc3339();
    for (path, schema, sibling_warnings) in siblings {
        // 0184: same path+basename writer as CLI scan (not schema.repo_name).
        let store_name = crate::federated::links::path_basename(path.as_str());
        for warning in &sibling_warnings {
            warn!(
                "Federated discovery warning for sibling '{}': {warning}",
                store_name
            );
        }
        let store_name = crate::federated::storage::upsert_federated_link_by_path(
            storage.get_connection(),
            path.as_str(),
            &timestamp,
        )?;
        crate::federated::storage::clear_federated_dependencies(
            storage.get_connection(),
            &store_name,
        )?;
        let (edges, scan_warnings) = scanner.discover_dependencies(packet, &store_name, &schema)?;
        for (local_symbol, sibling_symbol) in edges {
            crate::federated::storage::save_federated_dependencies(
                storage.get_connection(),
                &store_name,
                &local_symbol,
                &sibling_symbol,
            )?;
        }
        // 0034: collect scan degradation warnings so the caller
        // (FederatedProvider) can append them to `analysis_warnings` (DoD-5).
        for warning in scan_warnings {
            warn!(
                "Federated scan degradation for sibling '{}': {warning}",
                store_name
            );
            degradation_warnings.push(warning);
        }
    }

    // 0184: prune Dead/Self only (not "absent from this scan").
    // Use storage analysis root (layout.root), not process CWD / call-site
    // path parameter alone — same self-identity SoT as impact (0184-C).
    if let Err(e) = crate::federated::storage::prune_dead_and_self_links(
        storage.get_connection(),
        storage.root().as_str(),
    ) {
        warn!("Federated link prune failed: {e}");
    }

    // 0034: dedup cross-sibling degradation warnings. The local-repo walk
    // re-runs per sibling with identical root/budget/traversal order, so a
    // budget or deadline breach produces byte-identical warning text on
    // every sibling iteration. Without this dedup, an 8-sibling scan would
    // surface the same "hit file budget" line 8 times in
    // `analysis_warnings` (or 8 `println!`s in the CLI path) — a warning
    // flood the rest of this track exists to prevent.
    degradation_warnings.sort();
    degradation_warnings.dedup();

    Ok(degradation_warnings)
}
