use ledgerful::gemini::narrative::NarrativeEngine;
use ledgerful::impact::packet::{ChangedFile, Hotspot, ImpactPacket, RiskLevel, TemporalCoupling};
use std::path::PathBuf;

#[test]
fn test_narrative_golden_prompt() {
    let mut packet = ImpactPacket {
        schema_version: "v1".to_string(),
        timestamp_utc: "2024-01-01T00:00:00Z".to_string(),
        head_hash: Some("abcdef123456".to_string()),
        branch_name: Some("main".to_string()),
        risk_level: RiskLevel::High,
        risk_reasons: vec![
            "High complexity hotspot modified".to_string(),
            "Strong temporal coupling to sensitive module".to_string(),
        ],
        changes: vec![
            ChangedFile {
                path: PathBuf::from("src/core/logic.rs"),
                status: "Modified".to_string(),
                old_path: None,
                is_staged: true,

                symbols: None,
                imports: None,
                runtime_usage: None,
                analysis_status: Default::default(),
                analysis_warnings: vec![],
                api_routes: vec![],
                data_models: vec![],
                ci_gates: vec![],
            },
            ChangedFile {
                path: PathBuf::from("tests/integration.rs"),
                status: "Modified".to_string(),
                old_path: None,
                is_staged: true,

                symbols: None,
                imports: None,
                runtime_usage: None,
                analysis_status: Default::default(),
                analysis_warnings: vec![],
                api_routes: vec![],
                data_models: vec![],
                ci_gates: vec![],
            },
        ],
        temporal_couplings: vec![TemporalCoupling {
            file_a: PathBuf::from("src/core/logic.rs"),
            file_b: PathBuf::from("src/auth/session.rs"),
            score: 0.85,
        }],
        structural_couplings: vec![],
        centrality_risks: vec![],
        logging_coverage_delta: vec![],
        error_handling_delta: vec![],
        telemetry_coverage_delta: vec![],
        infrastructure_dirs: vec![],
        env_var_deps: vec![],
        test_coverage: vec![],
        runtime_usage_delta: vec![],
        hotspots: vec![Hotspot {
            path: PathBuf::from("src/core/logic.rs"),
            score: 12.5,
            display_score: 0.0,
            complexity: 45,
            frequency: 120.0,
            centrality: None,
        }],
        verification_results: vec![],
        relevant_decisions: vec![],
        observability: vec![],
        affected_contracts: vec![],
        service_map_delta: None,
        ..ImpactPacket::default()
    };

    // Ensure deterministic ordering
    packet.finalize();

    let prompt = NarrativeEngine::generate_risk_prompt(&packet);

    let expected = r#"Act as a Senior Software Architect. Provide a high-level narrative summary of the following change impact report.

## Core Analysis
- Overall Risk Level: High
- Risk Reasons:
  * High complexity hotspot modified
  * Strong temporal coupling to sensitive module

## Changes Summary
- Total files changed: 2
  * src/core/logic.rs (Modified)
  * tests/integration.rs (Modified)

## Code Hotspots (High Risk Density)
  * src/core/logic.rs: Score 12.50 (Freq: 120, Complexity: 45)

## Temporal Couplings (Logical Dependencies)
  * src/core/logic.rs <-> src/auth/session.rs (Affinity: 85%)

## Task
Explain the 'Butterfly Effect' of these changes. What is the most likely thing to break that is NOT in the changed files? What should the reviewer focus on most?"#;

    assert_eq!(prompt, expected);
}
