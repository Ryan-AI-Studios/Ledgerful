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
        for warning in &sibling_warnings {
            warn!(
                "Federated discovery warning for sibling '{}': {warning}",
                schema.repo_name
            );
        }
        crate::federated::storage::update_federated_link(
            storage.get_connection(),
            &schema.repo_name,
            path.as_str(),
            &timestamp,
        )?;
        crate::federated::storage::clear_federated_dependencies(
            storage.get_connection(),
            &schema.repo_name,
        )?;
        let (edges, scan_warnings) =
            scanner.discover_dependencies(packet, &schema.repo_name, &schema)?;
        for (local_symbol, sibling_symbol) in edges {
            crate::federated::storage::save_federated_dependencies(
                storage.get_connection(),
                &schema.repo_name,
                &local_symbol,
                &sibling_symbol,
            )?;
        }
        // 0034: collect scan degradation warnings so the caller
        // (FederatedProvider) can append them to `analysis_warnings` (DoD-5).
        for warning in scan_warnings {
            warn!(
                "Federated scan degradation for sibling '{}': {warning}",
                schema.repo_name
            );
            degradation_warnings.push(warning);
        }
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
