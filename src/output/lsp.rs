use crate::impact::packet::{ImpactPacket, RiskLevel};
use std::collections::HashMap;
use std::path::PathBuf;
use tower_lsp_server::ls_types::{Diagnostic, DiagnosticSeverity, Position, Range};

/// Maps an ImpactPacket to a collection of LSP Diagnostics, grouped by file path.
pub fn map_impact_to_diagnostics(packet: &ImpactPacket) -> HashMap<PathBuf, Vec<Diagnostic>> {
    let mut diagnostics_map: HashMap<PathBuf, Vec<Diagnostic>> = HashMap::new();

    for file in &packet.changes {
        let mut diagnostics = Vec::new();

        // 1. Map file-level risk and status
        // We use informational diagnostics to show the risk level of the change at the top of the file.
        let severity = match packet.risk_level {
            RiskLevel::High => DiagnosticSeverity::ERROR,
            RiskLevel::Medium => DiagnosticSeverity::WARNING,
            RiskLevel::Low => DiagnosticSeverity::INFORMATION,
        };

        if !packet.risk_reasons.is_empty() {
            diagnostics.push(Diagnostic {
                range: Range::new(Position::new(0, 0), Position::new(0, 0)),
                severity: Some(severity),
                code: Some(tower_lsp_server::ls_types::NumberOrString::String(
                    "risk".to_string(),
                )),
                code_description: None,
                source: Some("Ledgerful".to_string()),
                message: format!(
                    "Risk Level: {:?}. Reasons: {}",
                    packet.risk_level,
                    packet.risk_reasons.join(", ")
                ),
                related_information: None,
                tags: None,
                data: None,
            });
        }

        // 2. Map Analysis Warnings
        for warning in &file.analysis_warnings {
            diagnostics.push(Diagnostic {
                range: Range::new(Position::new(0, 0), Position::new(0, 0)),
                severity: Some(DiagnosticSeverity::WARNING),
                code: Some(tower_lsp_server::ls_types::NumberOrString::String(
                    "analysis-warning".to_string(),
                )),
                code_description: None,
                source: Some("Ledgerful".to_string()),
                message: warning.clone(),
                related_information: None,
                tags: None,
                data: None,
            });
        }

        // 3. Map Symbol Issues (Complexity, Visibility)
        if let Some(symbols) = &file.symbols {
            for symbol in symbols {
                // If it's a public symbol modification, that's interesting
                if symbol.is_public {
                    diagnostics.push(Diagnostic {
                        range: Range::new(Position::new(0, 0), Position::new(0, 0)),
                        severity: Some(DiagnosticSeverity::INFORMATION),
                        code: Some(tower_lsp_server::ls_types::NumberOrString::String(
                            "public-change".to_string(),
                        )),
                        code_description: None,
                        source: Some("Ledgerful".to_string()),
                        message: format!("Public symbol modified: {}", symbol.name),
                        related_information: None,
                        tags: None,
                        data: None,
                    });
                }

                // Map complexity if available
                if let Some(cog) = symbol.cognitive_complexity.filter(|&c| c > 10) {
                    diagnostics.push(Diagnostic {
                        range: Range::new(Position::new(0, 0), Position::new(0, 0)),
                        severity: Some(DiagnosticSeverity::WARNING),
                        code: Some(tower_lsp_server::ls_types::NumberOrString::String(
                            "high-cognitive-complexity".to_string(),
                        )),
                        code_description: None,
                        source: Some("Ledgerful".to_string()),
                        message: format!(
                            "Symbol '{}' has high cognitive complexity: {}",
                            symbol.name, cog
                        ),
                        related_information: None,
                        tags: None,
                        data: None,
                    });
                }
            }
        }

        if !diagnostics.is_empty() {
            diagnostics_map.insert(file.path.clone(), diagnostics);
        }
    }

    // 4. Map Temporal Couplings
    // These could be shown in both files involved.
    for coupling in &packet.temporal_couplings {
        let message = format!(
            "Temporal coupling detected with {}: {:.2}",
            coupling.file_b.display(),
            coupling.score
        );

        let diag = Diagnostic {
            range: Range::new(Position::new(0, 0), Position::new(0, 0)),
            severity: Some(DiagnosticSeverity::HINT),
            code: Some(tower_lsp_server::ls_types::NumberOrString::String(
                "temporal-coupling".to_string(),
            )),
            code_description: None,
            source: Some("Ledgerful".to_string()),
            message,
            related_information: None,
            tags: None,
            data: None,
        };

        diagnostics_map
            .entry(coupling.file_a.clone())
            .or_default()
            .push(diag.clone());

        // Reverse message for the second file
        let mut diag_b = diag;
        diag_b.message = format!(
            "Temporal coupling detected with {}: {:.2}",
            coupling.file_a.display(),
            coupling.score
        );
        diagnostics_map
            .entry(coupling.file_b.clone())
            .or_default()
            .push(diag_b);
    }

    diagnostics_map
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::impact::packet::{ChangedFile, FileAnalysisStatus, RiskLevel, TemporalCoupling};

    #[test]
    fn test_map_impact_to_diagnostics() {
        let mut packet = ImpactPacket {
            risk_level: RiskLevel::High,
            risk_reasons: vec!["Something bad".to_string()],
            ..ImpactPacket::default()
        };

        packet.changes.push(ChangedFile {
            path: PathBuf::from("src/main.rs"),
            status: "Modified".to_string(),
            old_path: None,
            is_staged: true,
            symbols: None,
            imports: None,
            runtime_usage: None,
            analysis_status: FileAnalysisStatus::default(),
            analysis_warnings: vec!["Old API used".to_string()],
            api_routes: Vec::new(),
            data_models: Vec::new(),
            ci_gates: Vec::new(),
        });

        packet.temporal_couplings.push(TemporalCoupling {
            file_a: PathBuf::from("src/main.rs"),
            file_b: PathBuf::from("src/lib.rs"),
            score: 0.85,
        });

        let map = map_impact_to_diagnostics(&packet);

        assert!(map.contains_key(&PathBuf::from("src/main.rs")));
        assert!(map.contains_key(&PathBuf::from("src/lib.rs")));

        let main_diags = &map[&PathBuf::from("src/main.rs")];
        // Risk, Warning, Temporal Coupling = 3
        assert_eq!(main_diags.len(), 3);

        assert!(
            main_diags
                .iter()
                .any(|d| d.message.contains("Risk Level: High"))
        );
        assert!(
            main_diags
                .iter()
                .any(|d| d.message.contains("Old API used"))
        );
        assert!(main_diags.iter().any(|d| {
            d.message
                .contains("Temporal coupling detected with src/lib.rs")
        }));
    }
}
