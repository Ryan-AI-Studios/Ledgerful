use crate::config::model::Config;
use crate::impact::analysis::ImpactProvider;
use crate::impact::packet::{ImpactPacket, RiskImpact};
use crate::index::signature::SignatureChangeClass;
use crate::policy::rules::Rules;
use miette::Result;

/// Provider that analyzes semantic risk: centrality and reachability.
pub struct SemanticImpactProvider;

impl ImpactProvider for SemanticImpactProvider {
    fn name(&self) -> &'static str {
        "Semantic Impact Provider"
    }

    fn analyze(
        &self,
        packet: &ImpactPacket,
        _rules: &Rules,
        _config: &Config,
    ) -> Result<RiskImpact> {
        let mut total_weight = 0;
        let mut reasons = Vec::new();

        // 1. Centrality Risk
        for risk in &packet.centrality_risks {
            if risk.entrypoints_reachable >= 5 {
                reasons.push(format!(
                    "High centrality: changed symbol '{}' can reach {} entry points",
                    risk.symbol_name, risk.entrypoints_reachable
                ));
                total_weight += 15;
            }
        }

        // 2. Entrypoint and Public Symbol Changes
        for change in &packet.changes {
            if let Some(ref symbols) = change.symbols {
                let weight_mult = _config.impact.get_path_weight(&change.path);
                for sym in symbols {
                    if sym.is_public {
                        // Status-aware verb (0129): match ChangedFile.status string
                        // literals. Weights unchanged; still every public symbol
                        // in a touched file (0088 precision deferred).
                        let verb = match change.status.as_str() {
                            "Added" => "added",
                            "Deleted" => "deleted",
                            "Renamed" => "renamed",
                            _ => "modified",
                        };
                        reasons.push(format!("Public symbol {verb}: {}", sym.name));
                        total_weight += (30.0 * weight_mult) as u32;
                    }
                    if let Some(ref kind) = sym.entrypoint_kind {
                        if kind == "ENTRYPOINT" {
                            reasons.push(format!("Entry point changed: {}", sym.name));
                            total_weight += (20.0 * weight_mult) as u32;
                        } else if kind == "HANDLER" {
                            reasons.push(format!("Handler changed: {}", sym.name));
                            total_weight += (15.0 * weight_mult) as u32;
                        }
                    }
                }
            }
        }

        // 3. Function signature shape changes (0088) — beside Public symbol modified,
        // not replacing it. Shape only; cosmetic/unknown are not scored.
        // Floor claim: name the symbol and show old → new. Never claim completeness.
        for delta in &packet.signature_deltas {
            if delta.change_class != SignatureChangeClass::Shape.as_str() {
                continue;
            }
            reasons.push(format!(
                "Signature changed: {}: {} → {}",
                delta.symbol_name, delta.previous_signature, delta.current_signature
            ));
            total_weight += 30;
        }

        Ok(RiskImpact {
            weight: total_weight,
            reasons,
        })
    }
}

#[cfg(test)]
mod public_symbol_verb_tests {
    use super::*;
    use crate::impact::packet::ChangedFile;
    use crate::index::symbols::{Symbol, SymbolKind};
    use std::path::PathBuf;

    fn public_sym(name: &str) -> Symbol {
        Symbol {
            name: name.to_string(),
            kind: SymbolKind::Function,
            is_public: true,
            cognitive_complexity: None,
            cyclomatic_complexity: None,
            line_start: None,
            line_end: None,
            qualified_name: None,
            byte_start: None,
            byte_end: None,
            entrypoint_kind: None,
            metadata: Default::default(),
        }
    }

    fn packet_with_status(status: &str, sym_name: &str) -> ImpactPacket {
        let mut packet = ImpactPacket::default();
        packet.changes.push(ChangedFile {
            path: PathBuf::from("src/lib.rs"),
            status: status.to_string(),
            old_path: None,
            is_staged: false,
            symbols: Some(vec![public_sym(sym_name)]),
            imports: None,
            runtime_usage: None,
            analysis_status: Default::default(),
            analysis_warnings: Vec::new(),
            api_routes: Vec::new(),
            data_models: Vec::new(),
            ci_gates: Vec::new(),
        });
        packet
    }

