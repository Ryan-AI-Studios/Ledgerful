use crate::federated::links::{present_federated_links, resolve_sibling_schema};
use crate::federated::schema::FederatedSchema;
use crate::federated::storage::{get_dependencies_for_sibling, get_federated_links};
use crate::impact::packet::ImpactPacket;
use crate::ledger::db::LedgerDb;
use crate::state::storage::StorageManager;
use miette::Result;
use std::fs;
use std::panic;

/// Where a federation signal should land on the impact packet (0129).
///
/// Schema availability is ambient federation health → `analysis_warnings`.
/// Real cross-repo change consequences → `risk_reasons`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FederationSignalKind {
    /// Sibling schema path missing, unreadable, unparseable, or invalid.
    SchemaUnavailable,
    /// Sibling modified a linked entity, or a linked interface was removed.
    RealImpact,
}

/// Pure classification for unit tests without full storage.
pub(crate) fn classify_federation_outcome(kind: FederationSignalKind) -> FederationChannel {
    match kind {
        FederationSignalKind::SchemaUnavailable => FederationChannel::AnalysisWarning,
        FederationSignalKind::RealImpact => FederationChannel::RiskReason,
    }
}

/// Target channel for a classified federation signal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FederationChannel {
    AnalysisWarning,
    RiskReason,
}

/// Greppable schema-miss message (same text on analysis_warnings as historical risk).
pub(crate) fn schema_unavailable_message(sibling: &str) -> String {
    format!("Cross-repo impact: Sibling '{sibling}' schema is unavailable or invalid.")
}

/// Route a federation signal to risk_reasons or analysis_warnings per classification.
fn push_federation_signal(
    kind: FederationSignalKind,
    message: String,
    risk_reasons: &mut Vec<String>,
    analysis_warnings: &mut Vec<String>,
) {
    match classify_federation_outcome(kind) {
        FederationChannel::AnalysisWarning => analysis_warnings.push(message),
        FederationChannel::RiskReason => risk_reasons.push(message),
    }
}

