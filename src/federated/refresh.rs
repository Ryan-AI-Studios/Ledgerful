use crate::impact::packet::ImpactPacket;
use crate::state::storage::StorageManager;
use camino::Utf8PathBuf;
use miette::{Result, miette};
use std::path::Path;
use tracing::warn;

pub fn refresh_federated_dependencies(
    current_dir: &Path,
    packet: &ImpactPacket,
    storage: &StorageManager,
) -> Result<()> {
    let utf8_current_dir = Utf8PathBuf::from_path_buf(current_dir.to_path_buf())
        .map_err(|_| miette!("Invalid UTF-8 path in current directory"))?;
    let scanner = crate::federated::scanner::FederatedScanner::new(utf8_current_dir);
    let (siblings, warnings) = scanner.scan_siblings()?;

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
        for (local_symbol, sibling_symbol) in
            scanner.discover_dependencies(packet, &schema.repo_name, &schema)?
        {
            crate::federated::storage::save_federated_dependencies(
                storage.get_connection(),
                &schema.repo_name,
                &local_symbol,
                &sibling_symbol,
            )?;
        }
    }

    Ok(())
}