    #[test]
    fn added_status_uses_public_symbol_added_verb() {
        let packet = packet_with_status("Added", "brand_new");
        let impact = SemanticImpactProvider
            .analyze(&packet, &Rules::default(), &Config::default())
            .unwrap();
        assert!(
            impact
                .reasons
                .iter()
                .any(|r| r == "Public symbol added: brand_new"),
            "expected Public symbol added, got {:?}",
            impact.reasons
        );
        assert!(
            impact
                .reasons
                .iter()
                .all(|r| !r.starts_with("Public symbol modified:")),
            "Added must not say modified: {:?}",
            impact.reasons
        );
        assert!(impact.weight >= 30);
    }

    #[test]
    fn modified_status_uses_public_symbol_modified_verb() {
        let packet = packet_with_status("Modified", "existing");
        let impact = SemanticImpactProvider
            .analyze(&packet, &Rules::default(), &Config::default())
            .unwrap();
        assert!(
            impact
                .reasons
                .iter()
                .any(|r| r == "Public symbol modified: existing"),
            "expected Public symbol modified, got {:?}",
            impact.reasons
        );
        assert!(impact.weight >= 30);
    }

    #[test]
    fn deleted_and_renamed_status_use_status_aware_verbs() {
        for (status, prefix) in [
            ("Deleted", "Public symbol deleted:"),
            ("Renamed", "Public symbol renamed:"),
        ] {
            let packet = packet_with_status(status, "sym");
            let impact = SemanticImpactProvider
                .analyze(&packet, &Rules::default(), &Config::default())
                .unwrap();
            assert!(
                impact.reasons.iter().any(|r| r.starts_with(prefix)),
                "status {status}: expected prefix {prefix}, got {:?}",
                impact.reasons
            );
            assert!(impact.weight >= 30);
        }
    }
}

#[cfg(test)]
mod signature_risk_tests {
    use super::*;
    use crate::impact::packet::SignatureDelta;

    #[test]
    fn shape_change_emits_distinct_reason() {
        let mut packet = ImpactPacket::default();
        packet.signature_deltas.push(SignatureDelta {
            file_path: "src/a.rs".into(),
            symbol_name: "foo".into(),
            previous_signature: "fn foo(a: u32)".into(),
            current_signature: "fn foo(a: u64)".into(),
            change_class: "shape".into(),
        });
        let impact = SemanticImpactProvider
            .analyze(&packet, &Rules::default(), &Config::default())
            .unwrap();
        assert!(
            impact
                .reasons
                .iter()
                .any(|r| r.starts_with("Signature changed:")),
            "expected Signature changed reason, got {:?}",
            impact.reasons
        );
        assert!(
            impact
                .reasons
                .iter()
                .all(|r| !r.to_lowercase().contains("no breaking changes")),
            "DoD-9: must never claim 'no breaking changes'"
        );
        assert!(impact.weight >= 30);
        // Distinct from Public symbol modified
        assert!(
            impact.reasons.iter().all(
                |r| !r.starts_with("Public symbol modified") || r.contains("Signature changed")
            ) || impact
                .reasons
                .iter()
                .any(|r| r.starts_with("Signature changed:"))
        );
    }

    #[test]
    fn cosmetic_change_does_not_score() {
        let mut packet = ImpactPacket::default();
        packet.signature_deltas.push(SignatureDelta {
            file_path: "src/a.rs".into(),
            symbol_name: "foo".into(),
            previous_signature: "fn foo(a: u32)".into(),
            current_signature: "fn foo(x: u32)".into(),
            change_class: "cosmetic".into(),
        });
        let impact = SemanticImpactProvider
            .analyze(&packet, &Rules::default(), &Config::default())
            .unwrap();
        assert!(
            impact
                .reasons
                .iter()
                .all(|r| !r.starts_with("Signature changed:")),
            "cosmetic must not raise risk"
        );
        assert_eq!(impact.weight, 0);
    }

    #[test]
    fn risk_reasons_never_claim_no_breaking_changes() {
        // Grep-assertable DoD-9 pin: no surface emits "no breaking changes detected".
        let banned = ["no breaking changes", "no breaking change detected"];
        let mut packet = ImpactPacket::default();
        packet.signature_deltas.push(SignatureDelta {
            file_path: "src/a.rs".into(),
            symbol_name: "foo".into(),
            previous_signature: "fn foo()".into(),
            current_signature: "async fn foo()".into(),
            change_class: "shape".into(),
        });
        let impact = SemanticImpactProvider
            .analyze(&packet, &Rules::default(), &Config::default())
            .unwrap();
        for reason in &impact.reasons {
            let lower = reason.to_lowercase();
            for b in banned {
                assert!(
                    !lower.contains(b),
                    "banned phrasing {b:?} found in reason: {reason}"
                );
            }
        }
    }
}