pub fn check_cross_repo_impact(packet: &mut ImpactPacket, storage: &StorageManager) -> Result<()> {
    let raw = get_federated_links(storage.get_connection())?;
    if raw.is_empty() {
        return Ok(());
    }

    // 0184: Live collapsed set only (path identity). Self/Dead omitted —
    // husks must not emit 0129 schema-unavailable noise. Repo root is
    // storage analysis root (layout.root), not process CWD.
    let repo_root = storage.root().as_str();
    let presented = present_federated_links(&raw, repo_root);
    if presented.live.is_empty() {
        return Ok(());
    }

    let mut impact_reasons = Vec::new();
    let mut analysis_warnings = Vec::new();
    let db = LedgerDb::new(storage.get_connection());

    for link in &presented.live {
        let name = &link.name;
        let path = &link.path;

        let Some(schema_path) = resolve_sibling_schema(path) else {
            // Live classify requires schema on disk; race / unreadable → 0129.
            push_federation_signal(
                FederationSignalKind::SchemaUnavailable,
                schema_unavailable_message(name),
                &mut impact_reasons,
                &mut analysis_warnings,
            );
            continue;
        };

        let content = match fs::read_to_string(&schema_path) {
            Ok(c) => c,
            Err(_) => {
                push_federation_signal(
                    FederationSignalKind::SchemaUnavailable,
                    schema_unavailable_message(name),
                    &mut impact_reasons,
                    &mut analysis_warnings,
                );
                continue;
            }
        };

        // JSON Safety: Wrap in catch_unwind
        let schema_result =
            panic::catch_unwind(|| serde_json::from_str::<FederatedSchema>(&content));

        let schema = match schema_result {
            Ok(Ok(s)) => s,
            _ => {
                push_federation_signal(
                    FederationSignalKind::SchemaUnavailable,
                    schema_unavailable_message(name),
                    &mut impact_reasons,
                    &mut analysis_warnings,
                );
                continue;
            }
        };

        if schema.validate().is_err() {
            push_federation_signal(
                FederationSignalKind::SchemaUnavailable,
                schema_unavailable_message(name),
                &mut impact_reasons,
                &mut analysis_warnings,
            );
            continue;
        }

        // Deps may still be keyed under an older cached name until scan
        // re-persists; look up by presented basename (post-migrate).
        let dependencies = get_dependencies_for_sibling(storage.get_connection(), name)?;

        for (local_symbol, sibling_symbol) in dependencies {
            // Check for removal
            let interface = schema
                .public_interfaces
                .iter()
                .find(|i| i.symbol == sibling_symbol);

            if let Some(iface) = interface {
                // If exists, check for recent imported ledger entries for this entity from this sibling
                let federated_entries = db
                    .get_federated_entries_by_entity(&iface.file, name, 30)
                    .map_err(|e| miette::miette!("{}", e))?;

                for entry in federated_entries {
                    push_federation_signal(
                        FederationSignalKind::RealImpact,
                        format!(
                            "Cross-repo impact: Sibling '{}' modified '{}' ([FEDERATED] {})",
                            name, entry.entity, entry.summary
                        ),
                        &mut impact_reasons,
                        &mut analysis_warnings,
                    );
                }
            } else {
                push_federation_signal(
                    FederationSignalKind::RealImpact,
                    format!(
                        "Cross-repo impact: Local symbol '{}' depends on sibling '{}' interface '{}' which was removed.",
                        local_symbol, name, sibling_symbol
                    ),
                    &mut impact_reasons,
                    &mut analysis_warnings,
                );
            }
        }
    }

    // Engineering standard: deterministic sorting
    impact_reasons.sort();
    impact_reasons.dedup();
    analysis_warnings.sort();
    analysis_warnings.dedup();
    packet.risk_reasons.extend(impact_reasons);
    packet.analysis_warnings.extend(analysis_warnings);

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::impact::packet::RiskLevel;
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

    #[test]
    fn husk_link_does_not_emit_schema_unavailable() {
        use crate::federated::storage::update_federated_link;
        use crate::state::migrations::get_migrations;
        use rusqlite::Connection;

        let root = tempdir().unwrap();
        let husk = tempdir().unwrap();
        std::fs::write(husk.path().join("only.md"), "x").unwrap();

        let mut conn = Connection::open_in_memory().unwrap();
        get_migrations().to_latest(&mut conn).unwrap();
        update_federated_link(
            &conn,
            "husk",
            husk.path().to_str().unwrap(),
            "2026-01-01T00:00:00Z",
        )
        .unwrap();

        // Storage root = analysis repo root (layout.root), not CWD.
        let db_path = root
            .path()
            .join(".ledgerful")
            .join("state")
            .join("ledger.db");
        std::fs::create_dir_all(db_path.parent().unwrap()).unwrap();
        // Use in-memory storage with explicit root via init_from_conn shape:
        // open real db under root so root() derives correctly.
        drop(conn);
        let storage = crate::state::storage::StorageManager::init(&db_path).unwrap();
        // Re-seed husk into the on-disk db
        update_federated_link(
            storage.get_connection(),
            "husk",
            husk.path().to_str().unwrap(),
            "2026-01-01T00:00:00Z",
        )
        .unwrap();

        let mut packet = ImpactPacket::default();
        check_cross_repo_impact(&mut packet, &storage).unwrap();
        assert!(
            !packet
                .analysis_warnings
                .iter()
                .any(|w| w.contains("schema is unavailable")),
            "husk must not emit schema-unavailable: {:?}",
            packet.analysis_warnings
        );
        assert!(packet.risk_reasons.is_empty());
        storage.shutdown().unwrap();
    }

    #[test]
    fn same_path_dup_names_processed_once() {
        use crate::federated::schema::{FederatedSchema, PublicInterface};
        use crate::federated::storage::update_federated_link;
        use crate::index::symbols::SymbolKind;

        let root = tempdir().unwrap();
        let peer = tempdir().unwrap();
        let state = peer.path().join(".ledgerful").join("state");
        std::fs::create_dir_all(&state).unwrap();
        let schema = FederatedSchema::new(
            "stale-export-name".into(),
            vec![PublicInterface {
                symbol: "iface".into(),
                file: "src/lib.rs".into(),
                kind: SymbolKind::Function,
            }],
        );
        std::fs::write(
            state.join("schema.json"),
            serde_json::to_string_pretty(&schema).unwrap(),
        )
        .unwrap();

        let db_path = root
            .path()
            .join(".ledgerful")
            .join("state")
            .join("ledger.db");
        std::fs::create_dir_all(db_path.parent().unwrap()).unwrap();
        let storage = crate::state::storage::StorageManager::init(&db_path).unwrap();
        let peer_s = peer.path().to_str().unwrap();
        update_federated_link(storage.get_connection(), "AI-Brains", peer_s, "t1").unwrap();
        update_federated_link(storage.get_connection(), "ai-brains", peer_s, "t2").unwrap();

        let mut packet = ImpactPacket::default();
        check_cross_repo_impact(&mut packet, &storage).unwrap();
        // No deps → no signals; success means collapse did not double-error.
        let unavailable: Vec<_> = packet
            .analysis_warnings
            .iter()
            .filter(|w| w.contains("schema is unavailable"))
            .collect();
        assert!(
            unavailable.is_empty(),
            "live peer should parse once, not warn: {:?}",
            unavailable
        );
        storage.shutdown().unwrap();
    }

    #[test]
    fn classify_schema_unavailable_is_analysis_warning() {
        assert_eq!(
            classify_federation_outcome(FederationSignalKind::SchemaUnavailable),
            FederationChannel::AnalysisWarning
        );
    }

    #[test]
    fn classify_real_impact_is_risk_reason() {
        assert_eq!(
            classify_federation_outcome(FederationSignalKind::RealImpact),
            FederationChannel::RiskReason
        );
    }

    #[test]
    fn schema_unavailable_message_is_greppable() {
        let msg = schema_unavailable_message("changeguard");
        assert_eq!(
            msg,
            "Cross-repo impact: Sibling 'changeguard' schema is unavailable or invalid."
        );
        // Must never land on risk_reasons after 0129 — simulate packet routing.
        let mut packet = ImpactPacket::default();
        match classify_federation_outcome(FederationSignalKind::SchemaUnavailable) {
            FederationChannel::AnalysisWarning => {
                packet.analysis_warnings.push(msg.clone());
            }
            FederationChannel::RiskReason => {
                packet.risk_reasons.push(msg.clone());
            }
        }
        assert!(
            packet.risk_reasons.is_empty(),
            "schema-miss must not pollute risk_reasons: {:?}",
            packet.risk_reasons
        );
        assert_eq!(packet.analysis_warnings, vec![msg]);
    }

    #[test]
    fn real_federated_modify_message_routes_to_risk_reasons() {
        let mut packet = ImpactPacket::default();
        let msg =
            "Cross-repo impact: Sibling 'other' modified 'iface' ([FEDERATED] bumped version)"
                .to_string();
        match classify_federation_outcome(FederationSignalKind::RealImpact) {
            FederationChannel::AnalysisWarning => packet.analysis_warnings.push(msg.clone()),
            FederationChannel::RiskReason => packet.risk_reasons.push(msg.clone()),
        }
        assert!(packet.analysis_warnings.is_empty());
        assert_eq!(packet.risk_reasons, vec![msg]);
    }

    #[test]
    fn real_interface_removed_message_routes_to_risk_reasons() {
        let mut packet = ImpactPacket::default();
        let msg = "Cross-repo impact: Local symbol 'foo' depends on sibling 'other' interface 'bar' which was removed."
            .to_string();
        match classify_federation_outcome(FederationSignalKind::RealImpact) {
            FederationChannel::AnalysisWarning => packet.analysis_warnings.push(msg.clone()),
            FederationChannel::RiskReason => packet.risk_reasons.push(msg.clone()),
        }
        assert!(packet.analysis_warnings.is_empty());
        assert_eq!(packet.risk_reasons, vec![msg]);
    }

    #[test]
    fn empty_tree_with_schema_warnings_only_finalizes_low() {
        let mut packet = ImpactPacket::default();
        // Simulate federated enrichment: schema-miss on analysis_warnings only.
        packet
            .analysis_warnings
            .push(schema_unavailable_message("changeguard"));
        packet
            .analysis_warnings
            .push(schema_unavailable_message("changeguard")); // dup
        packet.finalize();
        // Empty changes + empty risk_reasons → Low + "No changes detected"
        packet.finalize_risk_level(0, false);
        assert_eq!(packet.risk_level, RiskLevel::Low);
        assert!(
            packet
                .risk_reasons
                .iter()
                .any(|r| r == "No changes detected"),
            "expected No changes detected, got {:?}",
            packet.risk_reasons
        );
        assert!(
            !packet
                .risk_reasons
                .iter()
                .any(|r| r.contains("schema is unavailable")),
            "schema miss must not appear in risk_reasons: {:?}",
            packet.risk_reasons
        );
        // finalize sorts + dedups analysis_warnings
        assert_eq!(
            packet.analysis_warnings,
            vec![schema_unavailable_message("changeguard")]
        );
    }

    #[test]
    fn finalize_sorts_and_dedups_analysis_warnings() {
        let mut packet = ImpactPacket::default();
        packet.analysis_warnings.push("z-warn".into());
        packet.analysis_warnings.push("a-warn".into());
        packet.analysis_warnings.push("a-warn".into());
        packet.finalize();
        assert_eq!(
            packet.analysis_warnings,
            vec!["a-warn".to_string(), "z-warn".to_string()]
        );
    }
}
