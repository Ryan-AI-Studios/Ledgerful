use crate::federated::schema::FederatedSchema;
use crate::federated::storage::{get_dependencies_for_sibling, get_federated_links};
use crate::impact::packet::ImpactPacket;
use crate::ledger::db::LedgerDb;
use crate::state::storage::StorageManager;
use miette::Result;
use std::fs;
use std::panic;

fn resolve_sibling_schema(path: &str) -> Option<std::path::PathBuf> {
    let base = std::path::Path::new(path);
    // Try current path first
    let current = base.join(".ledgerful").join("state").join("schema.json");
    if current.exists() {
        return Some(current);
    }
    // Fall back to legacy path
    let legacy = base.join(".ledgerful").join("schema.json");
    if legacy.exists() {
        return Some(legacy);
    }
    None
}

pub fn check_cross_repo_impact(packet: &mut ImpactPacket, storage: &StorageManager) -> Result<()> {
    let links = get_federated_links(storage.get_connection())?;
    if links.is_empty() {
        return Ok(());
    }

    let mut impact_reasons = Vec::new();
    let db = LedgerDb::new(storage.get_connection());

    for (name, path, _) in links {
        // Skip self: if the sibling path resolves to our own repo, skip it.
        let self_path = std::env::current_dir().unwrap_or_default();
        let sibling_canonical = std::path::Path::new(&path)
            .canonicalize()
            .unwrap_or_else(|_| std::path::Path::new(&path).to_path_buf());
        let self_canonical = self_path
            .canonicalize()
            .unwrap_or_else(|_| self_path.clone());
        if sibling_canonical == self_canonical {
            continue;
        }

        let Some(schema_path) = resolve_sibling_schema(&path) else {
            impact_reasons.push(format!(
                "Cross-repo impact: Sibling '{}' schema is unavailable or invalid.",
                name
            ));
            continue;
        };

        let content = match fs::read_to_string(&schema_path) {
            Ok(c) => c,
            Err(_) => {
                impact_reasons.push(format!(
                    "Cross-repo impact: Sibling '{}' schema is unavailable or invalid.",
                    name
                ));
                continue;
            }
        };

        // JSON Safety: Wrap in catch_unwind
        let schema_result =
            panic::catch_unwind(|| serde_json::from_str::<FederatedSchema>(&content));

        let schema = match schema_result {
            Ok(Ok(s)) => s,
            _ => {
                impact_reasons.push(format!(
                    "Cross-repo impact: Sibling '{}' schema is unavailable or invalid.",
                    name
                ));
                continue;
            }
        };

        if schema.validate().is_err() {
            impact_reasons.push(format!(
                "Cross-repo impact: Sibling '{}' schema is unavailable or invalid.",
                name
            ));
            continue;
        }

        let dependencies = get_dependencies_for_sibling(storage.get_connection(), &name)?;

        for (local_symbol, sibling_symbol) in dependencies {
            // Check for removal
            let interface = schema
                .public_interfaces
                .iter()
                .find(|i| i.symbol == sibling_symbol);

            if let Some(iface) = interface {
                // If exists, check for recent imported ledger entries for this entity from this sibling
                let federated_entries = db
                    .get_federated_entries_by_entity(&iface.file, &name, 30)
                    .map_err(|e| miette::miette!("{}", e))?;

                for entry in federated_entries {
                    impact_reasons.push(format!(
                        "Cross-repo impact: Sibling '{}' modified '{}' ([FEDERATED] {})",
                        name, entry.entity, entry.summary
                    ));
                }
            } else {
                impact_reasons.push(format!(
                    "Cross-repo impact: Local symbol '{}' depends on sibling '{}' interface '{}' which was removed.",
                    local_symbol, name, sibling_symbol
                ));
            }
        }
    }

    // Engineering standard: deterministic sorting
    impact_reasons.sort();
    impact_reasons.dedup();
    packet.risk_reasons.extend(impact_reasons);

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn schema_path_current_recognized() {
        let dir = tempdir().unwrap();
        let state_dir = dir.path().join(".ledgerful").join("state");
        std::fs::create_dir_all(&state_dir).unwrap();
        let schema_file = state_dir.join("schema.json");
        std::fs::write(&schema_file, "{}").unwrap();

        let result = resolve_sibling_schema(dir.path().to_str().unwrap());
        assert!(result.is_some());
        let p = result.unwrap();
        assert!(p.ends_with("state/schema.json") || p.ends_with("state\\schema.json"));
    }

    #[test]
    fn schema_path_legacy_fallback() {
        let dir = tempdir().unwrap();
        let legacy_dir = dir.path().join(".ledgerful");
        std::fs::create_dir_all(&legacy_dir).unwrap();
        let legacy_schema = legacy_dir.join("schema.json");
        std::fs::write(&legacy_schema, "{}").unwrap();
        // No state/schema.json exists — should fall back to legacy

        let result = resolve_sibling_schema(dir.path().to_str().unwrap());
        assert!(result.is_some());
        // Should NOT return the state path (doesn't exist)
        assert!(!result.as_ref().unwrap().to_str().unwrap().contains("state"));
    }

    #[test]
    fn schema_path_missing_returns_none() {
        let dir = tempdir().unwrap();
        // No .ledgerful directory at all
        let result = resolve_sibling_schema(dir.path().to_str().unwrap());
        assert!(result.is_none());
    }
}
